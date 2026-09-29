//! RDBN cfg.bin — self-describing typed database (302 Victory Road files).
//!
//! Byte-level spec: `docs/formats/rdbn.md`. Reference implementation: `read_rdbn` /
//! `write_rdbn` in `tools/py/cfgbin.py`, ported faithfully, including:
//!
//! * natural field alignment (§5.1) and the "arrays count as one" stored-offset quirk (§5.2);
//! * signed-byte field type 8 and embedded records (category 2, §5.1);
//! * list placement relative to the unaligned values base (§6.1);
//! * u16 sort indexes, regenerated on write (§7);
//! * exact-dedup UTF-8 string pool in game order (§8).
//!
//! Unlike the Python reference, unreferenced bytes at the end of the string pool are kept
//! ([`Rdbn::trailing_strings`]), which also makes `soccer_common_text.cfg.bin` (§9) round-trip.

mod view;

pub use view::{ColumnView, TableView};

use std::cmp::Ordering;

use serde::{Deserialize, Serialize};

use crate::bytes::{self, StringPool, align};
use crate::error::{Error, Result};
use crate::text::TextEncoding;

/// File magic `RDBN` at offset 0 (rdbn.md §2).
pub const MAGIC: [u8; 4] = *b"RDBN";
/// Header size / data base used by every VR file.
pub const DATA_BASE: usize = 0x50;
/// Header `version` of every VR file.
pub const VERSION: u32 = 0x64;

/// Maximum nesting of embedded records (category 2) accepted by the reader.
const MAX_NESTING: usize = 16;

/// Does `data` start with the RDBN magic?
pub fn is_rdbn(data: &[u8]) -> bool {
    data.len() >= 4 && data[..4] == MAGIC
}

/// Field type ids (rdbn.md §5.1; names from CfgBinEditor's `FieldType`).
pub mod field_type {
    /// u8 0/1
    pub const BOOL: i16 = 3;
    /// u8
    pub const BYTE: i16 = 4;
    /// i16
    pub const SHORT: i16 = 5;
    /// i32
    pub const INT: i16 = 6;
    /// s8 (missing from CfgBinEditor)
    pub const SBYTE: i16 = 8;
    /// i16
    pub const ACT_TYPE: i16 = 9;
    /// i32
    pub const FLAG: i16 = 10;
    /// f32
    pub const FLOAT: i16 = 13;
    /// u32 CRC-32 ID
    pub const HASH: i16 = 15;
    /// 2 × f32
    pub const POSITION_2D: i16 = 17;
    /// 4 × f32
    pub const RATE_MATRIX: i16 = 18;
    /// 4 × f32
    pub const POSITION: i16 = 19;
    /// u32 string-pool offset
    pub const STRING: i16 = 20;
    /// (i16 start, i16 count)
    pub const DATA_TUPLE: i16 = 21;
}

/// Field category (rdbn.md §5).
pub mod category {
    /// Primitive value.
    pub const PRIMITIVE: i16 = 1;
    /// Embedded record: the field type is an index into the type table.
    pub const NESTED: i16 = 2;
    /// 4-byte/composite value.
    pub const COMPOSITE: i16 = 3;
}

/// Human-readable name of a field type id.
pub fn field_type_name(ty: i16) -> &'static str {
    match ty {
        0 => "AbilityData",
        1 => "EnhanceData",
        2 => "StatusRate",
        3 => "Bool",
        4 => "Byte",
        5 => "Short",
        6 => "Int",
        8 => "SByte",
        9 => "ActType",
        10 => "Flag",
        13 => "Float",
        15 => "Hash",
        17 => "Position2D",
        18 => "RateMatrix",
        19 => "Position",
        20 => "String",
        21 => "DataTuple",
        _ => "Unknown",
    }
}

/// Size in bytes of a primitive/composite field type, if known.
fn natural_size(ty: i16) -> Option<i32> {
    Some(match ty {
        3 | 4 | 8 => 1,
        5 | 9 => 2,
        6 | 10 | 13 | 15 | 20 | 21 => 4,
        17 => 8,
        18 | 19 => 16,
        _ => return None,
    })
}

/// A field declaration (rdbn.md §5).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Field {
    pub name: String,
    /// Stored name hash (written verbatim).
    pub hash: u32,
    /// Primitive type id, or type-table index when `category == 2`.
    #[serde(rename = "type")]
    pub ty: i16,
    pub category: i16,
    /// Element size in bytes.
    pub size: i32,
    /// Array length (≥ 1).
    pub count: u32,
    /// `+0x0C` as read (the quirky "stored" offset, informational; recomputed on write).
    #[serde(default)]
    pub stored_offset: i32,
}

