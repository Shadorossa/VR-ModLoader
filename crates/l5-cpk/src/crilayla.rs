//! CRILAYLA compression (CRI's LZ variant used inside CPKs).
//!
//! Spec: `docs/formats/cpk.md` §3.
//!
//! ```text
//! 0x00  "CRILAYLA"
//! 0x08  u32 LE  uncompressed size EXCLUDING the 0x100-byte prefix
//! 0x0C  u32 LE  compressed payload size (csize)
//! 0x10  payload[csize]      bitstream, consumed BACKWARDS from its last byte, bits MSB-first
//! 0x10+csize  prefix[0x100] first 0x100 bytes of the output, stored raw
//! ```
//!
//! Output is filled from the end towards 0x100. Each step reads a flag bit: `0` → 8-bit literal; `1` →
//! back-reference with a 13-bit distance (`src = dst + d + 3`, copying from already-written higher addresses) and
//! a length `3 + ext` where `ext` uses 2/3/5-bit fields, then 8-bit fields, each continuing while all ones.
//!
//! The compressor emits exactly this format (greedy LZ with hash chains). It round-trips through the
//! decompressor; whether the game accepts self-compressed files is **untested** (Viola never compresses and
//! retail audio CPKs are stored raw).

use crate::error::{Error, Result};

/// `CRILAYLA` magic.
pub const MAGIC: &[u8; 8] = b"CRILAYLA";
/// Size of the raw prefix stored after the payload.
pub const PREFIX_SIZE: usize = 0x100;
const HEADER_SIZE: usize = 0x10;
const MAX_DISTANCE: usize = (1 << 13) - 1;
const MIN_MATCH: usize = 3;

/// True if `data` starts with the `CRILAYLA` magic.
pub fn is_crilayla(data: &[u8]) -> bool {
    data.starts_with(MAGIC)
}

/// Decompressed size announced by a CRILAYLA header (`usize + 0x100`), if the header is present.
pub fn decompressed_size(data: &[u8]) -> Option<u64> {
    if !is_crilayla(data) || data.len() < HEADER_SIZE {
        return None;
    }
    let u = u32::from_le_bytes(data[8..12].try_into().ok()?);
    Some(u as u64 + PREFIX_SIZE as u64)
}

struct BitReader<'a> {
    payload: &'a [u8],
    pos: usize,
    pool: u32,
    left: u32,
}

impl BitReader<'_> {
    #[inline]
    fn bits(&mut self, n: u32) -> Result<u32> {
        while self.left < n {
            if self.pos == 0 {
                return Err(Error::Crilayla("bitstream exhausted".into()));
            }
            self.pos -= 1;
            self.pool = (self.pool << 8) | self.payload[self.pos] as u32;
            self.left += 8;
        }
        self.left -= n;
        Ok((self.pool >> self.left) & ((1u32 << n) - 1))
    }
}

