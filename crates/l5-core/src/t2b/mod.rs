//! T2B cfg.bin — the flat "entry list" container used by 70 798 Victory Road files.
//!
//! Byte-level spec: `docs/formats/cfgbin.md` §1 (layout) and §2 (nesting conventions).
//! Reference implementation: `read_t2b` / `write_t2b` in `tools/py/cfgbin.py`.
//!
//! [`T2b::parse`] followed by [`T2b::to_bytes`] reproduces every game file byte for byte:
//! the writer follows the canonical game layout (§1.7), including the value-string pooling
//! style detected on read (§1.4) and the key table order (§1.5).

mod tree;

pub use tree::{Node, SortedList, TreeMode, build_tree};

use serde::{Deserialize, Serialize};

use crate::bytes::{self, StringPool, align, align_checked, pad_to};
use crate::error::{Error, Result};
use crate::hash;
use crate::text::TextEncoding;

/// Footer magic `01 74 32 62` ("\x01t2b"), stored at `len - 16` (cfgbin.md §1.6).
pub const MAGIC: [u8; 4] = [0x01, 0x74, 0x32, 0x62];
/// Size of the footer in bytes.
pub const FOOTER_SIZE: usize = 16;
/// Name of the entries that hold a counted list's sort index (cfgbin.md §2.2).
pub const SORT_INDEX_NAME: &str = "__SORT_INDEX";

/// Value type code 0 (cfgbin.md §1.2).
pub const TYPE_STRING: u8 = 0;
/// Value type code 1.
pub const TYPE_INT: u8 = 1;
/// Value type code 2.
pub const TYPE_FLOAT: u8 = 2;

/// Does `data` end with a T2B footer? (cfgbin.md §1.6)
pub fn is_t2b(data: &[u8]) -> bool {
    data.len() >= FOOTER_SIZE && data[data.len() - FOOTER_SIZE..data.len() - 12] == MAGIC
}

/// One typed value of an entry (cfgbin.md §1.2).
///
/// JSON: `{"type": "int", "value": 5}`, `{"type": "float", "value": 1.5}`,
/// `{"type": "string", "value": "text" | null}`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", content = "value", rename_all = "camelCase")]
pub enum Value {
    /// Type 1. Many ints are CRC-32 IDs: see [`Value::as_hash`].
    Int(i32),
    /// Type 2 (IEEE f32; the bit pattern is preserved).
    Float(f32),
    /// Type 0; `None` is the null offset `-1`.
    String(Option<String>),
}

impl Value {
    /// On-disk 2-bit type code.
    pub fn type_code(&self) -> u8 {
        match self {
            Value::String(_) => TYPE_STRING,
            Value::Int(_) => TYPE_INT,
            Value::Float(_) => TYPE_FLOAT,
        }
    }

    /// The int value, if this is an int.
    pub fn as_int(&self) -> Option<i32> {
        match *self {
            Value::Int(v) => Some(v),
            _ => None,
        }
    }

    /// The int value reinterpreted as an unsigned CRC-32 ID.
    pub fn as_hash(&self) -> Option<u32> {
        self.as_int().map(hash::to_u32)
    }

    /// The float value, if this is a float.
    pub fn as_float(&self) -> Option<f32> {
        match *self {
            Value::Float(v) => Some(v),
            _ => None,
        }
    }

    /// The string value (`None` both for null strings and non-strings).
    pub fn as_str(&self) -> Option<&str> {
        match self {
            Value::String(Some(s)) => Some(s),
            _ => None,
        }
    }
}

/// One entry record: `crc32(name)` + up to 255 typed values (cfgbin.md §1.2).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Entry {
    /// Name resolved through the key table, `None` if the hash has no key record.
    pub name: Option<String>,
    /// Name hash as written in the record. The writer always writes this value;
    /// use [`T2b::rename_entry`] / [`T2b::new_entry`] to keep it in sync with `name`.
    pub hash: u32,
    /// Typed values (types are per record, not per name).
    pub values: Vec<Value>,
}

impl Entry {
    /// Name, or `#XXXXXXXX` for unresolved hashes.
    pub fn display_name(&self) -> std::borrow::Cow<'_, str> {
        match &self.name {
            Some(n) => n.as_str().into(),
            None => format!("#{:08X}", self.hash).into(),
        }
    }
}

