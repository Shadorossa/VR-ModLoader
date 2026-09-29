//! Plaintext model of `data/cpk_list.cfg.bin` (after AES decryption).
//!
//! Spec: `docs/formats/cpk_list.md` §3 (layout), §5 (how Viola edits it), §8 (adding new files).
//! The writer reproduces the Level-5 ("vanilla") layout: `0xFF` padding, null cpk fields as `-1`,
//! string pool deduplicated in first-use order. Unmodified vanilla lists round-trip byte-exact
//! except for pool order, which the game does not care about.

use std::collections::HashMap;

use crate::error::{Error, Result};

/// CRC-32 of the record name `CPK_ITEM_BEGIN`.
pub const CRC_ITEM_BEGIN: u32 = 0xF0C5_583A;
/// CRC-32 of the record name `CPK_ITEM`.
pub const CRC_ITEM: u32 = 0x36F3_46D3;

/// One file known to the game.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CpkItem {
    /// Directory with trailing `/`, `""` for files at the game root.
    pub dir: String,
    /// File name.
    pub name: String,
    /// CPK directory (e.g. `data/packs/`), `None` when the file is loose.
    pub cpk_dir: Option<String>,
    /// CPK file name, `None` when the file is loose.
    pub cpk_name: Option<String>,
    /// Extracted size in bytes.
    pub size: i32,
}

impl CpkItem {
    pub fn path(&self) -> String {
        format!("{}{}", self.dir, self.name)
    }

    pub fn is_loose(&self) -> bool {
        self.cpk_name.as_deref().map_or(true, str::is_empty)
    }

    /// `cpkDir + cpkName`, or `None` when loose.
    pub fn cpk_path(&self) -> Option<String> {
        if self.is_loose() {
            None
        } else {
            Some(format!("{}{}", self.cpk_dir.as_deref().unwrap_or(""), self.cpk_name.as_deref().unwrap_or("")))
        }
    }

    /// Sort key used by the game: standard CRC-32 of the UTF-8 path.
    pub fn hash(&self) -> u32 {
        let mut h = crc32fast::Hasher::new();
        h.update(self.dir.as_bytes());
        h.update(self.name.as_bytes());
        h.finalize()
    }

    /// Split a full logical path into `(dir, name)` the way Viola does.
    pub fn split_path(full: &str) -> (String, String) {
        match full.rfind('/') {
            Some(i) => (full[..=i].to_string(), full[i + 1..].to_string()),
            None => (String::new(), full.to_string()),
        }
    }
}

/// Parsed list. `tail` is the key table + footer, kept verbatim.
#[derive(Debug, Clone)]
pub struct CpkList {
    pub items: Vec<CpkItem>,
    tail: Vec<u8>,
}

fn align(v: usize, a: usize) -> usize {
    (v + a - 1) & !(a - 1)
}

fn rd_u32(b: &[u8], p: usize) -> Result<u32> {
    b.get(p..p + 4)
        .map(|s| u32::from_le_bytes(s.try_into().unwrap()))
        .ok_or_else(|| Error::Format(format!("cpk_list truncated at 0x{p:X}")))
}

impl CpkList {
    /// Parse the AES-decrypted bytes.
    pub fn parse(b: &[u8]) -> Result<Self> {
        if b.len() < 0x20 || rd_u32(b, b.len() - 0x10)? != 0x6232_7401 {
            return Err(Error::Format("cpk_list: missing T2B footer (wrong key or not decrypted?)".into()));
        }
        let count = rd_u32(b, 0)? as usize;
        let str_off = rd_u32(b, 4)? as usize;
        let str_size = rd_u32(b, 8)? as usize;
        let pool = b
            .get(str_off..str_off + str_size)
            .ok_or_else(|| Error::Format("cpk_list: string pool out of range".into()))?;
        let read_str = |o: i32| -> Result<Option<String>> {
            if o < 0 {
                return Ok(None);
            }
            let o = o as usize;
            let s = pool.get(o..).ok_or_else(|| Error::Format(format!("cpk_list: string offset {o} out of range")))?;
            let end = s.iter().position(|&c| c == 0).unwrap_or(s.len());
            Ok(Some(String::from_utf8_lossy(&s[..end]).into_owned()))
        };

        let mut p = 16;
        let mut items = Vec::with_capacity(count);
        for _ in 0..count {
            let crc = rd_u32(b, p)?;
            let n = *b.get(p + 4).ok_or_else(|| Error::Format("cpk_list: truncated record".into()))? as usize;
            p += 5;
            let types: Vec<u8> = (0..n).map(|i| (b[p + i / 4] >> (2 * (i % 4))) & 3).collect();
            p = align(p + n.div_ceil(4), 4);
            let mut vals = Vec::with_capacity(n);
            for i in 0..n {
                vals.push(rd_u32(b, p + 4 * i)? as i32);
            }
            p += 4 * n;
            if crc == CRC_ITEM && types == [0, 0, 0, 0, 1] {
                items.push(CpkItem {
                    dir: read_str(vals[0])?.unwrap_or_default(),
                    name: read_str(vals[1])?.unwrap_or_default(),
                    cpk_dir: read_str(vals[2])?,
                    cpk_name: read_str(vals[3])?,
                    size: vals[4],
                });
            } else if crc != CRC_ITEM_BEGIN {
                return Err(Error::Format(format!("cpk_list: unexpected record 0x{crc:08X}")));
            }
        }

        // Tail = key table + footer. The key table starts right after the 16-aligned pool.
        let tail_start = align(str_off + str_size, 16);
        let tail = b.get(tail_start..).unwrap_or_default().to_vec();
        Ok(Self { items, tail })
    }