impl Field {
    /// Alignment inside a record (rdbn.md §5.1): nested records align to 4.
    pub fn align(&self) -> usize {
        if self.category == category::NESTED {
            return 4;
        }
        match self.ty {
            3 | 4 | 8 => 1,
            5 | 9 | 21 => 2,
            18 | 19 => 16,
            _ => 4,
        }
    }

    /// Is this an embedded record (category 2)?
    pub fn is_nested(&self) -> bool {
        self.category == category::NESTED
    }

    /// Human-readable type name.
    pub fn type_name(&self) -> &'static str {
        if self.is_nested() {
            "Struct"
        } else {
            field_type_name(self.ty)
        }
    }
}

/// A type (struct) declaration (rdbn.md §4).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Type {
    pub name: String,
    pub hash: u32,
    /// `+0x04`, unknown schema hash — preserved verbatim.
    pub unk_hash: u32,
    pub fields: Vec<Field>,
}

/// Record layout of a type (rdbn.md §5.2).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Layout {
    /// Real byte offset of each field (arrays contiguous).
    pub real_offsets: Vec<usize>,
    /// Offset written in the field table (arrays counted as length 1).
    pub stored_offsets: Vec<i32>,
    /// Record size (aligned to `align`).
    pub size: usize,
    /// Record alignment (max field alignment).
    pub align: usize,
}

/// Compute the record layout of `fields` (rdbn.md §5.2).
pub fn layout(fields: &[Field]) -> Result<Layout> {
    let (mut real, mut stored, mut hi) = (0usize, 0usize, 1usize);
    let mut real_offsets = Vec::with_capacity(fields.len());
    let mut stored_offsets = Vec::with_capacity(fields.len());
    for f in fields {
        let a = f.align();
        hi = hi.max(a);
        real = align(real, a);
        stored = align(stored, a);
        real_offsets.push(real);
        stored_offsets
            .push(i32::try_from(stored).map_err(|_| Error::overflow("RDBN field offset", stored))?);
        let size =
            usize::try_from(f.size).map_err(|_| Error::overflow("RDBN field size", f.size))?;
        let total = size
            .checked_mul(f.count as usize)
            .and_then(|t| real.checked_add(t))
            .filter(|&t| t <= i32::MAX as usize)
            .ok_or_else(|| Error::overflow("RDBN record size", f.size as i64 * f.count as i64))?;
        real = total;
        stored += size;
    }
    Ok(Layout {
        real_offsets,
        stored_offsets,
        size: align(real, hi),
        align: hi,
    })
}

/// A field element value. JSON: `{"type": "hash", "value": 123}`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", content = "value", rename_all = "camelCase")]
pub enum Value {
    /// Type 3.
    Bool(bool),
    /// Type 4.
    Byte(u8),
    /// Type 8.
    SByte(i8),
    /// Types 5 and 9.
    Short(i16),
    /// Types 6 and 10.
    Int(i32),
    /// Type 13.
    Float(f32),
    /// Type 15 — CRC-32 ID / foreign key.
    Hash(u32),
    /// Type 17.
    Vec2([f32; 2]),
    /// Types 18 and 19.
    Vec4([f32; 4]),
    /// Type 20; `None` is `0xFFFFFFFF`.
    String(Option<String>),
    /// Type 20 whose offset points past the end of the file (kept verbatim).
    DanglingString(u32),
    /// Type 21 — `(start, count)` slice of another list.
    Tuple([i16; 2]),
    /// Category 2 — embedded record of `types[field.ty]`: `[field][element]`.
    Struct(Vec<Vec<Value>>),
    /// Unknown type or size mismatch: raw bytes of the element.
    Raw(Vec<u8>),
}

/// One record: `row[field][element]` (arrays have `count` elements, scalars one).
pub type Row = Vec<Vec<Value>>;

/// A list ("root") of records of one type (rdbn.md §6).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct List {
    pub name: String,
    pub hash: u32,
    /// Index into [`Rdbn::types`].
    pub type_index: usize,
    /// `+0x02`, always 2.
    pub unk1: i16,
    /// `+0x18`: a u16 sort index follows the records (regenerated on write).
    pub indexed: bool,
    /// Field the sort index is ordered by (detected on read, rdbn.md §7).
    #[serde(default)]
    pub key_field: usize,
    /// Sort index as read (informational).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sort_index: Option<Vec<u16>>,
    pub rows: Vec<Row>,
}

/// A parsed RDBN file.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Rdbn {
    /// Header `+0x06` (0x64 in VR).
    pub version: u32,
    /// Type declarations, in file order (orphans included — indices matter).
    pub types: Vec<Type>,
    pub lists: Vec<List>,
    /// Unreferenced bytes at the end of the string pool (rdbn.md §9), kept for byte-exactness.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub trailing_strings: Vec<u8>,
}