/// How the value-string pool is shared (cfgbin.md §1.4).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum StringDedup {
    /// Identical strings are stored once (60 391 VR files).
    #[default]
    Exact,
    /// Every string value is written anew (10 407 VR files: events, some maps).
    None,
}

/// Hash algorithm of the key table, detected from the first key record.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum HashKind {
    /// Standard CRC-32 (all of Victory Road).
    #[default]
    Crc32,
    /// `!crc32` (older Level-5 titles).
    JamCrc,
    /// Neither matched (or no keys). Entries keep their stored hashes regardless.
    Unknown,
}

impl HashKind {
    /// Hash `bytes` with this algorithm (Unknown falls back to CRC-32).
    pub fn hash(self, bytes: &[u8]) -> u32 {
        match self {
            HashKind::JamCrc => hash::jamcrc(bytes),
            _ => hash::crc32(bytes),
        }
    }
}

/// The 16-byte footer (cfgbin.md §1.6), minus the magic and the `FF` padding.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Footer {
    /// `+0x4`, always `0x01FE`.
    pub unk1: i16,
    /// `+0x6`, raw encoding flag: 0 = Shift-JIS, anything else = UTF-8.
    pub encoding: i16,
    /// `+0x8`, always `1`.
    pub unk2: i16,
}

impl Default for Footer {
    fn default() -> Self {
        Footer {
            unk1: 0x01FE,
            encoding: 1,
            unk2: 1,
        }
    }
}

impl Footer {
    /// Text encoding selected by the flag.
    pub fn text_encoding(&self) -> TextEncoding {
        if self.encoding == 0 {
            TextEncoding::ShiftJis
        } else {
            TextEncoding::Utf8
        }
    }
}

/// Header values as read from the file (informational; the writer recomputes them).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct T2bInfo {
    pub entry_count: u32,
    pub string_offset: u32,
    pub string_length: u32,
    pub string_count: u32,
    pub key_section: u32,
    pub key_size: u32,
    pub key_count: u32,
    pub key_string_offset: u32,
    pub key_string_size: u32,
    pub file_size: u32,
}

/// One record of the key (name) table (cfgbin.md §1.5).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Key {
    pub hash: u32,
    pub name: String,
}

/// A parsed T2B file.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct T2b {
    /// Flat entry list; see [`build_tree`] for the nested view.
    pub entries: Vec<Entry>,
    pub footer: Footer,
    pub string_dedup: StringDedup,
    #[serde(default)]
    pub hash_kind: HashKind,
    /// Header values of the parsed file (absent for documents built in memory).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub info: Option<T2bInfo>,
}

impl Default for T2b {
    fn default() -> Self {
        T2b {
            entries: Vec::new(),
            footer: Footer::default(),
            string_dedup: StringDedup::Exact,
            hash_kind: HashKind::Crc32,
            info: None,
        }
    }
}

struct RawRecord {
    hash: u32,
    types: Vec<u8>,
    data: usize,
}

/// Walk `count` records from `start` with value width `vlen` (cfgbin.md §1.3).
/// Returns `None` if the layout does not fit `end` exactly (≤ 15 bytes of padding left).
fn parse_records(
    d: &[u8],
    count: u32,
    start: usize,
    end: usize,
    vlen: usize,
) -> Option<Vec<RawRecord>> {
    let mut pos = start;
    let mut out = Vec::with_capacity((count as usize).min(end.saturating_sub(start) / 8));
    for _ in 0..count {
        if pos + 8 > end {
            return None;
        }
        let hash = bytes::u32_at(d, pos).ok()?;
        let n = d[pos + 4] as usize;
        pos += 5;
        let tb = d.get(pos..pos + n.div_ceil(4))?;
        pos += tb.len();
        let types: Vec<u8> = (0..n).map(|k| (tb[k / 4] >> ((k % 4) * 2)) & 3).collect();
        pos = align(pos, 4);
        if pos > end || types.contains(&3) {
            return None;
        }
        if pos + n * vlen > end {
            return None;
        }
        out.push(RawRecord {
            hash,
            types,
            data: pos,
        });
        pos += n * vlen;
    }
    if pos > end || end - pos >= 0x10 {
        return None;
    }
    Some(out)
}

