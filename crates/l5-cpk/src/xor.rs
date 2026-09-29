//! The Level-5 / CRI "filename XOR" stream cipher.
//!
//! Spec: `docs/formats/encryption.md` §1. Every `data/packs/*.cpk`, Viola's `data/packs_custom/*.cpk` and retail
//! loose CRI files (e.g. `data/dx11/movie/L5logo.usm`) are XOR-ed with a keystream derived from
//! `key = crc32(file basename)`.
//!
//! The keystream is position dependent and seekable: for absolute file offset `p`,
//!
//! ```text
//! state(p) = crc32(key.to_le_bytes(), seed = p & !3)      // zlib-style crc32 with an initial value
//! s        = (p & 3) * 2
//! ks(p)    = ((state >> s) & 3) << 6 | ((state >> (s+8)) & 3) << 4
//!          | ((state >> (s+16)) & 3) << 2 | ((state >> (s+24)) & 3)
//! ```
//!
//! so any byte range can be (de)crypted independently. Encryption and decryption are the same operation.
//! Offsets are truncated to 32 bits for the seed (Viola casts to `uint`); this was validated against a
//! 4.58 GB retail CPK (see the integration tests).

use std::io::{self, Read, Seek, SeekFrom, Write};

/// Standard IEEE CRC-32 table (polynomial 0xEDB88320).
const CRC_TABLE: [u32; 256] = {
    let mut table = [0u32; 256];
    let mut i = 0;
    while i < 256 {
        let mut c = i as u32;
        let mut j = 0;
        while j < 8 {
            c = if c & 1 != 0 { (c >> 1) ^ 0xEDB8_8320 } else { c >> 1 };
            j += 1;
        }
        table[i] = c;
        i += 1;
    }
    table
};

/// Standard CRC-32 (zlib) of a byte string.
pub fn crc32(data: &[u8]) -> u32 {
    crc32fast::hash(data)
}

/// XOR key of a file: `crc32(UTF-8 basename)`.
///
/// Anything up to the last `/` or `\` is stripped, so a full path may be passed.
/// Case matters (retail names are lowercase).
///
/// ```
/// assert_eq!(l5_cpk::xor::key_for_name("e832856918ebb97cb4430f715e6bd525.cpk"), 0x045D_9503);
/// ```
pub fn key_for_name(name: &str) -> u32 {
    crc32(basename(name).as_bytes())
}

/// The part of `path` after the last `/` or `\`.
pub(crate) fn basename(path: &str) -> &str {
    path.rsplit(['/', '\\']).next().unwrap_or(path)
}

/// Per-word keystream state: zlib `crc32(key_le_bytes, seed)`.
#[inline]
fn word_state(seed: u32, key: [u8; 4]) -> u32 {
    let mut crc = !seed;
    for b in key {
        crc = (crc >> 8) ^ CRC_TABLE[((crc ^ b as u32) & 0xFF) as usize];
    }
    !crc
}

/// Keystream byte `j` (0..4) of a word with state `st`.
#[inline]
fn ks_byte(st: u32, j: u32) -> u8 {
    let s = j * 2;
    ((((st >> s) & 3) << 6) | (((st >> (s + 8)) & 3) << 4) | (((st >> (s + 16)) & 3) << 2) | ((st >> (s + 24)) & 3))
        as u8
}

/// XOR `buf` in place with the keystream for `key`, where `buf[0]` sits at absolute file offset `offset`.
///
/// Symmetric: the same call encrypts and decrypts. Works for any offset/length; no need to process the
/// preceding bytes.
pub fn crypt_in_place(key: u32, offset: u64, buf: &mut [u8]) {
    let kb = key.to_le_bytes();
    let mut pos = offset;
    let mut rest = buf;
    // Leading partial word.
    let lead = (pos & 3) as usize;
    if lead != 0 && !rest.is_empty() {
        let st = word_state((pos & !3) as u32, kb);
        let n = (4 - lead).min(rest.len());
        let (head, tail) = rest.split_at_mut(n);
        for (i, b) in head.iter_mut().enumerate() {
            *b ^= ks_byte(st, (lead + i) as u32);
        }
        pos += n as u64;
        rest = tail;
    }
    // Aligned words.
    let mut chunks = rest.as_chunks_mut::<4>();
    for w in chunks.0.iter_mut() {
        let st = word_state(pos as u32, kb);
        w[0] ^= ks_byte(st, 0);
        w[1] ^= ks_byte(st, 1);
        w[2] ^= ks_byte(st, 2);
        w[3] ^= ks_byte(st, 3);
        pos += 4;
    }
    let tail = &mut chunks.1;
    if !tail.is_empty() {
        let st = word_state(pos as u32, kb);
        for (j, b) in tail.iter_mut().enumerate() {
            *b ^= ks_byte(st, j as u32);
        }
    }
}