impl Default for Rdbn {
    fn default() -> Self {
        Rdbn {
            version: VERSION,
            types: Vec::new(),
            lists: Vec::new(),
            trailing_strings: Vec::new(),
        }
    }
}

const UTF8: TextEncoding = TextEncoding::Utf8;

/// Resolve `base + (word << 2)` from an i16 header field.
fn word_offset(base: usize, word: i16, what: &'static str) -> Result<usize> {
    if word < 0 {
        return Err(Error::overflow(what, word));
    }
    Ok(base + ((word as usize) << 2))
}

fn to_usize(v: i64, what: &'static str) -> Result<usize> {
    usize::try_from(v).map_err(|_| Error::overflow(what, v))
}

struct Reader<'a> {
    d: &'a [u8],
    sbase: usize,
    types: &'a [Type],
    layouts: &'a [Layout],
}

impl Reader<'_> {
    fn value(&self, pos: usize, f: &Field, depth: usize) -> Result<Value> {
        let d = self.d;
        let ty = f.ty;
        if f.is_nested() && ty >= 0 && (ty as usize) < self.types.len() {
            if depth >= MAX_NESTING {
                return Err(Error::Malformed(
                    "RDBN embedded records nest too deeply".into(),
                ));
            }
            let sub = &self.types[ty as usize];
            let lay = &self.layouts[ty as usize];
            let mut out = Vec::with_capacity(sub.fields.len());
            for (i, sf) in sub.fields.iter().enumerate() {
                out.push(self.elements(pos + lay.real_offsets[i], sf, depth + 1)?);
            }
            return Ok(Value::Struct(out));
        }
        if f.is_nested() || natural_size(ty) != Some(f.size) {
            let size = to_usize(f.size as i64, "RDBN field size")?;
            return Ok(Value::Raw(bytes::get(d, pos, size)?.to_vec()));
        }
        Ok(match ty {
            3 => Value::Bool(bytes::u8_at(d, pos)? != 0),
            4 => Value::Byte(bytes::u8_at(d, pos)?),
            8 => Value::SByte(bytes::u8_at(d, pos)? as i8),
            5 | 9 => Value::Short(bytes::i16_at(d, pos)?),
            6 | 10 => Value::Int(bytes::i32_at(d, pos)?),
            13 => Value::Float(bytes::f32_at(d, pos)?),
            15 => Value::Hash(bytes::u32_at(d, pos)?),
            17 => Value::Vec2([bytes::f32_at(d, pos)?, bytes::f32_at(d, pos + 4)?]),
            18 | 19 => Value::Vec4([
                bytes::f32_at(d, pos)?,
                bytes::f32_at(d, pos + 4)?,
                bytes::f32_at(d, pos + 8)?,
                bytes::f32_at(d, pos + 12)?,
            ]),
            20 => {
                let o = bytes::u32_at(d, pos)?;
                if o == u32::MAX {
                    Value::String(None)
                } else if self.sbase + o as usize >= d.len() {
                    Value::DanglingString(o)
                } else {
                    Value::String(Some(UTF8.decode(bytes::cstr(d, self.sbase + o as usize)?)))
                }
            }
            21 => Value::Tuple([bytes::i16_at(d, pos)?, bytes::i16_at(d, pos + 2)?]),
            // natural_size() only accepts the types above; keep anything else verbatim
            _ => Value::Raw(bytes::get(d, pos, f.size as usize)?.to_vec()),
        })
    }

    fn elements(&self, pos: usize, f: &Field, depth: usize) -> Result<Vec<Value>> {
        let size = to_usize(f.size as i64, "RDBN field size")?;
        if f.count as usize > self.d.len().max(1) {
            return Err(Error::overflow("RDBN field count", f.count));
        }
        (0..f.count as usize)
            .map(|k| self.value(pos + k * size, f, depth))
            .collect()
    }
}