impl T2b {
    /// Text encoding given by the footer.
    pub fn text_encoding(&self) -> TextEncoding {
        self.footer.text_encoding()
    }

    /// Parse a T2B file (cfgbin.md §1).
    pub fn parse(d: &[u8]) -> Result<T2b> {
        if d.len() < 0x30 || !is_t2b(d) {
            return Err(Error::NotT2b);
        }
        let ft = d.len() - 12;
        let footer = Footer {
            unk1: bytes::i16_at(d, ft)?,
            encoding: bytes::i16_at(d, ft + 2)?,
            unk2: bytes::i16_at(d, ft + 4)?,
        };
        let enc = footer.text_encoding();

        // --- entry section (§1.1 / §1.2) ---
        let e_count = bytes::u32_at(d, 0)?;
        let s_off = bytes::u32_at(d, 4)?;
        let s_len = bytes::u32_at(d, 8)?;
        let s_cnt = bytes::u32_at(d, 12)?;
        let s_off_u = s_off as usize;
        if s_off_u > d.len() || s_off_u < 0x10 {
            return Err(Error::Malformed(format!(
                "T2B string offset {s_off:#x} outside file"
            )));
        }
        let raw = if e_count == 0 {
            Vec::new()
        } else {
            match parse_records(d, e_count, 0x10, s_off_u, 4) {
                Some(r) => r,
                None if parse_records(d, e_count, 0x10, s_off_u, 8).is_some() => {
                    return Err(Error::UnsupportedValueWidth);
                }
                None => return Err(Error::T2bLayout),
            }
        };
        let s_end = s_off_u.saturating_add(s_len as usize).min(d.len());
        let vstr = &d[s_off_u..s_end];

        // --- key section (§1.5) ---
        let k_base = align_checked(s_off_u + s_len as usize, 16)?;
        let k_size = bytes::u32_at(d, k_base)?;
        let k_count = bytes::u32_at(d, k_base + 4)?;
        let k_soff = bytes::u32_at(d, k_base + 8)?;
        let k_ssize = bytes::u32_at(d, k_base + 12)?;
        let ks_start = (k_base + k_soff as usize).min(d.len());
        let ks_end = ks_start.saturating_add(k_ssize as usize).min(d.len());
        let kstr = &d[ks_start..ks_end];
        let mut keys: std::collections::HashMap<u32, String> = std::collections::HashMap::new();
        let mut first_off = None;
        let mut hash_kind = HashKind::Unknown;
        for i in 0..k_count as usize {
            let rec = k_base + 0x10 + i * 8;
            let h = bytes::u32_at(d, rec)?;
            let o = bytes::u32_at(d, rec + 4)?;
            let first = *first_off.get_or_insert(o);
            // CfgBinEditor normalises by the first offset (always 0 in practice)
            let rel = o
                .checked_sub(first)
                .ok_or_else(|| Error::Malformed(format!("key offset {o:#x} before first key")))?;
            let raw_name = bytes::cstr(kstr, rel as usize)?;
            if i == 0 {
                hash_kind = if hash::crc32(raw_name) == h {
                    HashKind::Crc32
                } else if hash::jamcrc(raw_name) == h {
                    HashKind::JamCrc
                } else {
                    HashKind::Unknown
                };
            }
            keys.insert(h, enc.decode(raw_name));
        }
        if k_count == 0 {
            hash_kind = HashKind::Crc32;
        }

        // --- values ---
        let mut entries = Vec::with_capacity(raw.len());
        let mut n_strings = 0usize;
        for (idx, r) in raw.iter().enumerate() {
            let mut values = Vec::with_capacity(r.types.len());
            for (k, &t) in r.types.iter().enumerate() {
                let p = r.data + k * 4;
                let v = match t {
                    TYPE_STRING => {
                        let iv = bytes::i32_at(d, p)?;
                        if iv < 0 {
                            Value::String(None)
                        } else {
                            n_strings += 1;
                            Value::String(Some(enc.decode(bytes::cstr(vstr, iv as usize)?)))
                        }
                    }
                    TYPE_INT => Value::Int(bytes::i32_at(d, p)?),
                    TYPE_FLOAT => Value::Float(bytes::f32_at(d, p)?),
                    ty => return Err(Error::Malformed(format!("value type {ty} in entry {idx}"))),
                };
                values.push(v);
            }
            entries.push(Entry {
                name: keys.get(&r.hash).cloned(),
                hash: r.hash,
                values,
            });
        }

        // --- pooling style (§1.4) ---
        let string_dedup = if s_cnt as usize == n_strings && has_duplicate_strings(&entries) {
            StringDedup::None
        } else {
            StringDedup::Exact
        };

        Ok(T2b {
            entries,
            footer,
            string_dedup,
            hash_kind,
            info: Some(T2bInfo {
                entry_count: e_count,
                string_offset: s_off,
                string_length: s_len,
                string_count: s_cnt,
                key_section: k_base as u32,
                key_size: k_size,
                key_count: k_count,
                key_string_offset: k_soff,
                key_string_size: k_ssize,
                file_size: d.len() as u32,
            }),
        })
    }

