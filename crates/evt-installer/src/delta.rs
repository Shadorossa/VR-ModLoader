//! Binary deltas (own format, pure Rust, no dependency): a mod file that replaces a retail file ships as
//! "copy this range of the player's retail file / insert these bytes" instead of whole.
//!
//! Format (`.evtd`):
//! ```text
//! "EVTDELTA" u32 version=1
//! u64 source length, [20] source SHA-1     the retail file it applies to (checked before applying)
//! u64 result length, [20] result SHA-1     the mod file (checked after applying)
//! ops: 0x01 COPY varint(source offset) varint(len) | 0x02 ADD varint(len) bytes | 0x00 END
//! ```
//! Diff: source blocks (size 16..64 by file size) indexed by a polynomial hash; the target is scanned with the
//! same hash rolled byte by byte; a hit is verified, extended forwards and backwards, and emitted as COPY. After a
//! COPY the "same place" of the source (in-place edits, the usual case for cfg.bin / awb cue edits) is tried first.

use std::collections::HashMap;
use std::hash::{BuildHasherDefault, Hasher};
use std::io::Write;

use sha1::{Digest, Sha1};

const MAGIC: &[u8; 8] = b"EVTDELTA";
const VERSION: u32 = 1;
const OP_END: u8 = 0;
const OP_COPY: u8 = 1;
const OP_ADD: u8 = 2;
const P: u64 = 0x100_0000_01B3; // FNV prime: odd, good spread for the rolling polynomial
pub const HEADER_LEN: usize = 8 + 4 + 8 + 20 + 8 + 20;

pub fn sha1(b: &[u8]) -> [u8; 20] {
    Sha1::digest(b).into()
}