impl Rdbn {
    /// Parse an RDBN file (rdbn.md §2–§8).
    pub fn parse(d: &[u8]) -> Result<Rdbn> {
        if d.len() < 0x3C || !is_rdbn(d) {
            return Err(Error::NotRdbn);
        }
        let version = bytes::u32_at(d, 6)?;
        let base = (bytes::u16_at(d, 10)? as usize) << 2;
        let h = |i: usize| bytes::i16_at(d, 0x24 + 2 * i);
        let type_off = word_offset(base, h(0)?, "RDBN type offset")?;
        let type_cnt = h(1)?.max(0) as usize;
        let field_off = word_offset(base, h(2)?, "RDBN field offset")?;
        let field_cnt = h(3)?.max(0) as usize;
        let root_off = word_offset(base, h(4)?, "RDBN list offset")?;
        let root_cnt = h(5)?.max(0) as usize;
        let hash_off = word_offset(base, h(6)?, "RDBN hash offset")?;
        let soff_off = word_offset(base, h(7)?, "RDBN name offset table")?;
        let hash_cnt = h(8)?.max(0) as usize;
        let value_off = word_offset(base, h(9)?, "RDBN value offset")?;
        let string_rel = bytes::i32_at(d, 0x38)?;
        let sbase = base + to_usize(string_rel as i64, "RDBN string offset")?;

        // name table (§3)
        let mut names = std::collections::HashMap::with_capacity(hash_cnt);
        for i in 0..hash_cnt {
            let hsh = bytes::u32_at(d, hash_off + 4 * i)?;
            let off = bytes::i32_at(d, soff_off + 4 * i)?;
            let off = to_usize(off as i64, "RDBN name offset")?;
            names.insert(hsh, UTF8.decode(bytes::cstr(d, sbase + off)?));
        }
        let nm = |hsh: u32| {
            names
                .get(&hsh)
                .cloned()
                .unwrap_or_else(|| format!("#{hsh:08X}"))
        };

        // fields (§5)
        let mut raw_fields = Vec::with_capacity(field_cnt);
        for i in 0..field_cnt {
            let p = field_off + i * 0x20;
            let hash = bytes::u32_at(d, p)?;
            raw_fields.push(Field {
                name: nm(hash),
                hash,
                ty: bytes::i16_at(d, p + 4)?,
                category: bytes::i16_at(d, p + 6)?,
                size: bytes::i32_at(d, p + 8)?,
                stored_offset: bytes::i32_at(d, p + 12)?,
                count: bytes::u32_at(d, p + 16)?,
            });
        }
        // types (§4)
        let mut types = Vec::with_capacity(type_cnt);
        for i in 0..type_cnt {
            let p = type_off + i * 0x20;
            let hash = bytes::u32_at(d, p)?;
            let unk_hash = bytes::u32_at(d, p + 4)?;
            let fi = bytes::i16_at(d, p + 8)?;
            let fc = bytes::i16_at(d, p + 10)?;
            let fields = usize::try_from(fi)
                .ok()
                .zip(usize::try_from(fc).ok())
                .and_then(|(a, n)| raw_fields.get(a..a + n))
                .ok_or_else(|| {
                    Error::Malformed(format!(
                        "RDBN type {i}: field range {fi}+{fc} out of bounds"
                    ))
                })?
                .to_vec();
            types.push(Type {
                name: nm(hash),
                hash,
                unk_hash,
                fields,
            });
        }
        let layouts = types
            .iter()
            .map(|t| layout(&t.fields))
            .collect::<Result<Vec<_>>>()?;
        let reader = Reader {
            d,
            sbase,
            types: &types,
            layouts: &layouts,
        };

        // lists (§6)
        let mut lists = Vec::with_capacity(root_cnt);
        for i in 0..root_cnt {
            let p = root_off + i * 0x20;
            let ti = bytes::i16_at(d, p)?;
            let unk1 = bytes::i16_at(d, p + 2)?;
            let vo = bytes::i32_at(d, p + 4)? as i64;
            let vs = bytes::i32_at(d, p + 8)? as i64;
            let vc = bytes::i32_at(d, p + 12)?;
            let hash = bytes::u32_at(d, p + 16)?;
            let io = bytes::u32_at(d, p + 20)?;
            let iflag = bytes::u32_at(d, p + 24)?;
            let type_index = usize::try_from(ti)
                .ok()
                .filter(|&t| t < types.len())
                .ok_or_else(|| {
                    Error::Malformed(format!("RDBN list {i}: type index {ti} out of range"))
                })?;
            let vc = to_usize(vc as i64, "RDBN record count")?;
            if vc > d.len() {
                return Err(Error::overflow("RDBN record count", vc));
            }
            let typ = &types[type_index];
            let lay = &layouts[type_index];
            let mut rows = Vec::with_capacity(vc);
            for j in 0..vc {
                let rbase = to_usize(value_off as i64 + vo + j as i64 * vs, "RDBN record offset")?;
                let mut row = Vec::with_capacity(typ.fields.len());
                for (fi, f) in typ.fields.iter().enumerate() {
                    row.push(reader.elements(rbase + lay.real_offsets[fi], f, 0)?);
                }
                rows.push(row);
            }
            let mut list = List {
                name: nm(hash),
                hash,
                type_index,
                unk1,
                indexed: false,
                key_field: 0,
                sort_index: None,
                rows,
            };
            if iflag != 0 {
                let ip = value_off + io as usize;
                let idx = (0..vc)
                    .map(|k| bytes::u16_at(d, ip + 2 * k))
                    .collect::<Result<Vec<_>>>()?;
                list.indexed = true;
                list.key_field = detect_key_field(&list, &idx, typ);
                list.sort_index = Some(idx);
            }
            lists.push(list);
        }

        let mut doc = Rdbn {
            version,
            types,
            lists,
            trailing_strings: Vec::new(),
        };
        // Keep unreferenced pool bytes (rdbn.md §9): if the real pool extends the pool we would
        // write, remember the extra tail.
        if let Ok((rebuilt, pool_start)) = doc.write_inner() {
            let ours = &rebuilt[pool_start..];
            if let Some(theirs) = d.get(sbase..)
                && theirs.len() > ours.len()
                && theirs.starts_with(ours)
            {
                doc.trailing_strings = theirs[ours.len()..].to_vec();
            }
        }
        Ok(doc)
    }