    /// Serialise with the canonical game layout (cfgbin.md §1.7). Byte-exact for unmodified game files.
    pub fn to_bytes(&self) -> Result<Vec<u8>> {
        let enc = self.text_encoding();
        let mut body = Vec::with_capacity(self.entries.len() * 16);
        let mut pool = StringPool::new(self.string_dedup == StringDedup::Exact);
        for e in &self.entries {
            let n = e.values.len();
            if n > 255 {
                return Err(Error::overflow("T2B values per entry", n));
            }
            body.extend_from_slice(&e.hash.to_le_bytes());
            body.push(n as u8);
            for chunk in e.values.chunks(4) {
                let b = chunk
                    .iter()
                    .enumerate()
                    .fold(0u8, |b, (k, v)| b | (v.type_code() << (k * 2)));
                body.push(b);
            }
            // records start at 0x10, so body-relative alignment == absolute alignment
            pad_to(&mut body, 4, 0xFF);
            for v in &e.values {
                let word: [u8; 4] = match v {
                    Value::String(None) => (-1i32).to_le_bytes(),
                    Value::String(Some(s)) => {
                        let off = pool.add(&enc.encode(s)?);
                        i32::try_from(off)
                            .map_err(|_| Error::overflow("T2B string offset", off))?
                            .to_le_bytes()
                    }
                    Value::Int(i) => i.to_le_bytes(),
                    Value::Float(f) => f.to_le_bytes(),
                };
                body.extend_from_slice(&word);
            }
        }
        let s_off = align(0x10 + body.len(), 16);
        let mut out = Vec::with_capacity(s_off + pool.buf.len() + 256);
        for v in [self.entries.len(), s_off, pool.buf.len(), pool.count] {
            let v = u32::try_from(v).map_err(|_| Error::overflow("T2B header field", v))?;
            out.extend_from_slice(&v.to_le_bytes());
        }
        out.extend_from_slice(&body);
        out.resize(s_off, 0xFF);
        out.extend_from_slice(&pool.buf);
        pad_to(&mut out, 16, 0xFF);

        // --- key section: distinct names in order of first appearance (§1.5) ---
        let keys = self.key_table();
        let mut kpool = StringPool::new(true);
        let mut krecs = Vec::with_capacity(keys.len() * 8);
        for k in &keys {
            let off = kpool.add(&enc.encode(&k.name)?);
            krecs.extend_from_slice(&k.hash.to_le_bytes());
            krecs.extend_from_slice(&(off as u32).to_le_bytes());
        }
        let k_soff = align(0x10 + krecs.len(), 16);
        let k_size = k_soff + align(kpool.buf.len(), 16);
        let k_base = out.len();
        for v in [k_size, keys.len(), k_soff, kpool.buf.len()] {
            let v = u32::try_from(v).map_err(|_| Error::overflow("T2B key header field", v))?;
            out.extend_from_slice(&v.to_le_bytes());
        }
        out.extend_from_slice(&krecs);
        out.resize(k_base + k_soff, 0xFF);
        out.extend_from_slice(&kpool.buf);
        out.resize(k_base + k_size, 0xFF);

        // --- footer (§1.6) ---
        out.extend_from_slice(&MAGIC);
        out.extend_from_slice(&self.footer.unk1.to_le_bytes());
        out.extend_from_slice(&self.footer.encoding.to_le_bytes());
        out.extend_from_slice(&self.footer.unk2.to_le_bytes());
        out.extend_from_slice(&[0xFF; 6]);
        Ok(out)
    }

