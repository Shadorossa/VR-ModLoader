//! HCA **encoder** (constant bitrate, v2.0 stream like VGAudio 2.2.1's, whose output the game already plays: voice
//! packs, custom BGM). Written against this crate's decoder ([`crate::hca`]): every field it writes is one the decoder
//! (a port of the reference clHCA / VGAudio readers) reads back, and the tests decode what it writes.
//!
//! Stream shape: `HCA\0` v2.0, `fmt` (delay 128), `comp` (min resolution 1, max 15, 128 bands, all discrete: no
//! intensity stereo, no high-frequency reconstruction), optional `loop`, `ciph` 0, `pad`, CRC-16. Per frame (1024
//! samples = 8 subframes of 128): MDCT (the exact transpose of the decoder's windowed IMDCT, so the lapped transform
//! reconstructs perfectly), one scale factor per band (the smallest whose gain covers the band's peak over the 8
//! subframes), and the frame's noise level / evaluation boundary chosen as the lowest (= best quality) that fits the
//! frame size; the resolutions then follow from the decoder's own rule (`calculate_resolution`, ATH type 0).

use crate::hca::{crc16, dequantizer_range, dequantizer_scaling, Transform, CODE_BITS, CODE_VALUE, MAX_BITS, MDCT_WINDOW, RESOLUTION_ZERO_FROM, SCALE_TO_RESOLUTION};
use crate::{Error, Result};

const SUB: usize = 8;
const N: usize = 128;
const FRAME: usize = SUB * N;
/// Encoder delay written in `fmt` (the first 128 samples of the stream are silence the decoder trims).
pub const DELAY: usize = 128;
const MIN_RES: u8 = 1;
const MAX_RES: u8 = 15;

/// Options of [`encode`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Options {
    /// Frame size in bytes (CBR). 0 = [`default_frame_size`].
    pub frame_size: u16,
    /// Loop `(start, end)` in samples (end exclusive), `None` = no `loop` chunk.
    pub loop_range: Option<(u64, u64)>,
}

impl Default for Options {
    fn default() -> Self {
        Options { frame_size: 0, loop_range: None }
    }
}

/// VGAudio's defaults at 48 kHz: 341 bytes per channel-ish (≈128 kbit/s mono, 256 kbit/s stereo).
pub fn default_frame_size(channels: u16) -> u16 {
    (341 * channels.max(1) as u32).min(0xFFFF) as u16
}

/// Frame size of a bitrate (bits/s) at `rate` Hz.
pub fn frame_size_for_bitrate(bitrate: u32, rate: u32) -> u16 {
    ((bitrate as u64 * FRAME as u64 / 8 / rate.max(1) as u64).clamp(64, 0xFFFF)) as u16
}

// ---------------------------------------------------------------- bit writer

struct BitWriter {
    d: Vec<u8>,
    pos: usize,
}

impl BitWriter {
    fn new(bytes: usize) -> Self {
        BitWriter { d: vec![0; bytes], pos: 0 }
    }
    fn write(&mut self, v: u32, n: u32) {
        for i in (0..n).rev() {
            if (v >> i) & 1 != 0 {
                let p = self.pos;
                if let Some(b) = self.d.get_mut(p / 8) {
                    *b |= 0x80 >> (p % 8);
                }
            }
            self.pos += 1;
        }
    }
}

// ---------------------------------------------------------------- quantization tables

/// Largest magnitude a resolution can code.
fn max_value(res: usize) -> i32 {
    match res {
        0 => 0,
        1..=7 => res as i32,
        _ => (1 << (MAX_BITS[res] - 1)) - 1,
    }
}

/// Prefix code of `value` for resolutions 1..=7: (codeword, length).
fn prefix_code(res: usize, value: i32) -> (u32, u32) {
    let bits = MAX_BITS[res] as u32;
    for c in 0..(1usize << bits) {
        if CODE_VALUE[res][c] as i32 == value {
            let len = CODE_BITS[res][c] as u32;
            return ((c as u32) >> (bits - len), len);
        }
    }
    unreachable!("value {value} not codable at resolution {res}")
}

struct Codes {
    /// [res][value + 8] -> (codeword, length), resolutions 1..=7.
    small: [[(u32, u32); 17]; 8],
}