    /// Serialise the way Level-5's tool does (byte-exact for 302/302 VR files).
    pub fn to_bytes(&self) -> Result<Vec<u8>> {
        self.write_inner().map(|(b, _)| b)
    }

    /// Layout of every type, in type-table order.
    pub fn layouts(&self) -> Result<Vec<Layout>> {
        self.types.iter().map(|t| layout(&t.fields)).collect()
    }

    /// Type of a list.
    pub fn list_type(&self, list: &List) -> Option<&Type> {
        self.types.get(list.type_index)
    }

    /// Stable permutation of `list`'s rows by its key field, ascending unsigned (rdbn.md §7).
    pub fn sort_index(&self, list: &List) -> Vec<usize> {
        let mut perm: Vec<usize> = (0..list.rows.len()).collect();
        let keys: Vec<Key<'_>> = list
            .rows
            .iter()
            .map(|r| top_key(r.get(list.key_field)))
            .collect();
        perm.sort_by(|&a, &b| keys[a].cmp(&keys[b]));
        perm
    }

    /// Returns the file bytes and the offset of the string pool.
    fn write_inner(&self) -> Result<(Vec<u8>, usize)> {
        let types = &self.types;
        let nfields: usize = types.iter().map(|t| t.fields.len()).sum();
        let hash_cnt = self.lists.len() + types.len() + nfields;
        let i16c =
            |v: usize, what: &'static str| i16::try_from(v).map_err(|_| Error::overflow(what, v));
        i16c(hash_cnt, "RDBN name count")?;
        let layouts = self.layouts()?;

        let value_off =
            DATA_BASE + (types.len() + nfields + self.lists.len()) * 0x20 + hash_cnt * 8;
        // list placement (§6.1)
        struct Root {
            start: usize,
            size: usize,
            index_off: usize,
        }
        let mut roots = Vec::with_capacity(self.lists.len());
        let mut loc = value_off;
        for l in &self.lists {
            let lay = layouts.get(l.type_index).ok_or_else(|| {
                Error::Malformed(format!(
                    "list {}: type index {} out of range",
                    l.name, l.type_index
                ))
            })?;
            loc = align(loc, lay.align);
            let start = loc - value_off;
            loc += l.rows.len() * lay.size;
            let mut index_off = 0;
            if l.indexed {
                index_off = loc - value_off;
                loc += 2 * l.rows.len();
            }
            roots.push(Root {
                start,
                size: lay.size,
                index_off,
            });
        }
        let value_end = loc;

        // string pool (§8): type names + field names, then per list: name, then values
        let mut pool = StringPool::new(true);
        for t in types {
            pool.add(&UTF8.encode(&t.name)?);
            for f in &t.fields {
                pool.add(&UTF8.encode(&f.name)?);
            }
        }
        let mut buf = vec![0u8; value_end - value_off];
        let packer = Packer {
            types,
            layouts: &layouts,
        };
        for (l, r) in self.lists.iter().zip(&roots) {
            pool.add(&UTF8.encode(&l.name)?);
            let t = &types[l.type_index];
            let lay = &layouts[l.type_index];
            for (j, row) in l.rows.iter().enumerate() {
                if row.len() != t.fields.len() {
                    return Err(Error::TypeMismatch(format!(
                        "list {} row {j}: {} fields, type {} has {}",
                        l.name,
                        row.len(),
                        t.name,
                        t.fields.len()
                    )));
                }
                let rec = r.start + j * r.size;
                packer.fields(
                    &mut buf[rec..rec + r.size],
                    &t.fields,
                    &lay.real_offsets,
                    row,
                    &mut pool,
                    0,
                )?;
            }
            if l.indexed {
                let perm = self.sort_index(l);
                for (k, &p) in perm.iter().enumerate() {
                    let p = u16::try_from(p).map_err(|_| Error::overflow("RDBN sort index", p))?;
                    let at = r.index_off + 2 * k;
                    buf[at..at + 2].copy_from_slice(&p.to_le_bytes());
                }
            }
        }

        let mut out = Vec::with_capacity(value_end + pool.buf.len() + self.trailing_strings.len());
        out.resize(DATA_BASE, 0);
        let mut fidx = 0usize;
        for t in types {
            out.extend_from_slice(&t.hash.to_le_bytes());
            out.extend_from_slice(&t.unk_hash.to_le_bytes());
            out.extend_from_slice(&i16c(fidx, "RDBN field index")?.to_le_bytes());
            out.extend_from_slice(&i16c(t.fields.len(), "RDBN field count")?.to_le_bytes());
            out.extend_from_slice(&[0; 0x14]);
            fidx += t.fields.len();
        }
        for (t, lay) in types.iter().zip(&layouts) {
            for (f, stored) in t.fields.iter().zip(&lay.stored_offsets) {
                out.extend_from_slice(&f.hash.to_le_bytes());
                out.extend_from_slice(&f.ty.to_le_bytes());
                out.extend_from_slice(&f.category.to_le_bytes());
                out.extend_from_slice(&f.size.to_le_bytes());
                out.extend_from_slice(&stored.to_le_bytes());
                out.extend_from_slice(&f.count.to_le_bytes());
                out.extend_from_slice(&[0; 0xC]);
            }
        }
        let i32c =
            |v: usize, what: &'static str| i32::try_from(v).map_err(|_| Error::overflow(what, v));
        for (l, r) in self.lists.iter().zip(&roots) {
            out.extend_from_slice(&i16c(l.type_index, "RDBN type index")?.to_le_bytes());
            out.extend_from_slice(&l.unk1.to_le_bytes());
            out.extend_from_slice(&i32c(r.start, "RDBN list offset")?.to_le_bytes());
            out.extend_from_slice(&i32c(r.size, "RDBN record size")?.to_le_bytes());
            out.extend_from_slice(&i32c(l.rows.len(), "RDBN record count")?.to_le_bytes());
            out.extend_from_slice(&l.hash.to_le_bytes());
            out.extend_from_slice(&i32c(r.index_off, "RDBN sort index offset")?.to_le_bytes());
            out.extend_from_slice(&u32::from(l.indexed).to_le_bytes());
            out.extend_from_slice(&[0; 4]);
        }
        // name tables (§3): lists, then each type followed by its fields
        let mut name_refs: Vec<(u32, &str)> = Vec::with_capacity(hash_cnt);
        name_refs.extend(self.lists.iter().map(|l| (l.hash, l.name.as_str())));
        for t in types {
            name_refs.push((t.hash, &t.name));
            name_refs.extend(t.fields.iter().map(|f| (f.hash, f.name.as_str())));
        }
        for (h, _) in &name_refs {
            out.extend_from_slice(&h.to_le_bytes());
        }
        for (_, n) in &name_refs {
            let off = pool
                .offset_of(&UTF8.encode(n)?)
                .ok_or_else(|| Error::Malformed(format!("name {n:?} missing from string pool")))?;
            out.extend_from_slice(&i32c(off, "RDBN name offset")?.to_le_bytes());
        }
        debug_assert_eq!(out.len(), value_off);
        out.extend_from_slice(&buf);
        let pool_start = out.len();
        out.extend_from_slice(&pool.buf);
        out.extend_from_slice(&self.trailing_strings);

        // header (§2)
        let o_fields = types.len() * 0x20;
        let o_roots = o_fields + nfields * 0x20;
        let o_hash = o_roots + self.lists.len() * 0x20;
        let o_soff = o_hash + hash_cnt * 4;
        let data_size = u32::try_from(out.len() - DATA_BASE)
            .map_err(|_| Error::overflow("RDBN data size", out.len()))?;
        out[0..4].copy_from_slice(&MAGIC);
        out[4..6].copy_from_slice(&(DATA_BASE as u16).to_le_bytes());
        out[6..10].copy_from_slice(&self.version.to_le_bytes());
        out[10..12].copy_from_slice(&((DATA_BASE >> 2) as u16).to_le_bytes());
        out[12..16].copy_from_slice(&data_size.to_le_bytes());
        let words = [
            0,
            types.len(),
            o_fields >> 2,
            nfields,
            o_roots >> 2,
            self.lists.len(),
            o_hash >> 2,
            o_soff >> 2,
            hash_cnt,
            (value_off - DATA_BASE) >> 2,
        ];
        for (i, w) in words.into_iter().enumerate() {
            let at = 0x24 + 2 * i;
            out[at..at + 2].copy_from_slice(&i16c(w, "RDBN header word")?.to_le_bytes());
        }
        out[0x38..0x3C]
            .copy_from_slice(&i32c(pool_start - DATA_BASE, "RDBN string offset")?.to_le_bytes());
        Ok((out, pool_start))
    }
}