/// Decrypt `buf` (which starts at absolute file offset `offset`) in place. Alias of [`crypt_in_place`].
pub fn decrypt_range(key: u32, offset: u64, buf: &mut [u8]) {
    crypt_in_place(key, offset, buf);
}

/// Encrypt `buf` (which starts at absolute file offset `offset`) in place. Alias of [`crypt_in_place`].
pub fn encrypt_range(key: u32, offset: u64, buf: &mut [u8]) {
    crypt_in_place(key, offset, buf);
}

/// Magics of plaintext files that may otherwise be XOR-encrypted (`docs/formats/encryption.md` §1.4).
pub const KNOWN_PLAIN_MAGICS: &[&[u8]] = &[b"CPK ", b"CRID", b"@UTF", b"AFS2", b"RDBN", b"CRILAYLA", b"HCA\0"];

/// True if `head` starts with one of [`KNOWN_PLAIN_MAGICS`].
pub fn has_known_magic(head: &[u8]) -> bool {
    KNOWN_PLAIN_MAGICS.iter().any(|m| head.starts_with(m))
}

/// Result of [`detect`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Encryption {
    /// The data is already plaintext.
    Plain,
    /// The data is XOR-encrypted with this key.
    Xor(u32),
}

impl Encryption {
    /// The XOR key, if any.
    pub fn key(self) -> Option<u32> {
        match self {
            Encryption::Plain => None,
            Encryption::Xor(k) => Some(k),
        }
    }
}

/// Detect whether a file is XOR-encrypted, given its name and its first bytes (≥ 4 recommended).
///
/// `is_plain` decides whether a candidate decryption of `head` looks right (e.g. `|h| h.starts_with(b"CPK ")`).
/// Tries plaintext, then `crc32(basename)`, then `crc32(lowercase basename)` — Viola's order
/// (`docs/formats/encryption.md` §1.1). Returns `None` if nothing matches.
pub fn detect_with(file_name: &str, head: &[u8], is_plain: impl Fn(&[u8]) -> bool) -> Option<Encryption> {
    if is_plain(head) {
        return Some(Encryption::Plain);
    }
    let base = basename(file_name);
    let mut candidates = vec![key_for_name(base)];
    let lower = crc32(base.to_lowercase().as_bytes());
    if lower != candidates[0] {
        candidates.push(lower);
    }
    for key in candidates {
        let mut tmp = head.to_vec();
        crypt_in_place(key, 0, &mut tmp);
        if is_plain(&tmp) {
            return Some(Encryption::Xor(key));
        }
    }
    None
}

/// [`detect_with`] using [`has_known_magic`] as the plaintext test.
pub fn detect(file_name: &str, head: &[u8]) -> Option<Encryption> {
    detect_with(file_name, head, has_known_magic)
}

/// A `Read + Seek` adapter that decrypts on the fly.
///
/// Wrap the raw file (ideally in a `BufReader`); reads at any position return plaintext, so a 4 GB CPK can be
/// accessed randomly without decrypting it whole. With `key = None` it is a transparent pass-through.
#[derive(Debug)]
pub struct XorReader<R> {
    inner: R,
    key: Option<u32>,
    pos: u64,
}

impl<R: Seek> XorReader<R> {
    /// Wrap `inner`; its current position is taken as the absolute file offset.
    pub fn new(mut inner: R, key: Option<u32>) -> io::Result<Self> {
        let pos = inner.stream_position()?;
        Ok(Self { inner, key, pos })
    }
}

impl<R> XorReader<R> {
    /// The key in use (`None` = plaintext pass-through).
    pub fn key(&self) -> Option<u32> {
        self.key
    }
    /// Borrow the wrapped reader.
    pub fn get_ref(&self) -> &R {
        &self.inner
    }
    /// Unwrap.
    pub fn into_inner(self) -> R {
        self.inner
    }
}

impl<R: Read> Read for XorReader<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let n = self.inner.read(buf)?;
        if let Some(key) = self.key {
            crypt_in_place(key, self.pos, &mut buf[..n]);
        }
        self.pos += n as u64;
        Ok(n)
    }
}

impl<R: Seek> Seek for XorReader<R> {
    fn seek(&mut self, pos: SeekFrom) -> io::Result<u64> {
        self.pos = self.inner.seek(pos)?;
        Ok(self.pos)
    }
}

/// A sequential `Write` adapter that encrypts on the fly (used by the CPK writer).
#[derive(Debug)]
pub struct XorWriter<W> {
    inner: W,
    key: Option<u32>,
    pos: u64,
    scratch: Vec<u8>,
}

impl<W: Write> XorWriter<W> {
    /// Wrap `inner`; `start_offset` is the absolute file offset of the next byte written.
    pub fn new(inner: W, key: Option<u32>, start_offset: u64) -> Self {
        Self { inner, key, pos: start_offset, scratch: Vec::new() }
    }
    /// Absolute offset of the next byte.
    pub fn position(&self) -> u64 {
        self.pos
    }
    /// Unwrap (does not flush).
    pub fn into_inner(self) -> W {
        self.inner
    }
}