/// Decompress a CRILAYLA blob.
pub fn decompress(src: &[u8]) -> Result<Vec<u8>> {
    if !is_crilayla(src) {
        return Err(Error::BadMagic { what: "CRILAYLA", expected: "CRILAYLA", found: crate::error::show_magic(src) });
    }
    if src.len() < HEADER_SIZE {
        return Err(Error::Truncated { what: "CRILAYLA header", offset: 0, needed: 16, available: src.len() as u64 });
    }
    let usize_ = u32::from_le_bytes([src[8], src[9], src[10], src[11]]) as usize;
    let csize = u32::from_le_bytes([src[12], src[13], src[14], src[15]]) as usize;
    crate::error::check_bounds("CRILAYLA payload", HEADER_SIZE as u64, (csize + PREFIX_SIZE) as u64, src.len() as u64)?;
    // Each payload byte can expand to at most ~255 output bytes; refuse absurd sizes instead of allocating them.
    if usize_ as u64 > csize as u64 * 256 + 0x1000 {
        return Err(Error::Crilayla(format!("implausible size {usize_} for a {csize}-byte payload")));
    }
    let payload = &src[HEADER_SIZE..HEADER_SIZE + csize];
    let prefix = &src[HEADER_SIZE + csize..HEADER_SIZE + csize + PREFIX_SIZE];
    let n = usize_ + PREFIX_SIZE;
    let mut out = vec![0u8; n];
    out[..PREFIX_SIZE].copy_from_slice(prefix);

    let mut br = BitReader { payload, pos: payload.len(), pool: 0, left: 0 };
    // `w` = number of the next byte to write + 1 (bytes [w, n) are filled).
    let mut w = n;
    while w > PREFIX_SIZE {
        if br.bits(1)? == 1 {
            let d = br.bits(13)? as usize;
            let mut len = MIN_MATCH;
            let mut done = false;
            for nb in [2u32, 3, 5] {
                let v = br.bits(nb)? as usize;
                len += v;
                if v != (1 << nb) - 1 {
                    done = true;
                    break;
                }
            }
            if !done {
                loop {
                    let v = br.bits(8)? as usize;
                    len += v;
                    if v != 0xFF {
                        break;
                    }
                }
            }
            let mut s = w + d + 2; // == (w-1) + d + 3
            if s >= n {
                return Err(Error::Crilayla(format!("back-reference beyond the end (dst {}, dist {d})", w - 1)));
            }
            for _ in 0..len {
                if w <= PREFIX_SIZE {
                    break;
                }
                out[w - 1] = out[s];
                w -= 1;
                s -= 1;
            }
        } else {
            out[w - 1] = br.bits(8)? as u8;
            w -= 1;
        }
    }
    Ok(out)
}

struct BitWriter {
    bytes: Vec<u8>,
    acc: u32,
    nbits: u32,
}

impl BitWriter {
    #[inline]
    fn put(&mut self, v: u32, n: u32) {
        for i in (0..n).rev() {
            self.acc = (self.acc << 1) | ((v >> i) & 1);
            self.nbits += 1;
            if self.nbits == 8 {
                self.bytes.push(self.acc as u8);
                self.acc = 0;
                self.nbits = 0;
            }
        }
    }
    fn finish(mut self) -> Vec<u8> {
        if self.nbits > 0 {
            self.bytes.push((self.acc << (8 - self.nbits)) as u8);
        }
        self.bytes
    }
}

#[inline]
fn hash3(d: &[u8], p: usize) -> usize {
    let v = (d[p] as u32) | (d[p - 1] as u32) << 8 | (d[p - 2] as u32) << 16;
    (v.wrapping_mul(0x9E37_79B1) >> 17) as usize
}