struct Packer<'a> {
    types: &'a [Type],
    layouts: &'a [Layout],
}

impl Packer<'_> {
    /// Pack a record (or embedded record) of `fields` into `rec`.
    fn fields(
        &self,
        rec: &mut [u8],
        fields: &[Field],
        offsets: &[usize],
        row: &[Vec<Value>],
        pool: &mut StringPool,
        depth: usize,
    ) -> Result<()> {
        for ((f, &off), elems) in fields.iter().zip(offsets).zip(row) {
            if elems.len() != f.count as usize {
                return Err(Error::TypeMismatch(format!(
                    "field {}: {} elements, declared count {}",
                    f.name,
                    elems.len(),
                    f.count
                )));
            }
            let size = f.size as usize;
            for (k, v) in elems.iter().enumerate() {
                let p = off + k * size;
                let slot = rec.get_mut(p..p + size).ok_or_else(|| {
                    Error::Malformed(format!("field {} outside its record", f.name))
                })?;
                self.value(slot, f, v, pool, depth)?;
            }
        }
        Ok(())
    }

    fn value(
        &self,
        slot: &mut [u8],
        f: &Field,
        v: &Value,
        pool: &mut StringPool,
        depth: usize,
    ) -> Result<()> {
        let mismatch = || {
            Error::TypeMismatch(format!(
                "field {} ({}) cannot hold {v:?}",
                f.name,
                f.type_name()
            ))
        };
        let mut put = |b: &[u8]| -> Result<()> {
            if b.len() != slot.len() {
                return Err(mismatch());
            }
            slot.copy_from_slice(b);
            Ok(())
        };
        match v {
            Value::Struct(sub) => {
                let ti = usize::try_from(f.ty)
                    .ok()
                    .filter(|&t| f.is_nested() && t < self.types.len());
                let ti = ti.ok_or_else(mismatch)?;
                if depth >= MAX_NESTING {
                    return Err(Error::Malformed("embedded records nest too deeply".into()));
                }
                let t = &self.types[ti];
                if sub.len() != t.fields.len() {
                    return Err(mismatch());
                }
                slot.fill(0);
                self.fields(
                    slot,
                    &t.fields,
                    &self.layouts[ti].real_offsets,
                    sub,
                    pool,
                    depth + 1,
                )
            }
            Value::Raw(b) => {
                let n = b.len().min(slot.len());
                slot[..n].copy_from_slice(&b[..n]);
                slot[n..].fill(0);
                Ok(())
            }
            Value::Bool(b) => put(&[u8::from(*b)]),
            Value::Byte(b) => put(&[*b]),
            Value::SByte(b) => put(&b.to_le_bytes()),
            Value::Short(s) => put(&s.to_le_bytes()),
            Value::Int(i) => put(&i.to_le_bytes()),
            Value::Float(x) => put(&x.to_le_bytes()),
            Value::Hash(h) => put(&h.to_le_bytes()),
            Value::Vec2(a) => put(&[a[0].to_le_bytes(), a[1].to_le_bytes()].concat()),
            Value::Vec4(a) => put(&a.iter().flat_map(|x| x.to_le_bytes()).collect::<Vec<_>>()),
            Value::String(None) => put(&u32::MAX.to_le_bytes()),
            Value::String(Some(s)) => {
                let off = pool.add(&UTF8.encode(s)?);
                let off =
                    u32::try_from(off).map_err(|_| Error::overflow("RDBN string offset", off))?;
                put(&off.to_le_bytes())
            }
            Value::DanglingString(o) => put(&o.to_le_bytes()),
            Value::Tuple([a, b]) => put(&[a.to_le_bytes(), b.to_le_bytes()].concat()),
        }
    }
}

