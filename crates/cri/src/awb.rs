//! `AFS2` archives (`.awb`, and the memory AWB embedded in an ACB) — docs/formats/audio-acb-awb-hca.md §3.
//! Integers are little-endian. Only the header is needed to locate a payload, so a file prefix is enough.

use crate::{Error, Result};

#[derive(Debug, Clone)]
pub struct Entry {
    /// Waveform id (the `StreamAwbId` / `MemoryAwbId` of the ACB); ids, not indices.
    pub id: u32,
    /// Absolute byte range of the payload (start already aligned).
    pub start: u64,
    pub end: u64,
}

#[derive(Debug, Clone)]
pub struct Afs2 {
    pub version: u8,
    pub offset_size: u8,
    pub id_size: u8,
    pub alignment: u16,
    /// HCA subkey (0 in this game).
    pub subkey: u16,
    pub entries: Vec<Entry>,
    /// Size of the header (`0x10 + n*idSize + (n+1)*offSize`), the part an ACB copies into `StreamAwbAfs2Header`.
    pub header_len: usize,
    /// Total size of the archive as given by the last offset.
    pub total_len: u64,
}

fn le(d: &[u8], o: usize, size: usize) -> Result<u64> {
    let b = d.get(o..o + size).ok_or_else(|| Error::Afs2(format!("header truncated at 0x{o:X}")))?;
    Ok(b.iter().rev().fold(0u64, |acc, x| (acc << 8) | *x as u64))
}

impl Afs2 {
    pub fn is_afs2(d: &[u8]) -> bool {
        d.len() >= 0x10 && &d[..4] == b"AFS2"
    }

    /// Minimum prefix needed to parse a header, once the file count is known (`None` if fewer than 16 bytes).
    pub fn header_len_of(d: &[u8]) -> Option<usize> {
        if !Self::is_afs2(d) {
            return None;
        }
        let (offset_size, id_size) = (d[5] as usize, d[6] as usize);
        let n = u32::from_le_bytes([d[8], d[9], d[10], d[11]]) as usize;
        Some(0x10 + n * id_size + (n + 1) * offset_size)
    }

    /// Parse the header from the file (or a prefix long enough to hold it).
    pub fn parse(d: &[u8]) -> Result<Afs2> {
        if !Self::is_afs2(d) {
            return Err(Error::Afs2("missing AFS2 magic".into()));
        }
        let version = d[4];
        let offset_size = d[5];
        let id_size = d[6];
        if !matches!(offset_size, 2 | 4 | 8) || !matches!(id_size, 1 | 2 | 4) {
            return Err(Error::Afs2(format!("unsupported sizes: offset {offset_size}, id {id_size}")));
        }
        let n = u32::from_le_bytes([d[8], d[9], d[10], d[11]]) as usize;
        let alignment = u16::from_le_bytes([d[12], d[13]]);
        let subkey = u16::from_le_bytes([d[14], d[15]]);
        let header_len = 0x10 + n * id_size as usize + (n + 1) * offset_size as usize;
        if d.len() < header_len {
            return Err(Error::Afs2(format!("header needs {header_len} bytes, got {}", d.len())));
        }
        let mut ids = Vec::with_capacity(n);
        let mut p = 0x10;
        for _ in 0..n {
            ids.push(le(d, p, id_size as usize)? as u32);
            p += id_size as usize;
        }
        let mut offsets = Vec::with_capacity(n + 1);
        for _ in 0..=n {
            offsets.push(le(d, p, offset_size as usize)?);
            p += offset_size as usize;
        }
        let align = alignment.max(1) as u64;
        let entries = (0..n)
            .map(|i| {
                let start = offsets[i].div_ceil(align) * align;
                Entry { id: ids[i], start, end: offsets[i + 1].max(start) }
            })
            .collect();
        Ok(Afs2 { version, offset_size, id_size, alignment, subkey, entries, header_len, total_len: offsets[n] })
    }

    pub fn entry(&self, id: u32) -> Option<&Entry> {
        self.entries.iter().find(|e| e.id == id)
    }

    /// Payload of `id` inside a fully loaded archive.
    pub fn payload<'a>(&self, archive: &'a [u8], id: u32) -> Option<&'a [u8]> {
        let e = self.entry(id)?;
        archive.get(e.start as usize..e.end as usize)
    }
}