impl Codes {
    fn new() -> Codes {
        let mut small = [[(0, 0); 17]; 8];
        for (r, row) in small.iter_mut().enumerate().skip(1) {
            for v in -(r as i32)..=(r as i32) {
                row[(v + 8) as usize] = prefix_code(r, v);
            }
        }
        Codes { small }
    }
    /// Bits of a quantized value.
    fn bits(&self, res: usize, v: i32) -> u32 {
        match res {
            0 => 0,
            1..=7 => self.small[res][(v + 8) as usize].1,
            _ => MAX_BITS[res] as u32 - (v == 0) as u32,
        }
    }
    fn write(&self, w: &mut BitWriter, res: usize, v: i32) {
        match res {
            0 => {}
            1..=7 => {
                let (c, l) = self.small[res][(v + 8) as usize];
                w.write(c, l);
            }
            _ => {
                let bits = MAX_BITS[res] as u32;
                if v == 0 {
                    w.write(0, bits - 1);
                } else {
                    let code = ((v.unsigned_abs()) << 1) | (v < 0) as u32;
                    w.write(code, bits);
                }
            }
        }
    }
}

/// The decoder's resolution of a band (`hca::calculate_resolution`, ATH 0).
fn resolution(sf: u8, band: usize, packed_noise_level: i32) -> usize {
    if sf == 0 {
        return 0;
    }
    let noise = (packed_noise_level + band as i32) >> 8;
    let pos = noise - ((sf as i32 * 5) >> 1) + 2;
    let r = if pos > RESOLUTION_ZERO_FROM { 0 } else { SCALE_TO_RESOLUTION[pos.clamp(0, SCALE_TO_RESOLUTION.len() as i32 - 1) as usize] };
    r.clamp(MIN_RES, MAX_RES) as usize
}

fn scale_factor(peak: f32) -> u8 {
    if peak <= dequantizer_scaling(0) * 0.5 {
        return 0;
    }
    (1..64).find(|&i| dequantizer_scaling(i) >= peak).unwrap_or(63) as u8
}

// ---------------------------------------------------------------- MDCT (transpose of the decoder's IMDCT)

/// Spectrum of the 256-sample block `x` (the decoder's `imdct` of it, overlap-added with its neighbours, gives the
/// samples back).
fn mdct(t: &Transform, x: &[f32; 2 * N], out: &mut [f32; N]) {
    let w = &MDCT_WINDOW;
    let mut u = [0f32; N];
    for i in 0..64 {
        u[i + 64] += w[i] * x[i];
        u[127 - i] -= w[64 + i] * x[64 + i];
        u[63 - i] -= w[127 - i] * x[128 + i];
        u[i] -= w[63 - i] * x[192 + i];
    }
    t.dct4(&u, out);
}

// ---------------------------------------------------------------- scale factor packing

fn pack_scalefactors(sf: &[u8]) -> (u32, u32) {
    // (delta_bits, total bits)
    if sf.iter().all(|&s| s == 0) {
        return (0, 3);
    }
    let mut best = (6u32, 3 + 6 * sf.len() as u32);
    for db in 1..6u32 {
        let expected = (1i32 << db) - 1;
        let half = expected >> 1;
        let mut bits = 3 + 6;
        for i in 1..sf.len() {
            let d = sf[i] as i32 - sf[i - 1] as i32 + half;
            bits += if (0..expected).contains(&d) { db } else { db + 6 };
        }
        if bits < best.1 {
            best = (db, bits);
        }
    }
    best
}

fn write_scalefactors(w: &mut BitWriter, sf: &[u8], db: u32) {
    w.write(db, 3);
    match db {
        0 => {}
        6.. => {
            for &s in sf {
                w.write(s as u32, 6);
            }
        }
        _ => {
            let expected = (1i32 << db) - 1;
            let half = expected >> 1;
            w.write(sf[0] as u32, 6);
            for i in 1..sf.len() {
                let d = sf[i] as i32 - sf[i - 1] as i32 + half;
                if (0..expected).contains(&d) {
                    w.write(d as u32, db);
                } else {
                    w.write(expected as u32, db);
                    w.write(sf[i] as u32, 6);
                }
            }
        }
    }
}

// ---------------------------------------------------------------- frame

struct ChannelFrame {
    spectra: [[f32; N]; SUB],
    sf: [u8; N],
}

fn quantize(x: f32, sf: u8, res: usize) -> i32 {
    if res == 0 {
        return 0;
    }
    let g = dequantizer_scaling(sf as usize) * dequantizer_range(res);
    let m = max_value(res);
    ((x / g).round() as i32).clamp(-m, m)
}