// ---- sort keys (rdbn.md §7) ------------------------------------------------------------

/// Total-order sort key mirroring the Python reference's `_sort_key`: top-level ints
/// compare as unsigned 32-bit, sequences compare lexicographically on raw values.
#[derive(Debug, Clone)]
enum Key<'a> {
    Null,
    Int(i64),
    Float(f64),
    Str(&'a str),
    Seq(Vec<Key<'a>>),
}

impl Key<'_> {
    fn rank(&self) -> u8 {
        match self {
            Key::Null => 0,
            Key::Int(_) | Key::Float(_) => 1,
            Key::Str(_) => 2,
            Key::Seq(_) => 3,
        }
    }
}

fn norm(f: f64) -> f64 {
    if f == 0.0 { 0.0 } else { f }
}

impl Ord for Key<'_> {
    fn cmp(&self, o: &Self) -> Ordering {
        match (self, o) {
            (Key::Int(a), Key::Int(b)) => a.cmp(b),
            (Key::Int(a), Key::Float(b)) => norm(*a as f64).total_cmp(&norm(*b)),
            (Key::Float(a), Key::Int(b)) => norm(*a).total_cmp(&norm(*b as f64)),
            (Key::Float(a), Key::Float(b)) => norm(*a).total_cmp(&norm(*b)),
            (Key::Str(a), Key::Str(b)) => a.cmp(b),
            (Key::Seq(a), Key::Seq(b)) => a.cmp(b),
            _ => self.rank().cmp(&o.rank()),
        }
    }
}
impl PartialOrd for Key<'_> {
    fn partial_cmp(&self, o: &Self) -> Option<Ordering> {
        Some(self.cmp(o))
    }
}
impl PartialEq for Key<'_> {
    fn eq(&self, o: &Self) -> bool {
        self.cmp(o) == Ordering::Equal
    }
}
impl Eq for Key<'_> {}