    /// The key table the writer will emit: one record per distinct hash of a named entry,
    /// in order of first appearance (cfgbin.md §1.5).
    pub fn key_table(&self) -> Vec<Key> {
        let mut seen = std::collections::HashSet::new();
        self.entries
            .iter()
            .filter_map(|e| {
                let name = e.name.as_ref()?;
                seen.insert(e.hash).then(|| Key {
                    hash: e.hash,
                    name: name.clone(),
                })
            })
            .collect()
    }

    /// Hash `name` the way this file does (file encoding + detected hash kind).
    pub fn hash_name(&self, name: &str) -> Result<u32> {
        Ok(self.hash_kind.hash(&self.text_encoding().encode(name)?))
    }

    /// Create an entry whose hash matches `name` in this file's encoding.
    pub fn new_entry(&self, name: &str, values: Vec<Value>) -> Result<Entry> {
        Ok(Entry {
            name: Some(name.to_owned()),
            hash: self.hash_name(name)?,
            values,
        })
    }

    /// Rename entry `index`, updating its hash.
    pub fn rename_entry(&mut self, index: usize, name: &str) -> Result<()> {
        let hash = self.hash_name(name)?;
        let e = self
            .entries
            .get_mut(index)
            .ok_or_else(|| Error::Malformed(format!("entry index {index} out of range")))?;
        e.name = Some(name.to_owned());
        e.hash = hash;
        Ok(())
    }

    /// Nested view of the entries (cfgbin.md §2.3), counted mode.
    pub fn tree(&self) -> Vec<Node> {
        build_tree(&self.entries, TreeMode::Counted)
    }

    /// Counted lists declared `X_LIST_BEG(count, 1)` with their detected sort key
    /// (cfgbin.md §2.2). Call this right after loading, before editing, and pass the result
    /// to [`rebuild_sort_indexes_with`](Self::rebuild_sort_indexes_with).
    pub fn sorted_lists(&self) -> Vec<SortedList> {
        tree::sorted_lists(self)
    }

    /// Regenerate every `__SORT_INDEX` run of counted lists declared `X_LIST_BEG(count, 1)`
    /// (cfgbin.md §2.2): one entry per row, holding the row indices ordered by ascending
    /// **unsigned** key value (stable). Missing or surplus `__SORT_INDEX` entries are
    /// inserted/removed. Returns the number of lists rebuilt.
    ///
    /// The key field of each list is detected from its current index (see
    /// [`sorted_lists`](Self::sorted_lists)); this works as long as the existing index still
    /// describes the first rows (e.g. rows were appended, or non-key values changed). For
    /// arbitrary edits prefer [`rebuild_sort_indexes_with`](Self::rebuild_sort_indexes_with).
    ///
    /// Update `X_LIST_BEG.values[0]` (the row count) and any `*_REF_*` slices before calling this.
    pub fn rebuild_sort_indexes(&mut self) -> Result<usize> {
        tree::rebuild_sort_indexes(self, None)
    }

    /// Like [`rebuild_sort_indexes`](Self::rebuild_sort_indexes), with key fields taken from
    /// `lists` (as returned by [`sorted_lists`](Self::sorted_lists) before editing; matched
    /// by BEGIN name and occurrence order). Lists not found there use field 0.
    pub fn rebuild_sort_indexes_with(&mut self, lists: &[SortedList]) -> Result<usize> {
        tree::rebuild_sort_indexes(self, Some(lists))
    }
}

fn has_duplicate_strings(entries: &[Entry]) -> bool {
    let mut seen = std::collections::HashSet::new();
    entries
        .iter()
        .flat_map(|e| e.values.iter())
        .filter_map(Value::as_str)
        .any(|s| !seen.insert(s))
}

#[cfg(test)]
mod tests;