/// Bits of the channel payloads at `pnl` (without the 32-bit frame header), or None.
fn frame_bits(chs: &[ChannelFrame], pnl: i32, codes: &Codes) -> u32 {
    let mut bits = 0;
    for ch in chs {
        bits += pack_scalefactors(&ch.sf).1;
        for b in 0..N {
            let res = resolution(ch.sf[b], b, pnl);
            if res == 0 {
                continue;
            }
            for s in 0..SUB {
                bits += codes.bits(res, quantize(ch.spectra[s][b], ch.sf[b], res));
            }
        }
    }
    bits
}

fn write_frame(chs: &[ChannelFrame], nl: i32, eb: i32, fs: usize, codes: &Codes) -> Vec<u8> {
    let pnl = (nl << 8) - eb;
    let mut w = BitWriter::new(fs);
    w.write(0xFFFF, 16);
    w.write(nl as u32, 9);
    w.write(eb as u32, 7);
    for ch in chs {
        let (db, _) = pack_scalefactors(&ch.sf);
        write_scalefactors(&mut w, &ch.sf, db);
    }
    for s in 0..SUB {
        for ch in chs {
            for b in 0..N {
                let res = resolution(ch.sf[b], b, pnl);
                codes.write(&mut w, res, quantize(ch.spectra[s][b], ch.sf[b], res));
            }
        }
    }
    let mut d = w.d;
    let crc = crc16(&d[..fs - 2]);
    d[fs - 2..].copy_from_slice(&crc.to_be_bytes());
    d
}

/// Lowest (best) noise level, then highest evaluation boundary, that fit `budget` bits. When even the lowest quality
/// does not fit (every coded band costs at least 1 bit per coefficient), the highest bands are dropped (scale factor 0
/// = silent) until it does: a lower bandwidth for this frame.
fn pick_noise(chs: &mut [ChannelFrame], budget: u32, codes: &Codes) -> Result<(i32, i32)> {
    let mut top = N;
    while frame_bits(chs, 511 << 8, codes) > budget {
        if top == 0 {
            return Err(Error::Hca("frame size too small for this audio".into()));
        }
        top -= 1;
        for ch in chs.iter_mut() {
            ch.sf[top] = 0;
        }
    }
    let chs = &*chs;
    let fits = |nl: i32, eb: i32| frame_bits(chs, (nl << 8) - eb, codes) <= budget;
    let (mut lo, mut hi) = (0i32, 511i32); // hi fits
    while lo < hi {
        let mid = (lo + hi) / 2;
        if fits(mid, 0) {
            hi = mid;
        } else {
            lo = mid + 1;
        }
    }
    let nl = hi;
    // bands below the boundary get one noise step less (more bits): as many as still fit
    let (mut lo, mut hi) = (0i32, 127i32);
    while lo < hi {
        let mid = (lo + hi + 1) / 2;
        if fits(nl, mid) {
            lo = mid;
        } else {
            hi = mid - 1;
        }
    }
    Ok((nl, lo))
}

// ---------------------------------------------------------------- header

fn header(channels: u16, rate: u32, frames: u32, pad: u16, fs: u16, lp: Option<(u32, u32, u16, u16)>) -> Vec<u8> {
    let size: usize = 0x60;
    let mut h = Vec::with_capacity(size);
    h.extend_from_slice(b"HCA\0");
    h.extend_from_slice(&0x0200u16.to_be_bytes());
    h.extend_from_slice(&(size as u16).to_be_bytes());
    h.extend_from_slice(b"fmt\0");
    h.push(channels as u8);
    h.extend_from_slice(&rate.to_be_bytes()[1..]);
    h.extend_from_slice(&frames.to_be_bytes());
    h.extend_from_slice(&(DELAY as u16).to_be_bytes());
    h.extend_from_slice(&pad.to_be_bytes());
    h.extend_from_slice(b"comp");
    h.extend_from_slice(&fs.to_be_bytes());
    h.extend_from_slice(&[MIN_RES, MAX_RES, 1, if channels == 1 { 1 } else { 0 }, N as u8, N as u8, 0, 0, 0, 0]);
    if let Some((s, e, pre, post)) = lp {
        h.extend_from_slice(b"loop");
        h.extend_from_slice(&s.to_be_bytes());
        h.extend_from_slice(&e.to_be_bytes());
        h.extend_from_slice(&pre.to_be_bytes());
        h.extend_from_slice(&post.to_be_bytes());
    }
    h.extend_from_slice(b"ciph");
    h.extend_from_slice(&0u16.to_be_bytes());
    h.extend_from_slice(b"pad\0");
    h.resize(size - 2, 0);
    let crc = crc16(&h);
    h.extend_from_slice(&crc.to_be_bytes());
    h
}