impl<W: Write> Write for XorWriter<W> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let n = match self.key {
            None => self.inner.write(buf)?,
            Some(key) => {
                let take = buf.len().min(1 << 20);
                self.scratch.clear();
                self.scratch.extend_from_slice(&buf[..take]);
                crypt_in_place(key, self.pos, &mut self.scratch);
                // write_all keeps keystream positions consistent with what we report.
                self.inner.write_all(&self.scratch)?;
                take
            }
        };
        self.pos += n as u64;
        Ok(n)
    }
    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Straight transcription of Viola's `CCriwareCrypt.DecryptBlock` as the reference.
    fn viola_decrypt(buf: &mut [u8], file_offset: u64, key: u32) {
        let kb = key.to_le_bytes();
        let upd = |seed: u32| {
            let mut crc = !seed;
            for &k in &kb {
                crc = (crc >> 8) ^ CRC_TABLE[((crc & 0xFF) as u8 ^ k) as usize];
            }
            !crc
        };
        let mut cur = upd((file_offset & !3) as u32);
        for (i, b) in buf.iter_mut().enumerate() {
            let g = i as u64 + file_offset;
            if g & 3 == 0 {
                cur = upd(g as u32);
            }
            let bs = ((g & 3) * 2) as u32;
            let mut r8 = (cur >> (bs + 8)) & 3;
            let rdx = (cur >> bs) & 0xFF;
            r8 |= (rdx << 2) & 0xFF;
            let rdx = (cur >> (bs + 16)) & 3;
            r8 = ((r8 << 2) & 0xFF) | rdx;
            let rdx = (cur >> (bs + 24)) & 3;
            r8 = ((r8 << 2) & 0xFF) | rdx;
            *b ^= r8 as u8;
        }
    }

    #[test]
    fn key_of_known_names() {
        assert_eq!(key_for_name("e832856918ebb97cb4430f715e6bd525.cpk"), 0x045D_9503);
        assert_eq!(key_for_name("data/packs/e832856918ebb97cb4430f715e6bd525.cpk"), 0x045D_9503);
        assert_eq!(key_for_name("cpk_list.cfg.bin"), 0x1717_E18E);
        assert_eq!(key_for_name("key"), 0x8A90_ABA9);
        assert_eq!(key_for_name("iv"), 0x4C80_1618);
    }

    #[test]
    fn matches_viola_for_all_alignments() {
        let key = 0x045D_9503;
        for off in [0u64, 1, 2, 3, 5, 0x7FF, 0x1_0000_0001, 0xFFFF_FFFE] {
            for len in [0usize, 1, 2, 3, 4, 5, 7, 64, 1001] {
                let mut a: Vec<u8> = (0..len).map(|i| (i * 31 + 7) as u8).collect();
                let mut b = a.clone();
                crypt_in_place(key, off, &mut a);
                viola_decrypt(&mut b, off, key);
                assert_eq!(a, b, "off={off} len={len}");
            }
        }
    }

    #[test]
    fn ranges_decrypt_independently() {
        let key = 0xDEAD_BEEF;
        let plain: Vec<u8> = (0..4096u32).map(|i| (i ^ (i >> 3)) as u8).collect();
        let mut enc = plain.clone();
        crypt_in_place(key, 0, &mut enc);
        for (a, b) in [(0usize, 10usize), (3, 17), (1000, 3001), (4095, 4096)] {
            let mut part = enc[a..b].to_vec();
            decrypt_range(key, a as u64, &mut part);
            assert_eq!(part, &plain[a..b]);
        }
    }

    #[test]
    fn reader_and_writer_roundtrip() {
        let key = key_for_name("x.cpk");
        let plain: Vec<u8> = (0..10_000u32).map(|i| (i * 7) as u8).collect();
        let mut w = XorWriter::new(Vec::new(), Some(key), 0);
        w.write_all(&plain[..333]).unwrap();
        w.write_all(&plain[333..]).unwrap();
        let enc = w.into_inner();
        assert_ne!(enc, plain);
        let mut r = XorReader::new(io::Cursor::new(enc), Some(key)).unwrap();
        r.seek(SeekFrom::Start(4321)).unwrap();
        let mut got = vec![0; 100];
        r.read_exact(&mut got).unwrap();
        assert_eq!(got, &plain[4321..4421]);
    }

    #[test]
    fn detect_cpk() {
        let mut head = b"CPK \xff\0\0\0".to_vec();
        assert_eq!(detect("a.cpk", &head), Some(Encryption::Plain));
        crypt_in_place(key_for_name("a.cpk"), 0, &mut head);
        assert_eq!(detect("dir/a.cpk", &head), Some(Encryption::Xor(key_for_name("a.cpk"))));
        assert_eq!(detect("A.CPK", &head), Some(Encryption::Xor(key_for_name("a.cpk"))));
        assert_eq!(detect("b.cpk", &head), None);
    }
}