    /// Serialize to plaintext (encrypt with AES before writing to disk).
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut pool: Vec<u8> = Vec::new();
        let mut offsets: HashMap<&str, i32> = HashMap::new();
        // Intern first so the pool order follows record order, like Level-5 and Viola.
        let mut refs: Vec<[i32; 4]> = Vec::with_capacity(self.items.len());
        for it in &self.items {
            let mut r = [-1i32; 4];
            let fields = [Some(it.dir.as_str()), Some(it.name.as_str()), it.cpk_dir.as_deref(), it.cpk_name.as_deref()];
            for (slot, s) in r.iter_mut().zip(fields) {
                if let Some(s) = s {
                    *slot = *offsets.entry(s).or_insert_with(|| {
                        let o = pool.len() as i32;
                        pool.extend_from_slice(s.as_bytes());
                        pool.push(0);
                        o
                    });
                }
            }
            refs.push(r);
        }
        let distinct = offsets.len() as u32;

        let records_len = 12 + 28 * self.items.len();
        let str_off = align(16 + records_len, 16);
        let mut out = Vec::with_capacity(str_off + pool.len() + 16 + self.tail.len());
        out.extend_from_slice(&((self.items.len() + 1) as u32).to_le_bytes());
        out.extend_from_slice(&(str_off as u32).to_le_bytes());
        out.extend_from_slice(&(pool.len() as u32).to_le_bytes());
        out.extend_from_slice(&distinct.to_le_bytes());
        // CPK_ITEM_BEGIN: crc, count 1, type byte 0b01 (int), 2 pad, value = item count.
        out.extend_from_slice(&CRC_ITEM_BEGIN.to_le_bytes());
        out.extend_from_slice(&[1, 0x01, 0xFF, 0xFF]);
        out.extend_from_slice(&(self.items.len() as i32).to_le_bytes());
        for (it, r) in self.items.iter().zip(&refs) {
            out.extend_from_slice(&CRC_ITEM.to_le_bytes());
            // 5 values, types [S,S,S,S,I] packed 2 bits LSB-first = 0x00, 0x01; 1 pad byte.
            out.extend_from_slice(&[5, 0x00, 0x01, 0xFF]);
            for v in r {
                out.extend_from_slice(&v.to_le_bytes());
            }
            out.extend_from_slice(&it.size.to_le_bytes());
        }
        out.resize(str_off, 0xFF);
        out.extend_from_slice(&pool);
        out.resize(align(out.len(), 16), 0xFF);
        out.extend_from_slice(&self.tail);
        out
    }

    /// Stable sort by path CRC-32; on ties the existing order is kept (new entries go last).
    pub fn sort(&mut self) {
        self.items.sort_by_cached_key(CpkItem::hash);
    }

    pub fn is_sorted(&self) -> bool {
        self.items.windows(2).all(|w| w[0].hash() <= w[1].hash())
    }

    /// Case-insensitive path → index map (the list has no case-insensitive duplicates).
    pub fn path_index(&self) -> HashMap<String, usize> {
        self.items.iter().enumerate().map(|(i, it)| (it.path().to_lowercase(), i)).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> CpkList {
        // Minimal tail: key table header + 2 keys + names + footer (content is opaque to us).
        let mut tail = vec![0u8; 0x40];
        tail.extend_from_slice(&[0x01, 0x74, 0x32, 0x62, 0xFE, 0x01, 0x01, 0x00, 0x01, 0x00, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF]);
        CpkList {
            items: vec![
                CpkItem { dir: "data/common/".into(), name: "a.cfg.bin".into(), cpk_dir: Some("data/packs/".into()), cpk_name: Some("x.cpk".into()), size: 10 },
                CpkItem { dir: "data/dx11/movie/".into(), name: "L5logo.usm".into(), cpk_dir: None, cpk_name: None, size: 3 },
            ],
            tail,
        }
    }

    #[test]
    fn round_trip() {
        let mut l = sample();
        l.sort();
        let b = l.to_bytes();
        let l2 = CpkList::parse(&b).unwrap();
        assert_eq!(l.items, l2.items);
        assert_eq!(b, l2.to_bytes());
        assert!(l2.items.iter().any(|i| i.is_loose()));
    }

    #[test]
    fn split() {
        assert_eq!(CpkItem::split_path("data/a/b.g4tx"), ("data/a/".into(), "b.g4tx".into()));
        assert_eq!(CpkItem::split_path("x.dll"), ("".into(), "x.dll".into()));
    }
}