/// Compress `data` into a CRILAYLA blob.
///
/// Fails if `data` is shorter than 0x100 bytes (the format cannot express that). The result may be larger than
/// the input for incompressible data; callers should then store the file raw.
pub fn compress(data: &[u8]) -> Result<Vec<u8>> {
    let n = data.len();
    if n < PREFIX_SIZE {
        return Err(Error::Crilayla(format!("input of {n} bytes is shorter than the 0x100-byte prefix")));
    }
    if n - PREFIX_SIZE > u32::MAX as usize {
        return Err(Error::OutOfRange("input larger than 4 GiB".into()));
    }
    const NONE: u32 = u32::MAX;
    const CHAIN_LIMIT: usize = 128;
    let mut head = vec![NONE; 1 << 15];
    let mut prev = vec![NONE; n];
    let mut bw = BitWriter { bytes: Vec::with_capacity(n / 2), acc: 0, nbits: 0 };

    // Positions > `next_insert` are in the hash chains.
    let mut next_insert = n; // exclusive upper bound of not-yet-inserted positions
    let mut w = n - 1; // python-style index of the byte to encode
    let mut remaining = n - PREFIX_SIZE; // bytes left to encode (w down to PREFIX_SIZE)
    while remaining > 0 {
        // Insert every position >= w + 3 not yet in the chains.
        while next_insert > w + MIN_MATCH {
            let p = next_insert - 1;
            let h = hash3(data, p);
            prev[p] = head[h];
            head[h] = p as u32;
            next_insert -= 1;
        }
        let max_len = w - PREFIX_SIZE + 1;
        let (mut best_len, mut best_off) = (0usize, 0usize);
        if max_len >= MIN_MATCH && w >= 2 {
            let mut cand = head[hash3(data, w)];
            let mut steps = 0;
            while cand != NONE && steps < CHAIN_LIMIT {
                let off = cand as usize;
                if off - w - MIN_MATCH > MAX_DISTANCE {
                    break;
                }
                // Compare downwards.
                let mut l = 0;
                while l < max_len && data[off - l] == data[w - l] {
                    l += 1;
                }
                if l > best_len {
                    best_len = l;
                    best_off = off;
                    if l == max_len {
                        break;
                    }
                }
                cand = prev[off];
                steps += 1;
            }
        }
        if best_len >= MIN_MATCH {
            bw.put(1, 1);
            bw.put((best_off - w - MIN_MATCH) as u32, 13);
            let mut v = best_len - MIN_MATCH;
            let mut open = true;
            for nb in [2u32, 3, 5] {
                let all = (1usize << nb) - 1;
                if v < all {
                    bw.put(v as u32, nb);
                    open = false;
                    break;
                }
                bw.put(all as u32, nb);
                v -= all;
            }
            if open {
                loop {
                    if v < 0xFF {
                        bw.put(v as u32, 8);
                        break;
                    }
                    bw.put(0xFF, 8);
                    v -= 0xFF;
                }
            }
            remaining -= best_len;
            w -= best_len;
        } else {
            bw.put(0, 1);
            bw.put(data[w] as u32, 8);
            remaining -= 1;
            w -= 1;
        }
    }
    let mut payload = bw.finish();
    payload.reverse();

    let mut out = Vec::with_capacity(HEADER_SIZE + payload.len() + PREFIX_SIZE);
    out.extend_from_slice(MAGIC);
    out.extend_from_slice(&((n - PREFIX_SIZE) as u32).to_le_bytes());
    out.extend_from_slice(&u32::try_from(payload.len()).map_err(|_| Error::OutOfRange("payload > 4 GiB".into()))?.to_le_bytes());
    out.extend_from_slice(&payload);
    out.extend_from_slice(&data[..PREFIX_SIZE]);
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lcg(n: usize, seed: u32) -> Vec<u8> {
        let mut s = seed;
        (0..n)
            .map(|_| {
                s = s.wrapping_mul(1_103_515_245).wrapping_add(12345);
                (s >> 16) as u8
            })
            .collect()
    }

    #[test]
    fn roundtrip_various() {
        let mut cases: Vec<Vec<u8>> = vec![
            vec![0u8; 0x100],
            vec![0u8; 0x101],
            vec![7u8; 100_000],
            lcg(5000, 1),
            (0..70_000u32).map(|i| (i % 251) as u8).collect(),
            b"hello world, hello world, hello CRILAYLA! ".repeat(500),
        ];
        let mut mixed = lcg(20_000, 9);
        for i in (0..mixed.len()).step_by(7) {
            mixed[i] = 0;
        }
        cases.push(mixed);
        for (i, c) in cases.iter().enumerate() {
            let z = compress(c).unwrap();
            assert!(is_crilayla(&z));
            assert_eq!(decompressed_size(&z), Some(c.len() as u64));
            let back = decompress(&z).unwrap();
            assert_eq!(&back, c, "case {i}");
        }
        // Redundant data really shrinks.
        assert!(compress(&vec![7u8; 100_000]).unwrap().len() < 2000);
    }

    #[test]
    fn too_short_input() {
        assert!(compress(&[1, 2, 3]).is_err());
    }

    #[test]
    fn malformed_does_not_panic() {
        let z = compress(&b"abcabcabcabc".repeat(100)).unwrap();
        for n in 0..z.len() {
            let _ = decompress(&z[..n]);
        }
        let mut bad = z.clone();
        for i in 0x10..bad.len().min(0x40) {
            bad[i] ^= 0xA5;
            let _ = decompress(&bad);
        }
        let mut huge = z.clone();
        huge[8..12].copy_from_slice(&u32::MAX.to_le_bytes());
        assert!(decompress(&huge).is_err());
    }
}
