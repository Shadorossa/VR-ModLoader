//! Source audio of a mod → HCA in the format a bank slot needs (boot-time conversion; docs audio-engine.md §0).
//!
//! * `.hca` files are used **as they are** (header + frame CRCs checked; `volume` / `normalize` refused for them).
//! * anything else (wav, flac, mp3, ogg vorbis) is decoded with Symphonia, mixed to the slot's channel count,
//!   resampled to its rate (windowed sinc), gain / normalized, then encoded with `cri::hca_enc` at the slot's frame
//!   size (loop points in samples of the output rate).

use cri::hca;

/// Decoded audio: interleaved f32 in [-1, 1].
#[derive(Debug, Clone, PartialEq)]
pub struct Audio {
    pub channels: u16,
    pub rate: u32,
    pub samples: Vec<f32>,
}

impl Audio {
    pub fn frames(&self) -> usize {
        self.samples.len() / self.channels.max(1) as usize
    }
}

/// Normalization of a converted source.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Normalize {
    None,
    /// Peak to -1 dBFS.
    Peak,
}

impl Normalize {
    pub fn parse(s: &str) -> Result<Normalize, String> {
        match s {
            "" | "none" => Ok(Normalize::None),
            "peak" => Ok(Normalize::Peak),
            other => Err(format!("normalize = \"{other}\": none | peak (loudnorm only with audio_mod_build.py)")),
        }
    }
}

/// Target of a conversion.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Target {
    pub rate: u32,
    pub channels: u16,
    pub frame_size: u16,
}

/// Options of one source.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Treat {
    pub gain_db: f32,
    pub normalize: Normalize,
    /// Loop (start, end) in seconds (end ≤ 0 = to the end).
    pub loop_secs: Option<(f64, f64)>,
}

impl Default for Treat {
    fn default() -> Self {
        Treat { gain_db: 0.0, normalize: Normalize::None, loop_secs: None }
    }
}

/// Is this a CRI HCA file (by extension or magic)?
pub fn is_hca(path: &std::path::Path, bytes: &[u8]) -> bool {
    path.extension().is_some_and(|e| e.eq_ignore_ascii_case("hca")) || hca::Header::is_hca(bytes)
}

/// Check a pre-encoded HCA: header CRC, every frame's sync + CRC (a bad file would play noise or stop the stream).
pub fn check_hca(bytes: &[u8]) -> Result<hca::Header, String> {
    let h = hca::Header::parse(bytes).map_err(|e| e.to_string())?;
    if !h.crc_ok {
        return Err("HCA header CRC does not match".into());
    }
    if h.cipher_type != 0 {
        return Err("encrypted HCA (ciph != 0)".into());
    }
    let fs = h.frame_size as usize;
    let start = h.header_size as usize;
    let need = start + h.frame_count as usize * fs;
    if bytes.len() < need {
        return Err(format!("HCA truncated: {} bytes, the header needs {need}", bytes.len()));
    }
    for f in 0..h.frame_count as usize {
        let fr = &bytes[start + f * fs..start + (f + 1) * fs];
        if fr[0] != 0xFF || fr[1] != 0xFF || hca::crc16(&fr[..fs - 2]) != u16::from_be_bytes([fr[fs - 2], fr[fs - 1]]) {
            return Err(format!("HCA frame {f}: bad sync / CRC"));
        }
    }
    Ok(h)
}