pub fn hex(h: &[u8]) -> String {
    h.iter().map(|b| format!("{b:02x}")).collect()
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Header {
    pub src_len: u64,
    pub src_sha1: [u8; 20],
    pub dst_len: u64,
    pub dst_sha1: [u8; 20],
}

pub fn header(delta: &[u8]) -> Result<Header, String> {
    if delta.len() < HEADER_LEN || &delta[..8] != MAGIC {
        return Err("no es un delta EVTDELTA".into());
    }
    let v = u32::from_le_bytes(delta[8..12].try_into().unwrap());
    if v != VERSION {
        return Err(format!("versión de delta {v} no soportada"));
    }
    let u64at = |o: usize| u64::from_le_bytes(delta[o..o + 8].try_into().unwrap());
    let mut src_sha1 = [0u8; 20];
    src_sha1.copy_from_slice(&delta[20..40]);
    let mut dst_sha1 = [0u8; 20];
    dst_sha1.copy_from_slice(&delta[48..68]);
    Ok(Header { src_len: u64at(12), src_sha1, dst_len: u64at(40), dst_sha1 })
}

fn put_varint(out: &mut Vec<u8>, mut v: u64) {
    loop {
        let b = (v & 0x7F) as u8;
        v >>= 7;
        if v == 0 {
            out.push(b);
            return;
        }
        out.push(b | 0x80);
    }
}

fn get_varint(b: &[u8], pos: &mut usize) -> Result<u64, String> {
    let mut v = 0u64;
    let mut shift = 0;
    loop {
        let byte = *b.get(*pos).ok_or("delta truncado")?;
        *pos += 1;
        v |= ((byte & 0x7F) as u64) << shift;
        if byte & 0x80 == 0 {
            return Ok(v);
        }
        shift += 7;
        if shift > 63 {
            return Err("delta dañado (varint)".into());
        }
    }
}

/// The keys are already hashes: mix them once instead of SipHash.
#[derive(Default)]
struct MixHasher(u64);
impl Hasher for MixHasher {
    fn finish(&self) -> u64 {
        let mut x = self.0;
        x ^= x >> 33;
        x = x.wrapping_mul(0xff51_afd7_ed55_8ccd);
        x ^= x >> 33;
        x
    }
    fn write(&mut self, _: &[u8]) {
        unreachable!("only u64 keys")
    }
    fn write_u64(&mut self, v: u64) {
        self.0 = v;
    }
}

fn block_size(n: usize) -> usize {
    match n {
        0..=4_000_000 => 16,
        4_000_001..=64_000_000 => 32,
        _ => 64,
    }
}

fn poly(b: &[u8]) -> u64 {
    b.iter().fold(0u64, |h, &x| h.wrapping_mul(P).wrapping_add(x as u64 + 1))
}

struct Emitter {
    out: Vec<u8>,
}

impl Emitter {
    fn add(&mut self, bytes: &[u8]) {
        if !bytes.is_empty() {
            self.out.push(OP_ADD);
            put_varint(&mut self.out, bytes.len() as u64);
            self.out.extend_from_slice(bytes);
        }
    }
    fn copy(&mut self, off: usize, len: usize) {
        self.out.push(OP_COPY);
        put_varint(&mut self.out, off as u64);
        put_varint(&mut self.out, len as u64);
    }
}

/// Delta that turns `src` into `dst`.
pub fn diff(src: &[u8], dst: &[u8]) -> Vec<u8> {
    let mut e = Emitter { out: Vec::with_capacity(HEADER_LEN + 64) };
    e.out.extend_from_slice(MAGIC);
    e.out.extend_from_slice(&VERSION.to_le_bytes());
    e.out.extend_from_slice(&(src.len() as u64).to_le_bytes());
    e.out.extend_from_slice(&sha1(src));
    e.out.extend_from_slice(&(dst.len() as u64).to_le_bytes());
    e.out.extend_from_slice(&sha1(dst));

    let b = block_size(src.len());
    let mut index: HashMap<u64, u32, BuildHasherDefault<MixHasher>> = HashMap::default();
    if src.len() >= b {
        index.reserve(src.len() / b);
        let mut o = 0;
        while o + b <= src.len() {
            index.entry(poly(&src[o..o + b])).or_insert(o as u32);
            o += b;
        }
    }
    let pow = (1..b).fold(1u64, |a, _| a.wrapping_mul(P)); // P^(b-1)

    let n = dst.len();
    let mut lit = 0usize; // start of the pending literal run
    let mut i = 0usize;
    let mut next_src: Option<usize> = None; // source position right after the last COPY, shifted with i
    let mut last_i = 0usize;
    let mut h: Option<u64> = None;
    while i + b <= n {
        // 1. Same place in the source as the previous copy (in-place edits).
        let mut hit: Option<usize> = None;
        if let Some(ns) = next_src {
            let s = ns + (i - last_i);
            if s + b <= src.len() && src[s..s + b] == dst[i..i + b] {
                hit = Some(s);
            }
        }
        // 2. Hash lookup.
        if hit.is_none() && !index.is_empty() {
            let hv = match h {
                Some(v) => v,
                None => poly(&dst[i..i + b]),
            };
            h = Some(hv);
            if let Some(&s) = index.get(&hv) {
                let s = s as usize;
                if src[s..s + b] == dst[i..i + b] {
                    hit = Some(s);
                }
            }
        }
        match hit {
            Some(mut s) => {
                let mut t = i;
                // extend backwards into the pending literal
                while t > lit && s > 0 && src[s - 1] == dst[t - 1] {
                    s -= 1;
                    t -= 1;
                }
                let mut len = i - t + b;
                while t + len < n && s + len < src.len() && src[s + len] == dst[t + len] {
                    len += 1;
                }
                e.add(&dst[lit..t]);
                e.copy(s, len);
                i = t + len;
                lit = i;
                next_src = Some(s + len);
                last_i = i;
                h = None;
            }
            None => {
                // roll one byte
                if let Some(hv) = h {
                    if i + b < n {
                        let out = dst[i] as u64 + 1;
                        let inn = dst[i + b] as u64 + 1;
                        h = Some(hv.wrapping_sub(out.wrapping_mul(pow)).wrapping_mul(P).wrapping_add(inn));
                    } else {
                        h = None;
                    }
                }
                i += 1;
            }
        }
    }
    e.add(&dst[lit..]);
    e.out.push(OP_END);
    e.out
}

/// Apply `delta` to `src`, writing the result into `out`. Checks the source (length + SHA-1) first and the
/// result (length + SHA-1) at the end; on a source mismatch nothing is written.
pub fn apply(src: &[u8], delta: &[u8], out: &mut dyn Write) -> Result<u64, ApplyError> {
    let h = header(delta).map_err(ApplyError::Corrupt)?;
    if src.len() as u64 != h.src_len || sha1(src) != h.src_sha1 {
        return Err(ApplyError::SourceMismatch);
    }
    let mut hasher = Sha1::new();
    let mut written = 0u64;
    let mut pos = HEADER_LEN;
    let io = |e: std::io::Error| ApplyError::Io(e.to_string());
    loop {
        let op = *delta.get(pos).ok_or(ApplyError::Corrupt("delta truncado".into()))?;
        pos += 1;
        match op {
            OP_END => break,
            OP_COPY => {
                let off = get_varint(delta, &mut pos).map_err(ApplyError::Corrupt)? as usize;
                let len = get_varint(delta, &mut pos).map_err(ApplyError::Corrupt)? as usize;
                let chunk = src.get(off..off.checked_add(len).unwrap_or(usize::MAX)).ok_or(ApplyError::Corrupt("COPY fuera del origen".into()))?;
                hasher.update(chunk);
                out.write_all(chunk).map_err(io)?;
                written += len as u64;
            }
            OP_ADD => {
                let len = get_varint(delta, &mut pos).map_err(ApplyError::Corrupt)? as usize;
                let chunk = delta.get(pos..pos.checked_add(len).unwrap_or(usize::MAX)).ok_or(ApplyError::Corrupt("ADD fuera del delta".into()))?;
                pos += len;
                hasher.update(chunk);
                out.write_all(chunk).map_err(io)?;
                written += len as u64;
            }
            x => return Err(ApplyError::Corrupt(format!("operación desconocida {x}"))),
        }
    }
    let got: [u8; 20] = hasher.finalize().into();
    if written != h.dst_len || got != h.dst_sha1 {
        return Err(ApplyError::ResultMismatch);
    }
    Ok(written)
}

#[derive(Debug, PartialEq, Eq)]
pub enum ApplyError {
    /// The player's file is not the retail file the delta was made from.
    SourceMismatch,
    /// The result is not the expected file (a damaged delta).
    ResultMismatch,
    Corrupt(String),
    Io(String),
}

impl std::fmt::Display for ApplyError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ApplyError::SourceMismatch => write!(f, "el archivo original del juego no es el de la v7.1.2"),
            ApplyError::ResultMismatch => write!(f, "el resultado no coincide (delta dañado)"),
            ApplyError::Corrupt(s) => write!(f, "delta dañado: {s}"),
            ApplyError::Io(s) => write!(f, "{s}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn roundtrip(src: &[u8], dst: &[u8]) -> usize {
        let d = diff(src, dst);
        let mut out = Vec::new();
        apply(src, &d, &mut out).unwrap();
        assert_eq!(out, dst);
        d.len()
    }

    fn noise(n: usize, seed: u64) -> Vec<u8> {
        let mut x = seed | 1;
        (0..n)
            .map(|_| {
                x ^= x << 13;
                x ^= x >> 7;
                x ^= x << 17;
                x as u8
            })
            .collect()
    }

    #[test]
    fn edits() {
        let src = noise(300_000, 7);
        assert!(roundtrip(&src, &src) < HEADER_LEN + 32);
        // in-place change
        let mut a = src.clone();
        a[1000..1100].copy_from_slice(&noise(100, 9));
        assert!(roundtrip(&src, &a) < HEADER_LEN + 200);
        // insertion + deletion (sizes shift)
        let mut b = src.clone();
        b.splice(5000..5000, noise(777, 3));
        b.drain(200_000..200_500);
        assert!(roundtrip(&src, &b) < HEADER_LEN + 900);
        // unrelated
        let c = noise(50_000, 11);
        roundtrip(&src, &c);
        // empty / tiny
        roundtrip(&[], b"abc");
        roundtrip(b"abc", &[]);
        roundtrip(b"abcdefgh", b"abcdefgh");
    }

    #[test]
    fn wrong_source_refused() {
        let src = noise(10_000, 1);
        let mut dst = src.clone();
        dst[5] ^= 1;
        let d = diff(&src, &dst);
        let mut other = src.clone();
        other[9000] ^= 1;
        assert_eq!(apply(&other, &d, &mut Vec::new()), Err(ApplyError::SourceMismatch));
    }
}