// ---------------------------------------------------------------- writer (port of tools/py/acb.py `build_awb`)

/// Where an entry's bytes come from.
#[derive(Debug, Clone, PartialEq)]
pub enum Payload {
    Bytes(Vec<u8>),
    /// A byte range of the source archive (copied without loading the whole archive).
    Range(u64, u64),
}

impl Payload {
    pub fn len(&self) -> u64 {
        match self {
            Payload::Bytes(b) => b.len() as u64,
            Payload::Range(s, e) => e - s,
        }
    }
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// An archive to write: the header fields of the source plus the entries (ids ascending).
#[derive(Debug, Clone, PartialEq)]
pub struct AwbPlan {
    pub version: u8,
    pub offset_size: u8,
    pub id_size: u8,
    pub alignment: u16,
    pub subkey: u16,
    pub entries: Vec<(u32, Payload)>,
}

/// What [`write`] produced.
#[derive(Debug, Clone, PartialEq)]
pub struct Written {
    pub size: u64,
    /// MD5 of the plain archive (the ACB's `StreamAwbHash`).
    pub md5: [u8; 16],
    /// The plain archive up to its first payload (header padded to the alignment: `StreamAwbAfs2Header`).
    pub header: Vec<u8>,
}

impl AwbPlan {
    /// Every entry of a parsed archive as a range of that archive.
    pub fn from_afs2(a: &Afs2) -> AwbPlan {
        AwbPlan {
            version: a.version,
            offset_size: a.offset_size,
            id_size: a.id_size,
            alignment: a.alignment,
            subkey: a.subkey,
            entries: a.entries.iter().map(|e| (e.id, Payload::Range(e.start, e.end))).collect(),
        }
    }

    /// Replace (or add) entry `id`, keeping the ids sorted.
    pub fn put(&mut self, id: u32, p: Payload) {
        match self.entries.iter().position(|e| e.0 == id) {
            Some(i) => self.entries[i].1 = p,
            None => {
                self.entries.push((id, p));
                self.entries.sort_by_key(|e| e.0);
            }
        }
    }

    pub fn max_id(&self) -> Option<u32> {
        self.entries.iter().map(|e| e.0).max()
    }

    /// Serialize to `out` (optionally XOR-encrypted with `xor_key`, as a loose file), reading ranges from `src`.
    pub fn write(&self, mut src: Option<&mut dyn ReadSeek>, out: &mut dyn std::io::Write, xor_key: Option<u32>) -> std::io::Result<Written> {
        use md5::Digest;
        use std::io::{Error as IoError, SeekFrom};
        let n = self.entries.len();
        let (osz, isz) = (self.offset_size as usize, self.id_size as usize);
        let align = self.alignment.max(1) as u64;
        let hl = (0x10 + n * isz + (n + 1) * osz) as u64;
        let mut offs = vec![hl];
        let mut starts = Vec::with_capacity(n);
        let mut pos = hl;
        for (_, p) in &self.entries {
            let s = pos.div_ceil(align) * align;
            starts.push(s);
            pos = s + p.len();
            offs.push(pos);
        }
        let mut head = Vec::with_capacity(hl as usize);
        head.extend_from_slice(b"AFS2");
        head.extend_from_slice(&[self.version, self.offset_size, self.id_size, 0]);
        head.extend_from_slice(&(n as u32).to_le_bytes());
        head.extend_from_slice(&self.alignment.to_le_bytes());
        head.extend_from_slice(&self.subkey.to_le_bytes());
        for (id, _) in &self.entries {
            head.extend_from_slice(&(*id as u64).to_le_bytes()[..isz]);
        }
        for o in &offs {
            if osz < 8 && *o >> (osz * 8) != 0 {
                return Err(IoError::other("AFS2 offset does not fit the offset size"));
            }
            head.extend_from_slice(&o.to_le_bytes()[..osz]);
        }
        let mut md5 = md5::Md5::new();
        let mut written: u64 = 0;
        let mut emit = |chunk: &[u8], out: &mut dyn std::io::Write, written: &mut u64| -> std::io::Result<()> {
            md5.update(chunk);
            match xor_key {
                Some(k) => {
                    let mut c = chunk.to_vec();
                    crate::crypt::xor_in_place(&mut c, k, *written);
                    out.write_all(&c)?;
                }
                None => out.write_all(chunk)?,
            }
            *written += chunk.len() as u64;
            Ok(())
        };
        emit(&head, out, &mut written)?;
        let first = starts.first().copied().unwrap_or(hl);
        let mut header = head.clone();
        header.resize(first.min(hl.div_ceil(align) * align) as usize, 0);
        let mut buf = vec![0u8; 1 << 20];
        for ((_, p), s) in self.entries.iter().zip(&starts) {
            let pad = vec![0u8; (s - written) as usize];
            emit(&pad, out, &mut written)?;
            match p {
                Payload::Bytes(b) => emit(b, out, &mut written)?,
                Payload::Range(a, e) => {
                    let r = src.as_mut().ok_or_else(|| IoError::other("range payload without a source"))?;
                    r.seek(SeekFrom::Start(*a))?;
                    let mut left = e - a;
                    while left > 0 {
                        let k = left.min(buf.len() as u64) as usize;
                        r.read_exact(&mut buf[..k])?;
                        emit(&buf[..k], out, &mut written)?;
                        left -= k as u64;
                    }
                }
            }
        }
        Ok(Written { size: written, md5: md5.finalize().into(), header })
    }