/// Decode any file Symphonia reads (wav, flac, mp3, ogg vorbis).
pub fn decode(bytes: Vec<u8>, ext_hint: Option<&str>) -> Result<Audio, String> {
    use symphonia::core::audio::SampleBuffer;
    use symphonia::core::codecs::DecoderOptions;
    use symphonia::core::errors::Error as SErr;
    use symphonia::core::formats::FormatOptions;
    use symphonia::core::io::MediaSourceStream;
    use symphonia::core::meta::MetadataOptions;
    use symphonia::core::probe::Hint;
    let mss = MediaSourceStream::new(Box::new(std::io::Cursor::new(bytes)), Default::default());
    let mut hint = Hint::new();
    if let Some(e) = ext_hint {
        hint.with_extension(e);
    }
    let probed = symphonia::default::get_probe()
        .format(&hint, mss, &FormatOptions::default(), &MetadataOptions::default())
        .map_err(|e| format!("not an audio file Symphonia reads ({e})"))?;
    let mut format = probed.format;
    let track = format.default_track().ok_or("no audio track")?.clone();
    let mut dec = symphonia::default::get_codecs().make(&track.codec_params, &DecoderOptions::default()).map_err(|e| format!("codec: {e}"))?;
    let mut channels = track.codec_params.channels.map(|c| c.count() as u16).unwrap_or(0);
    let mut rate = track.codec_params.sample_rate.unwrap_or(0);
    let mut samples = Vec::new();
    loop {
        let packet = match format.next_packet() {
            Ok(p) => p,
            Err(SErr::IoError(e)) if e.kind() == std::io::ErrorKind::UnexpectedEof => break,
            Err(SErr::ResetRequired) => break,
            Err(e) => return Err(format!("read: {e}")),
        };
        if packet.track_id() != track.id {
            continue;
        }
        match dec.decode(&packet) {
            Ok(buf) => {
                let spec = *buf.spec();
                channels = spec.channels.count() as u16;
                rate = spec.rate;
                let mut sb = SampleBuffer::<f32>::new(buf.capacity() as u64, spec);
                sb.copy_interleaved_ref(buf);
                samples.extend_from_slice(sb.samples());
            }
            Err(SErr::DecodeError(_)) => continue,
            Err(e) => return Err(format!("decode: {e}")),
        }
    }
    if channels == 0 || rate == 0 || samples.is_empty() {
        return Err("empty audio".into());
    }
    Ok(Audio { channels, rate, samples })
}

/// Mix to `ch` channels (mono ↔ stereo: average / duplicate; otherwise channel i takes source i mod n).
pub fn remix(a: &Audio, ch: u16) -> Audio {
    if a.channels == ch {
        return a.clone();
    }
    let (n, m) = (a.channels as usize, ch as usize);
    let frames = a.frames();
    let mut out = Vec::with_capacity(frames * m);
    for f in 0..frames {
        let fr = &a.samples[f * n..(f + 1) * n];
        for c in 0..m {
            out.push(if m == 1 { fr.iter().sum::<f32>() / n as f32 } else { fr[c % n] });
        }
    }
    Audio { channels: ch, rate: a.rate, samples: out }
}

/// Resample to `rate` (windowed-sinc, 32 taps each side, Blackman window, low-passed at the lower Nyquist).
pub fn resample(a: &Audio, rate: u32) -> Audio {
    if a.rate == rate || a.frames() == 0 {
        return Audio { channels: a.channels, rate, samples: a.samples.clone() };
    }
    let ch = a.channels as usize;
    let n_in = a.frames();
    let ratio = rate as f64 / a.rate as f64;
    let n_out = ((n_in as f64) * ratio).round().max(1.0) as usize;
    let cutoff = ratio.min(1.0);
    const TAPS: i64 = 32;
    let mut out = vec![0f32; n_out * ch];
    for o in 0..n_out {
        let t = o as f64 / ratio;
        let center = t.floor() as i64;
        let mut acc = vec![0f64; ch];
        let mut wsum = 0f64;
        for k in (center - TAPS + 1)..=(center + TAPS) {
            if k < 0 || k >= n_in as i64 {
                continue;
            }
            let x = t - k as f64;
            let sinc = if x.abs() < 1e-9 { 1.0 } else { (std::f64::consts::PI * x * cutoff).sin() / (std::f64::consts::PI * x * cutoff) };
            let wx = (x / TAPS as f64).clamp(-1.0, 1.0);
            let win = 0.42 + 0.5 * (std::f64::consts::PI * wx).cos() + 0.08 * (2.0 * std::f64::consts::PI * wx).cos();
            let w = sinc * win;
            wsum += w;
            for c in 0..ch {
                acc[c] += w * a.samples[k as usize * ch + c] as f64;
            }
        }
        for c in 0..ch {
            out[o * ch + c] = if wsum.abs() > 1e-9 { (acc[c] / wsum) as f32 } else { 0.0 };
        }
    }
    Audio { channels: a.channels, rate, samples: out }
}