/// Encode interleaved 16-bit PCM (`channels` 1..=8) to an HCA file.
pub fn encode(samples: &[i16], channels: u16, rate: u32, opts: Options) -> Result<Vec<u8>> {
    let ch = channels as usize;
    if ch == 0 || ch > 8 || rate == 0 || rate > 0xFF_FFFF {
        return Err(Error::Hca(format!("unsupported layout: {ch} channels, {rate} Hz")));
    }
    let n = samples.len() / ch;
    if n == 0 {
        return Err(Error::Hca("no samples".into()));
    }
    let fs = if opts.frame_size == 0 { default_frame_size(channels) } else { opts.frame_size } as usize;
    if fs < 16 {
        return Err(Error::Hca("frame size too small".into()));
    }
    let frames = (DELAY + n).div_ceil(FRAME);
    let pad = frames * FRAME - DELAY - n;
    // planar float input with the delay in front and one extra subframe of silence at the end
    let total = frames * FRAME + N;
    let mut planar = vec![vec![0f32; total]; ch];
    for (i, fr) in samples.chunks_exact(ch).enumerate() {
        for (c, &s) in fr.iter().enumerate() {
            planar[c][DELAY + i] = s as f32 / 32768.0;
        }
    }
    let t = Transform::new();
    let codes = Codes::new();
    let budget = ((fs - 2) * 8 - 32) as u32;
    let lp = match opts.loop_range {
        Some((s, e)) if s < e && e <= n as u64 => {
            let (a, b) = (DELAY as u64 + s, DELAY as u64 + e);
            let end_frame = (b - 1) / FRAME as u64;
            Some(((a / FRAME as u64) as u32, end_frame as u32, (a % FRAME as u64) as u16, ((end_frame + 1) * FRAME as u64 - b) as u16))
        }
        Some(_) => return Err(Error::Hca("bad loop range".into())),
        None => None,
    };
    let mut out = header(channels, rate, frames as u32, pad as u16, fs as u16, lp);
    let mut block = [0f32; 2 * N];
    for f in 0..frames {
        let mut chs: Vec<ChannelFrame> = Vec::with_capacity(ch);
        for p in planar.iter() {
            let mut cf = ChannelFrame { spectra: [[0.0; N]; SUB], sf: [0; N] };
            for s in 0..SUB {
                let start = f * FRAME + s * N;
                block.copy_from_slice(&p[start..start + 2 * N]);
                mdct(&t, &block, &mut cf.spectra[s]);
            }
            for b in 0..N {
                let peak = (0..SUB).map(|s| cf.spectra[s][b].abs()).fold(0f32, f32::max);
                cf.sf[b] = scale_factor(peak);
            }
            chs.push(cf);
        }
        let (nl, eb) = pick_noise(&mut chs, budget, &codes)?;
        out.extend_from_slice(&write_frame(&chs, nl, eb, fs, &codes));
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hca;

    fn tone(n: usize, ch: usize, rate: u32) -> Vec<i16> {
        let mut v = Vec::with_capacity(n * ch);
        for i in 0..n {
            let t = i as f64 / rate as f64;
            for c in 0..ch {
                let f = 440.0 * (1.0 + c as f64 * 0.5);
                let env = (i as f64 / 2000.0).min(1.0);
                v.push((12000.0 * env * (2.0 * std::f64::consts::PI * f * t).sin() + 3000.0 * (2.0 * std::f64::consts::PI * 3150.0 * t).sin()) as i16);
            }
        }
        v
    }

    fn snr_db(a: &[i16], b: &[i16]) -> f64 {
        let (mut s, mut e) = (0f64, 0f64);
        for (x, y) in a.iter().zip(b) {
            s += (*x as f64).powi(2);
            e += (*x as f64 - *y as f64).powi(2);
        }
        10.0 * (s / e.max(1e-9)).log10()
    }

    #[test]
    fn mdct_is_the_inverse_of_the_decoder() {
        // unquantized: MDCT -> decoder IMDCT (overlap-add) gives the samples back
        let t = Transform::new();
        let x: Vec<f32> = (0..N * 6).map(|i| ((i * 7919 % 101) as f32 / 101.0 - 0.5) * 0.8).collect();
        let w = &MDCT_WINDOW;
        let mut prev = [0f32; N];
        let mut out = Vec::new();
        for b in 0..5 {
            let mut blk = [0f32; 2 * N];
            blk.copy_from_slice(&x[b * N..b * N + 2 * N]);
            let mut spec = [0f32; N];
            mdct(&t, &blk, &mut spec);
            let mut c = [0f32; N];
            t.dct4(&spec, &mut c);
            let mut o = [0f32; N];
            for i in 0..64 {
                o[i] = w[i] * c[i + 64] + prev[i];
                o[i + 64] = w[i + 64] * -c[127 - i] - prev[i + 64];
                prev[i] = w[127 - i] * -c[63 - i];
                prev[i + 64] = w[63 - i] * c[i];
            }
            out.extend_from_slice(&o);
        }
        // block b's first half is output subframe b: subframes 1.. are complete
        for i in N..5 * N {
            assert!((out[i] - x[i]).abs() < 1e-4, "sample {i}: {} vs {}", out[i], x[i]);
        }
    }

    #[test]
    fn prefix_codes_round_trip() {
        let codes = Codes::new();
        for r in 1..8 {
            for v in -(r as i32)..=(r as i32) {
                let (c, l) = codes.small[r][(v + 8) as usize];
                let bits = MAX_BITS[r] as u32;
                let peek = (c << (bits - l)) as usize;
                assert_eq!(CODE_BITS[r][peek] as u32, l);
                assert_eq!(CODE_VALUE[r][peek] as i32, v);
            }
        }
    }

    #[test]
    fn mono_and_stereo_decode_back() {
        for (ch, fs, min_snr) in [(1usize, 341u16, 20.0), (2, 682, 20.0), (1, 1024, 30.0)] {
            let rate = 48000;
            let pcm = tone(30_000, ch, rate);
            let hca_bytes = encode(&pcm, ch as u16, rate, Options { frame_size: fs, loop_range: None }).unwrap();
            let h = hca::Header::parse(&hca_bytes).unwrap();
            assert!(h.crc_ok);
            assert_eq!((h.channels as usize, h.sample_rate, h.frame_size, h.version), (ch, rate, fs, 0x200));
            assert_eq!(h.sample_count(), 30_000);
            assert_eq!(hca_bytes.len(), 0x60 + h.frame_count as usize * fs as usize);
            let mut dec = hca::Decoder::new(h.clone()).unwrap();
            for f in 0..h.frame_count as usize {
                let p = 0x60 + f * fs as usize;
                dec.decode_frame(&hca_bytes[p..p + fs as usize]).unwrap();
            }
            assert_eq!((dec.bad_crc_frames, dec.overrun_frames), (0, 0), "{ch} ch");
            let back = hca::decode(&hca_bytes).unwrap();
            assert_eq!(back.samples.len(), pcm.len());
            let snr = snr_db(&pcm, &back.samples);
            assert!(snr > min_snr, "{ch} ch, frame {fs}: SNR {snr:.1} dB");
        }
    }

    #[test]
    fn silence_loops_and_errors() {
        let pcm = vec![0i16; 5000];
        let b = encode(&pcm, 1, 48000, Options::default()).unwrap();
        let d = hca::decode(&b).unwrap();
        assert!(d.samples.iter().all(|&s| s.abs() <= 1));
        let b = encode(&tone(50_000, 2, 44100), 2, 44100, Options { frame_size: 0, loop_range: Some((10_000, 45_000)) }).unwrap();
        let h = hca::Header::parse(&b).unwrap();
        assert_eq!(h.loop_samples(), Some((10_000, 45_000)));
        assert!(h.crc_ok);
        assert!(encode(&[], 1, 48000, Options::default()).is_err());
        assert!(encode(&pcm, 1, 48000, Options { frame_size: 0, loop_range: Some((10, 999_999)) }).is_err());
    }

    #[test]
    fn full_scale_noise_fits_small_frames() {
        let mut x: u32 = 1;
        let pcm: Vec<i16> = (0..20_000)
            .map(|_| {
                x = x.wrapping_mul(1103515245).wrapping_add(12345);
                (x >> 16) as i16
            })
            .collect();
        let b = encode(&pcm, 1, 48000, Options { frame_size: 128, loop_range: None }).unwrap();
        let back = hca::decode(&b).unwrap();
        assert_eq!(back.samples.len(), pcm.len());
    }
}
