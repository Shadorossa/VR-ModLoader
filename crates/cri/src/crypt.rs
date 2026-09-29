//! Loose-file XOR of CRI files (docs/formats/encryption.md §1; `tools/py/acb.py` `xor_crypt`): the game's file layer
//! decrypts every loose `.acb` / `.awb` / `.usm` / `.cpk` / `.acf` it reads with the key `crc32(file name)`, byte by
//! byte from 2-bit slices of `crc32(key bytes, init = position & !3)`. Symmetric (encrypt = decrypt).

/// Key of a loose file: crc32 of its name (no folder), as the game spells it.
pub fn loose_key(basename: &str) -> u32 {
    crc32fast::hash(basename.as_bytes())
}

/// XOR `data` (which starts at byte `file_offset` of the file) with key `key`.
pub fn xor(data: &[u8], key: u32, file_offset: u64) -> Vec<u8> {
    let mut out = data.to_vec();
    xor_in_place(&mut out, key, file_offset);
    out
}

pub fn xor_in_place(out: &mut [u8], key: u32, file_offset: u64) {
    let kb = key.to_le_bytes();
    let mut p = file_offset;
    let mut i = 0;
    while i < out.len() {
        let mut h = crc32fast::Hasher::new_with_initial((p & !3) as u32);
        h.update(&kb);
        let st = h.finalize().to_le_bytes();
        let mut j = (p & 3) as usize;
        while j < 4 && i < out.len() {
            let sh = j * 2;
            out[i] ^= (((st[0] >> sh) & 3) << 6) | (((st[1] >> sh) & 3) << 4) | (((st[2] >> sh) & 3) << 2) | ((st[3] >> sh) & 3);
            i += 1;
            p += 1;
            j += 1;
        }
    }
}

/// A reader that decrypts a loose-XOR file on the fly (random access: the key stream depends only on the position).
pub struct XorReader<R> {
    inner: R,
    key: u32,
    pos: u64,
}

impl<R> XorReader<R> {
    pub fn new(inner: R, key: u32) -> Self {
        XorReader { inner, key, pos: 0 }
    }
}

impl<R: std::io::Read> std::io::Read for XorReader<R> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let n = self.inner.read(buf)?;
        xor_in_place(&mut buf[..n], self.key, self.pos);
        self.pos += n as u64;
        Ok(n)
    }
}

impl<R: std::io::Seek> std::io::Seek for XorReader<R> {
    fn seek(&mut self, to: std::io::SeekFrom) -> std::io::Result<u64> {
        self.pos = self.inner.seek(to)?;
        Ok(self.pos)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn symmetric_and_offset_consistent() {
        let data: Vec<u8> = (0..1000u32).map(|i| (i * 31) as u8).collect();
        let k = loose_key("c01000010.acb");
        let enc = xor(&data, k, 0);
        assert_ne!(enc, data);
        assert_eq!(xor(&enc, k, 0), data);
        // encrypting a slice at its offset gives the same bytes as the whole-file pass
        assert_eq!(xor(&data[7..500], k, 7), enc[7..500]);
    }

}