    /// [`Self::write`] into memory (no ranges: every payload must be bytes).
    pub fn to_vec(&self) -> std::io::Result<(Vec<u8>, Written)> {
        let mut v = Vec::new();
        let w = self.write(None, &mut v, None)?;
        Ok((v, w))
    }
}

/// `Read + Seek` as one object-safe trait.
pub trait ReadSeek: std::io::Read + std::io::Seek {}
impl<T: std::io::Read + std::io::Seek> ReadSeek for T {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_two_entries_with_alignment() {
        let mut d = Vec::new();
        d.extend_from_slice(b"AFS2");
        d.extend_from_slice(&[2, 4, 2, 0]);
        d.extend_from_slice(&2u32.to_le_bytes());
        d.extend_from_slice(&0x20u16.to_le_bytes());
        d.extend_from_slice(&0u16.to_le_bytes());
        d.extend_from_slice(&7u16.to_le_bytes());
        d.extend_from_slice(&9u16.to_le_bytes());
        // header is 0x10 + 4 + 12 = 0x20; offsets: end of header, unaligned end of first, file size
        d.extend_from_slice(&0x20u32.to_le_bytes());
        d.extend_from_slice(&0x25u32.to_le_bytes());
        d.extend_from_slice(&0x48u32.to_le_bytes());
        let a = Afs2::parse(&d).unwrap();
        assert_eq!(a.header_len, 0x20);
        assert_eq!(a.entries.len(), 2);
        assert_eq!((a.entries[0].id, a.entries[0].start, a.entries[0].end), (7, 0x20, 0x25));
        assert_eq!((a.entries[1].id, a.entries[1].start, a.entries[1].end), (9, 0x40, 0x48));
        assert_eq!(Afs2::header_len_of(&d), Some(0x20));
    }

    #[test]
    fn writer_round_trips_retail_archives() {
        let dir = std::path::Path::new(crate::DUMP_SOUND_ASSET);
        for name in ["bgm_title.awb", "ja/c01000010.awb", "sr.awb"] {
            let Ok(raw) = std::fs::read(dir.join(name)) else { continue };
            let d = if &raw[..4] == b"AFS2" { raw } else { crate::crypt::xor(&raw, crate::crypt::loose_key(name.rsplit('/').next().unwrap()), 0) };
            let a = Afs2::parse(&d).unwrap();
            let plan = AwbPlan::from_afs2(&a);
            let mut out = Vec::new();
            let w = plan.write(Some(&mut std::io::Cursor::new(&d)), &mut out, None).unwrap();
            assert_eq!(out, d, "{name}");
            use md5::Digest;
            assert_eq!(w.md5, <[u8; 16]>::from(md5::Md5::digest(&d)));
            // encrypted output = XOR of the plain one
            let mut enc = Vec::new();
            plan.write(Some(&mut std::io::Cursor::new(&d)), &mut enc, Some(7)).unwrap();
            assert_eq!(crate::crypt::xor(&enc, 7, 0), d);
        }
    }
}