/// Apply gain / normalization, return 16-bit PCM.
pub fn to_pcm16(a: &Audio, t: &Treat) -> Vec<i16> {
    let mut gain = 10f32.powf(t.gain_db / 20.0);
    if t.normalize == Normalize::Peak {
        let peak = a.samples.iter().fold(0f32, |m, s| m.max(s.abs()));
        if peak > 1e-6 {
            gain *= 10f32.powf(-1.0 / 20.0) / peak;
        }
    }
    a.samples.iter().map(|s| (s * gain * 32767.0).round().clamp(-32768.0, 32767.0) as i16).collect()
}

/// A source file → HCA for `target` (a `.hca` is only checked and returned).
pub fn to_hca(path: &std::path::Path, target: &Target, treat: &Treat) -> Result<Vec<u8>, String> {
    let bytes = std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
    if is_hca(path, &bytes) {
        if treat.gain_db != 0.0 || treat.normalize != Normalize::None || treat.loop_secs.is_some() {
            return Err("a .hca is used as it is: volume / normalize / loop cannot apply (give a wav / ogg / flac)".into());
        }
        check_hca(&bytes)?;
        return Ok(bytes);
    }
    let ext = path.extension().and_then(|e| e.to_str()).map(|e| e.to_ascii_lowercase());
    let a = decode(bytes, ext.as_deref())?;
    let a = resample(&remix(&a, target.channels), target.rate);
    let pcm = to_pcm16(&a, treat);
    let frames = pcm.len() / target.channels.max(1) as usize;
    let loop_range = treat.loop_secs.map(|(s, e)| {
        let s = ((s * target.rate as f64).round().max(0.0) as u64).min(frames as u64 - 1);
        let e = if e <= 0.0 { frames as u64 } else { ((e * target.rate as f64).round() as u64).clamp(s + 1, frames as u64) };
        (s, e)
    });
    cri::hca_enc::encode(&pcm, target.channels, target.rate, cri::hca_enc::Options { frame_size: target.frame_size, loop_range }).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wav(ch: u16, rate: u32, secs: f32, f: f32) -> Vec<u8> {
        let n = (rate as f32 * secs) as usize;
        let pcm: Vec<i16> = (0..n * ch as usize).map(|i| (8000.0 * (2.0 * std::f32::consts::PI * f * (i / ch as usize) as f32 / rate as f32).sin()) as i16).collect();
        cri::wav::write(&cri::hca::Pcm { channels: ch, sample_rate: rate, samples: pcm, loop_range: None })
    }

    #[test]
    fn wav_to_hca_at_the_slot_format() {
        let dir = std::env::temp_dir().join(format!("evt-ae-src-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("t.wav");
        std::fs::write(&p, wav(1, 44100, 1.0, 440.0)).unwrap();
        let t = Target { rate: 48000, channels: 2, frame_size: 682 };
        let h = to_hca(&p, &t, &Treat { gain_db: -3.0, normalize: Normalize::None, loop_secs: Some((0.1, 0.0)) }).unwrap();
        let hd = check_hca(&h).unwrap();
        assert_eq!((hd.channels, hd.sample_rate, hd.frame_size), (2, 48000, 682));
        assert_eq!(hd.sample_count(), 48000);
        assert_eq!(hd.loop_samples(), Some((4800, 48000)));
        // a .hca goes through untouched, but not with volume
        let hp = dir.join("t.hca");
        std::fs::write(&hp, &h).unwrap();
        assert_eq!(to_hca(&hp, &t, &Treat::default()).unwrap(), h);
        assert!(to_hca(&hp, &t, &Treat { gain_db: 2.0, ..Default::default() }).is_err());
        let mut bad = h.clone();
        let n = bad.len();
        bad[n - 5] ^= 1;
        assert!(check_hca(&bad).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn resample_keeps_a_tone() {
        let a = Audio { channels: 1, rate: 24000, samples: (0..24000).map(|i| (2.0 * std::f32::consts::PI * 500.0 * i as f32 / 24000.0).sin() * 0.5).collect() };
        let b = resample(&a, 48000);
        assert_eq!(b.frames(), 48000);
        let err: f32 = (1000..47000).map(|i| (b.samples[i] - (2.0 * std::f32::consts::PI * 500.0 * i as f32 / 48000.0).sin() * 0.5).abs()).fold(0.0, f32::max);
        assert!(err < 0.01, "max error {err}");
        let m = remix(&Audio { channels: 2, rate: 1, samples: vec![0.2, 0.4, -1.0, 1.0] }, 1);
        assert_eq!(m.samples, vec![0.3f32 as f32, 0.0]);
    }
}