fn raw_key(v: &Value) -> Key<'_> {
    match v {
        Value::Bool(b) => Key::Int(*b as i64),
        Value::Byte(b) => Key::Int(*b as i64),
        Value::SByte(b) => Key::Int(*b as i64),
        Value::Short(s) => Key::Int(*s as i64),
        Value::Int(i) => Key::Int(*i as i64),
        Value::Hash(h) => Key::Int(*h as i64),
        Value::DanglingString(o) => Key::Int(*o as i64),
        Value::Float(f) => Key::Float(*f as f64),
        Value::String(None) => Key::Null,
        Value::String(Some(s)) => Key::Str(s),
        Value::Vec2(a) => Key::Seq(a.iter().map(|f| Key::Float(*f as f64)).collect()),
        Value::Vec4(a) => Key::Seq(a.iter().map(|f| Key::Float(*f as f64)).collect()),
        Value::Tuple(a) => Key::Seq(a.iter().map(|i| Key::Int(*i as i64)).collect()),
        Value::Raw(b) => Key::Seq(b.iter().map(|i| Key::Int(*i as i64)).collect()),
        Value::Struct(fs) => Key::Seq(
            fs.iter()
                .map(|el| Key::Seq(el.iter().map(raw_key).collect()))
                .collect(),
        ),
    }
}

/// Key of the first element of a cell; ints are masked to unsigned 32-bit.
fn top_key(cell: Option<&Vec<Value>>) -> Key<'_> {
    match cell.and_then(|c| c.first()) {
        None => Key::Null,
        Some(v) => match raw_key(v) {
            Key::Int(i) => Key::Int(i & 0xFFFF_FFFF),
            k => k,
        },
    }
}

/// Find the field whose values the stored index sorts (rdbn.md §7).
///
/// Mirrors `_detect_key_field` in the Python reference (first field whose column is
/// non-decreasing in index order; else the first Hash field; else field 0), with one
/// refinement: a field whose *stable* argsort reproduces the index exactly is preferred.
/// On game files both rules pick the same field (the first non-decreasing field always
/// reproduces the index there), but the refinement also round-trips synthetic lists.
fn detect_key_field(list: &List, perm: &[u16], typ: &Type) -> usize {
    let n = list.rows.len();
    let mut seen = vec![false; n];
    let is_perm = perm.len() == n
        && perm.iter().all(|&p| {
            let p = p as usize;
            p < n && !std::mem::replace(&mut seen[p], true)
        });
    if is_perm {
        let columns: Vec<Option<Vec<Key<'_>>>> = (0..typ.fields.len())
            .map(|fi| {
                let col: Vec<Key<'_>> = list.rows.iter().map(|r| top_key(r.get(fi))).collect();
                // Python's sorted() raises TypeError on None < None: such columns never match.
                (n < 2 || !col.iter().any(|k| matches!(k, Key::Null))).then_some(col)
            })
            .collect();
        let stable = |col: &[Key<'_>]| {
            let mut idx: Vec<usize> = (0..n).collect();
            idx.sort_by(|&a, &b| col[a].cmp(&col[b]));
            idx.iter().zip(perm).all(|(&a, &b)| a == b as usize)
        };
        if let Some(fi) = columns
            .iter()
            .position(|c| c.as_deref().is_some_and(stable))
        {
            return fi;
        }
        let non_decreasing = |col: &[Key<'_>]| {
            perm.windows(2)
                .all(|w| col[w[0] as usize] <= col[w[1] as usize])
        };
        if let Some(fi) = columns
            .iter()
            .position(|c| c.as_deref().is_some_and(non_decreasing))
        {
            return fi;
        }
    }
    typ.fields
        .iter()
        .position(|f| f.ty == field_type::HASH)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests;
