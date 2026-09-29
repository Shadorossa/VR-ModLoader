//! CRI `@UTF` tables: reader and writer.
//!
//! Spec: `docs/formats/cpk.md` §2.1. All integers are big-endian. Layout:
//!
//! ```text
//! 0x00 "@UTF"   0x04 u32 table size (total - 8)
//! 0x08 u32 rows offset   0x0C u32 string pool offset   0x10 u32 data pool offset   (all relative to +8)
//! 0x14 u32 table name (string pool offset)   0x18 u16 column count   0x1A u16 row width   0x1C u32 row count
//! 0x20 column descriptors: u8 flags (storage | type) + u32 name offset (+ inline value if constant)
//! ```
//!
//! Masked tables (XOR stream `m = 0x655F; m *= 0x4115`) are unmasked transparently on read; IEVR tables are not
//! masked. The writer never masks.

use std::collections::HashMap;

use crate::error::{Error, Result, show_magic};

/// Value type (low nibble of the column flags).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ColumnType {
    /// 0x0
    U8,
    /// 0x1
    I8,
    /// 0x2
    U16,
    /// 0x3
    I16,
    /// 0x4
    U32,
    /// 0x5
    I32,
    /// 0x6
    U64,
    /// 0x7
    I64,
    /// 0x8
    F32,
    /// 0x9
    F64,
    /// 0xA: u32 offset into the string pool.
    String,
    /// 0xB: u32 offset + u32 length into the data pool.
    Data,
}

impl ColumnType {
    /// Parse the low nibble.
    pub fn from_nibble(n: u8) -> Option<Self> {
        use ColumnType::*;
        Some(match n {
            0x0 => U8,
            0x1 => I8,
            0x2 => U16,
            0x3 => I16,
            0x4 => U32,
            0x5 => I32,
            0x6 => U64,
            0x7 => I64,
            0x8 => F32,
            0x9 => F64,
            0xA => String,
            0xB => Data,
            _ => return None,
        })
    }
    /// The low nibble.
    pub fn nibble(self) -> u8 {
        use ColumnType::*;
        match self {
            U8 => 0x0,
            I8 => 0x1,
            U16 => 0x2,
            I16 => 0x3,
            U32 => 0x4,
            I32 => 0x5,
            U64 => 0x6,
            I64 => 0x7,
            F32 => 0x8,
            F64 => 0x9,
            String => 0xA,
            Data => 0xB,
        }
    }
    /// Encoded size in bytes.
    pub fn size(self) -> usize {
        use ColumnType::*;
        match self {
            U8 | I8 => 1,
            U16 | I16 => 2,
            U32 | I32 | F32 | String => 4,
            U64 | I64 | F64 | Data => 8,
        }
    }
}

/// Storage class (high nibble of the column flags).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Storage {
    /// 0x00: no value.
    None,
    /// 0x10: zero / absent, no value stored.
    Zero,
    /// 0x30: one value stored in the column descriptor.
    Constant,
    /// 0x50: one value per row.
    PerRow,
}

impl Storage {
    /// Parse the high nibble (`flags & 0xF0`).
    pub fn from_bits(b: u8) -> Option<Self> {
        Some(match b {
            0x00 => Storage::None,
            0x10 => Storage::Zero,
            0x30 => Storage::Constant,
            0x50 => Storage::PerRow,
            _ => return None,
        })
    }
    /// The high-nibble bits.
    pub fn bits(self) -> u8 {
        match self {
            Storage::None => 0x00,
            Storage::Zero => 0x10,
            Storage::Constant => 0x30,
            Storage::PerRow => 0x50,
        }
    }
}

/// A cell value.
#[derive(Debug, Clone, PartialEq)]
#[allow(missing_docs)]
pub enum Value {
    U8(u8),
    I8(i8),
    U16(u16),
    I16(i16),
    U32(u32),
    I32(i32),
    U64(u64),
    I64(i64),
    F32(f32),
    F64(f64),
    String(String),
    Data(Vec<u8>),
}

impl Value {
    /// Any integer value as `u64` (negative values → `None`).
    pub fn as_u64(&self) -> Option<u64> {
        match *self {
            Value::U8(v) => Some(v as u64),
            Value::U16(v) => Some(v as u64),
            Value::U32(v) => Some(v as u64),
            Value::U64(v) => Some(v),
            Value::I8(v) => u64::try_from(v).ok(),
            Value::I16(v) => u64::try_from(v).ok(),
            Value::I32(v) => u64::try_from(v).ok(),
            Value::I64(v) => u64::try_from(v).ok(),
            _ => None,
        }
    }
    /// Any integer value as `i64` (u64 above `i64::MAX` → `None`).
    pub fn as_i64(&self) -> Option<i64> {
        match *self {
            Value::U8(v) => Some(v as i64),
            Value::U16(v) => Some(v as i64),
            Value::U32(v) => Some(v as i64),
            Value::U64(v) => i64::try_from(v).ok(),
            Value::I8(v) => Some(v as i64),
            Value::I16(v) => Some(v as i64),
            Value::I32(v) => Some(v as i64),
            Value::I64(v) => Some(v),
            _ => None,
        }
    }
    /// Float value (integers are converted).
    pub fn as_f64(&self) -> Option<f64> {
        match *self {
            Value::F32(v) => Some(v as f64),
            Value::F64(v) => Some(v),
            _ => self.as_i64().map(|v| v as f64).or_else(|| self.as_u64().map(|v| v as f64)),
        }
    }
    /// String value.
    pub fn as_str(&self) -> Option<&str> {
        match self {
            Value::String(s) => Some(s),
            _ => None,
        }
    }
    /// Data (blob) value.
    pub fn as_data(&self) -> Option<&[u8]> {
        match self {
            Value::Data(d) => Some(d),
            _ => None,
        }
    }
    /// The natural column type of this value.
    pub fn column_type(&self) -> ColumnType {
        match self {
            Value::U8(_) => ColumnType::U8,
            Value::I8(_) => ColumnType::I8,
            Value::U16(_) => ColumnType::U16,
            Value::I16(_) => ColumnType::I16,
            Value::U32(_) => ColumnType::U32,
            Value::I32(_) => ColumnType::I32,
            Value::U64(_) => ColumnType::U64,
            Value::I64(_) => ColumnType::I64,
            Value::F32(_) => ColumnType::F32,
            Value::F64(_) => ColumnType::F64,
            Value::String(_) => ColumnType::String,
            Value::Data(_) => ColumnType::Data,
        }
    }
}

/// A column descriptor.
#[derive(Debug, Clone, PartialEq)]
pub struct Column {
    /// Column name.
    pub name: String,
    /// Value type.
    pub ty: ColumnType,
    /// Storage class.
    pub storage: Storage,
    /// The value of a [`Storage::Constant`] column (ignored otherwise).
    pub constant: Option<Value>,
}

/// A parsed (or to-be-written) `@UTF` table.
///
/// `rows[r][c]` holds the value of per-row column `c`; cells of zero/constant columns are `None` and are ignored
/// by the writer. Use [`UtfTable::get`] to read a cell with constants resolved.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct UtfTable {
    /// Table name (e.g. `CpkHeader`, `CpkTocInfo`).
    pub name: String,
    /// Column descriptors.
    pub columns: Vec<Column>,
    /// Row cells, one per column.
    pub rows: Vec<Vec<Option<Value>>>,
    /// Strings written at the very start of the string pool, before the table name (CRI's tools write `<NULL>`
    /// there, Viola only in ETOC). Only affects the byte layout; the reader fills it from the input so that
    /// read → write is byte-identical for retail tables.
    pub seed_strings: Vec<String>,
}

const UTF_MAGIC: &[u8; 4] = b"@UTF";

/// Unmask a masked `@UTF` table (CriFsV2Lib's `m = 0x655F; m *= 0x4115`). Returns `None` if the result is not
/// `@UTF` either.
pub fn unmask(data: &[u8]) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(data.len());
    let mut m: u32 = 0x655F;
    for &b in data {
        out.push(b ^ (m as u8));
        m = m.wrapping_mul(0x4115);
    }
    out.starts_with(UTF_MAGIC).then_some(out)
}

struct Cur<'a> {
    b: &'a [u8],
}

impl<'a> Cur<'a> {
    fn slice(&self, off: usize, n: usize) -> Result<&'a [u8]> {
        off.checked_add(n)
            .and_then(|e| self.b.get(off..e))
            .ok_or(Error::Truncated { what: "@UTF table", offset: off as u64, needed: n as u64, available: self.b.len() as u64 })
    }
    fn u8(&self, off: usize) -> Result<u8> {
        Ok(self.slice(off, 1)?[0])
    }
    fn u16(&self, off: usize) -> Result<u16> {
        Ok(u16::from_be_bytes(self.slice(off, 2)?.try_into().unwrap_or_default()))
    }
    fn u32(&self, off: usize) -> Result<u32> {
        Ok(u32::from_be_bytes(self.slice(off, 4)?.try_into().unwrap_or_default()))
    }
    fn u64(&self, off: usize) -> Result<u64> {
        Ok(u64::from_be_bytes(self.slice(off, 8)?.try_into().unwrap_or_default()))
    }
}

impl UtfTable {
    /// An empty table named `name`.
    pub fn new(name: impl Into<String>) -> Self {
        Self { name: name.into(), ..Default::default() }
    }

    /// Append a column (builder style). For [`Storage::Constant`] use [`UtfTable::add_const`].
    pub fn add_column(&mut self, name: impl Into<String>, ty: ColumnType, storage: Storage) -> &mut Self {
        self.columns.push(Column { name: name.into(), ty, storage, constant: None });
        for row in &mut self.rows {
            row.push(None);
        }
        self
    }

    /// Append a constant column.
    pub fn add_const(&mut self, name: impl Into<String>, ty: ColumnType, value: Value) -> &mut Self {
        self.columns.push(Column { name: name.into(), ty, storage: Storage::Constant, constant: Some(value) });
        for row in &mut self.rows {
            row.push(None);
        }
        self
    }

    /// Append a row given the values of the **per-row** columns only, in column order.
    pub fn push_row_values(&mut self, values: impl IntoIterator<Item = Value>) -> Result<()> {
        let mut it = values.into_iter();
        let mut row = Vec::with_capacity(self.columns.len());
        for c in &self.columns {
            if c.storage == Storage::PerRow {
                let v = it.next().ok_or_else(|| Error::Utf(format!("missing value for column {}", c.name)))?;
                row.push(Some(v));
            } else {
                row.push(None);
            }
        }
        if it.next().is_some() {
            return Err(Error::Utf("too many row values".into()));
        }
        self.rows.push(row);
        Ok(())
    }

    /// Index of the column called `name`.
    pub fn column_index(&self, name: &str) -> Option<usize> {
        self.columns.iter().position(|c| c.name == name)
    }

    /// The cell `(row, column name)` with constants resolved; `None` for zero-storage columns, missing columns or
    /// out-of-range rows.
    pub fn get(&self, row: usize, name: &str) -> Option<&Value> {
        let ci = self.column_index(name)?;
        self.get_at(row, ci)
    }

    /// Like [`UtfTable::get`] by column index.
    pub fn get_at(&self, row: usize, col: usize) -> Option<&Value> {
        let c = self.columns.get(col)?;
        match c.storage {
            Storage::None | Storage::Zero => None,
            Storage::Constant => {
                if row < self.rows.len() {
                    c.constant.as_ref()
                } else {
                    None
                }
            }
            Storage::PerRow => self.rows.get(row)?.get(col)?.as_ref(),
        }
    }

    /// Integer cell as `u64`.
    pub fn get_u64(&self, row: usize, name: &str) -> Option<u64> {
        self.get(row, name)?.as_u64()
    }

    /// String cell.
    pub fn get_str(&self, row: usize, name: &str) -> Option<&str> {
        self.get(row, name)?.as_str()
    }

    /// Parse a table from bytes starting with `@UTF` (or a masked table).
    pub fn parse(data: &[u8]) -> Result<Self> {
        if data.starts_with(UTF_MAGIC) {
            Self::parse_plain(data)
        } else if let Some(unmasked) = unmask(data) {
            Self::parse_plain(&unmasked)
        } else {
            Err(Error::BadMagic { what: "@UTF table", expected: "@UTF", found: show_magic(data) })
        }
    }

    fn parse_plain(data: &[u8]) -> Result<Self> {
        let c = Cur { b: data };
        let table_size = c.u32(4)? as usize;
        let total = table_size
            .checked_add(8)
            .filter(|&t| t <= data.len())
            .ok_or_else(|| Error::Utf(format!("table size {table_size:#x} exceeds data length {:#x}", data.len())))?;
        let c = Cur { b: &data[..total] };
        let rows_off = c.u32(8)? as usize + 8;
        let str_off = c.u32(12)? as usize + 8;
        let data_off = c.u32(16)? as usize + 8;
        let name_off = c.u32(20)?;
        let ncols = c.u16(24)? as usize;
        let row_width = c.u16(26)? as usize;
        let nrows = c.u32(28)? as usize;
        if str_off > total || data_off > total || rows_off > total || str_off > data_off {
            return Err(Error::Utf("pool offsets out of range".into()));
        }
        let strings = &data[str_off..data_off];
        let blobs = &data[data_off..total];
        let get_str = |off: u32| -> Result<String> {
            let off = off as usize;
            let s = strings.get(off..).ok_or_else(|| Error::Utf(format!("string offset {off:#x} out of range")))?;
            let end = s.iter().position(|&b| b == 0).unwrap_or(s.len());
            Ok(String::from_utf8_lossy(&s[..end]).into_owned())
        };
        let read_value = |ty: ColumnType, p: usize| -> Result<Value> {
            Ok(match ty {
                ColumnType::U8 => Value::U8(c.u8(p)?),
                ColumnType::I8 => Value::I8(c.u8(p)? as i8),
                ColumnType::U16 => Value::U16(c.u16(p)?),
                ColumnType::I16 => Value::I16(c.u16(p)? as i16),
                ColumnType::U32 => Value::U32(c.u32(p)?),
                ColumnType::I32 => Value::I32(c.u32(p)? as i32),
                ColumnType::U64 => Value::U64(c.u64(p)?),
                ColumnType::I64 => Value::I64(c.u64(p)? as i64),
                ColumnType::F32 => Value::F32(f32::from_bits(c.u32(p)?)),
                ColumnType::F64 => Value::F64(f64::from_bits(c.u64(p)?)),
                ColumnType::String => Value::String(get_str(c.u32(p)?)?),
                ColumnType::Data => {
                    let o = c.u32(p)? as usize;
                    let l = c.u32(p + 4)? as usize;
                    let d = o
                        .checked_add(l)
                        .and_then(|e| blobs.get(o..e))
                        .ok_or_else(|| Error::Utf(format!("data blob {o:#x}+{l:#x} out of range")))?;
                    Value::Data(d.to_vec())
                }
            })
        };

        let name = get_str(name_off)?;
        let mut columns = Vec::with_capacity(ncols.min(4096));
        let mut p = 0x20usize;
        for _ in 0..ncols {
            let flags = c.u8(p)?;
            let cname = get_str(c.u32(p + 1)?)?;
            p += 5;
            let storage = Storage::from_bits(flags & 0xF0)
                .ok_or_else(|| Error::Utf(format!("column {cname}: unknown storage {:#x}", flags & 0xF0)))?;
            let ty = ColumnType::from_nibble(flags & 0x0F)
                .ok_or_else(|| Error::Utf(format!("column {cname}: unknown type {:#x}", flags & 0x0F)))?;
            let constant = if storage == Storage::Constant {
                let v = read_value(ty, p)?;
                p += ty.size();
                Some(v)
            } else {
                None
            };
            columns.push(Column { name: cname, ty, storage, constant });
        }
        let needed_width: usize = columns.iter().filter(|c| c.storage == Storage::PerRow).map(|c| c.ty.size()).sum();
        if needed_width > row_width {
            return Err(Error::Utf(format!("row width {row_width} < sum of per-row columns {needed_width}")));
        }
        // Rows must fit before the table end.
        let rows_bytes = nrows.checked_mul(row_width).ok_or_else(|| Error::Utf("row count overflow".into()))?;
        if nrows > 0 && rows_off.checked_add(rows_bytes).is_none_or(|e| e > total) {
            return Err(Error::Utf(format!("{nrows} rows of {row_width} bytes do not fit")));
        }
        let mut rows = Vec::with_capacity(nrows);
        for r in 0..nrows {
            let mut rp = rows_off + r * row_width;
            let mut row = Vec::with_capacity(columns.len());
            for col in &columns {
                if col.storage == Storage::PerRow {
                    row.push(Some(read_value(col.ty, rp)?));
                    rp += col.ty.size();
                } else {
                    row.push(None);
                }
            }
            rows.push(row);
        }
        // Strings stored before the table name (CRI's own writer puts "<NULL>" first) are kept so that
        // re-serialisation is byte-identical.
        let seed_strings = strings
            .get(..name_off as usize)
            .filter(|p| p.last() == Some(&0))
            .map(|p| p[..p.len() - 1].split(|&b| b == 0).map(|s| String::from_utf8_lossy(s).into_owned()).collect())
            .unwrap_or_default();
        Ok(Self { name, columns, rows, seed_strings })
    }

    /// Serialize the table (unmasked, padded to 8 bytes).
    ///
    /// String pool order: [`UtfTable::seed_strings`], table name, column names and constant strings in column order,
    /// then row strings row by row; identical strings are shared. The data pool follows the string pool (8-aligned).
    pub fn to_bytes(&self) -> Result<Vec<u8>> {
        let mut pool = StringPool::default();
        for s in &self.seed_strings {
            pool.add(s);
        }
        let name_off = pool.add(&self.name);
        let mut blobs: Vec<u8> = Vec::new();

        let mut cols = Vec::new();
        for c in &self.columns {
            cols.push(c.storage.bits() | c.ty.nibble());
            cols.extend_from_slice(&pool.add(&c.name).to_be_bytes());
            if c.storage == Storage::Constant {
                let v = c
                    .constant
                    .as_ref()
                    .ok_or_else(|| Error::Utf(format!("constant column {} has no value", c.name)))?;
                write_value(&mut cols, c, v, &mut pool, &mut blobs)?;
            }
        }
        let row_width: usize =
            self.columns.iter().filter(|c| c.storage == Storage::PerRow).map(|c| c.ty.size()).sum();
        let mut rows = Vec::with_capacity(row_width * self.rows.len());
        for (ri, row) in self.rows.iter().enumerate() {
            for (ci, c) in self.columns.iter().enumerate() {
                if c.storage != Storage::PerRow {
                    continue;
                }
                let v = row
                    .get(ci)
                    .and_then(|v| v.as_ref())
                    .ok_or_else(|| Error::Utf(format!("row {ri}: missing value for column {}", c.name)))?;
                write_value(&mut rows, c, v, &mut pool, &mut blobs)?;
            }
        }
        let ncols = u16::try_from(self.columns.len()).map_err(|_| Error::OutOfRange("too many columns".into()))?;
        let row_width = u16::try_from(row_width).map_err(|_| Error::OutOfRange("row too wide".into()))?;
        let nrows = u32::try_from(self.rows.len()).map_err(|_| Error::OutOfRange("too many rows".into()))?;

        let rows_off = 0x20 + cols.len();
        let str_off = rows_off + rows.len();
        let mut out = Vec::with_capacity(str_off + pool.bytes.len() + blobs.len() + 16);
        out.extend_from_slice(UTF_MAGIC);
        out.extend_from_slice(&[0; 4]); // table size, patched below
        out.extend_from_slice(&be32(rows_off - 8)?);
        out.extend_from_slice(&be32(str_off - 8)?);
        out.extend_from_slice(&[0; 4]); // data offset, patched below
        out.extend_from_slice(&name_off.to_be_bytes());
        out.extend_from_slice(&ncols.to_be_bytes());
        out.extend_from_slice(&row_width.to_be_bytes());
        out.extend_from_slice(&nrows.to_be_bytes());
        out.extend_from_slice(&cols);
        out.extend_from_slice(&rows);
        out.extend_from_slice(&pool.bytes);
        pad_to(&mut out, 8);
        let data_off = out.len();
        out.extend_from_slice(&blobs);
        pad_to(&mut out, 8);
        let table_size = be32(out.len() - 8)?;
        out[4..8].copy_from_slice(&table_size);
        out[16..20].copy_from_slice(&be32(data_off - 8)?);
        Ok(out)
    }
}

fn be32(v: usize) -> Result<[u8; 4]> {
    u32::try_from(v).map(u32::to_be_bytes).map_err(|_| Error::OutOfRange(format!("{v:#x} does not fit in u32")))
}

pub(crate) fn pad_to(v: &mut Vec<u8>, align: usize) {
    let rem = v.len() % align;
    if rem != 0 {
        v.resize(v.len() + align - rem, 0);
    }
}

#[derive(Default)]
struct StringPool {
    bytes: Vec<u8>,
    map: HashMap<String, u32>,
}

impl StringPool {
    fn add(&mut self, s: &str) -> u32 {
        if let Some(&o) = self.map.get(s) {
            return o;
        }
        let o = self.bytes.len() as u32;
        self.bytes.extend_from_slice(s.as_bytes());
        self.bytes.push(0);
        self.map.insert(s.to_owned(), o);
        o
    }
}

fn write_value(out: &mut Vec<u8>, col: &Column, v: &Value, pool: &mut StringPool, blobs: &mut Vec<u8>) -> Result<()> {
    let bad = || Error::OutOfRange(format!("column {} ({:?}) cannot hold {:?}", col.name, col.ty, v));
    // Integer bit pattern of `bits` width: non-negative values must fit unsigned, negative ones signed.
    let int = |bits: u32| -> Option<u64> {
        let max = if bits == 64 { u64::MAX } else { (1u64 << bits) - 1 };
        if let Some(u) = v.as_u64() {
            return (u <= max).then_some(u);
        }
        let i = v.as_i64()?;
        let min = if bits == 64 { i64::MIN } else { -(1i64 << (bits - 1)) };
        (i >= min).then_some(i as u64 & max)
    };
    match col.ty {
        ColumnType::U8 | ColumnType::I8 => out.push(int(8).ok_or_else(bad)? as u8),
        ColumnType::U16 | ColumnType::I16 => out.extend_from_slice(&(int(16).ok_or_else(bad)? as u16).to_be_bytes()),
        ColumnType::U32 | ColumnType::I32 => out.extend_from_slice(&(int(32).ok_or_else(bad)? as u32).to_be_bytes()),
        ColumnType::U64 | ColumnType::I64 => out.extend_from_slice(&int(64).ok_or_else(bad)?.to_be_bytes()),
        ColumnType::F32 => out.extend_from_slice(&(v.as_f64().ok_or_else(bad)? as f32).to_bits().to_be_bytes()),
        ColumnType::F64 => out.extend_from_slice(&v.as_f64().ok_or_else(bad)?.to_bits().to_be_bytes()),
        ColumnType::String => out.extend_from_slice(&pool.add(v.as_str().ok_or_else(bad)?).to_be_bytes()),
        ColumnType::Data => {
            let d = v.as_data().ok_or_else(bad)?;
            let off = if d.is_empty() { 0 } else { blobs.len() };
            blobs.extend_from_slice(d);
            out.extend_from_slice(&be32(off)?);
            out.extend_from_slice(&be32(d.len())?);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> UtfTable {
        let mut t = UtfTable::new("Sample");
        t.add_column("A", ColumnType::U8, Storage::PerRow)
            .add_column("B", ColumnType::I16, Storage::PerRow)
            .add_const("C", ColumnType::String, Value::String("<NULL>".into()))
            .add_column("D", ColumnType::U64, Storage::Zero)
            .add_column("E", ColumnType::F32, Storage::PerRow)
            .add_column("F", ColumnType::String, Storage::PerRow)
            .add_column("G", ColumnType::Data, Storage::PerRow)
            .add_column("H", ColumnType::I64, Storage::PerRow)
            .add_column("I", ColumnType::F64, Storage::PerRow)
            .add_column("J", ColumnType::U32, Storage::PerRow);
        for i in 0..3u8 {
            t.push_row_values([
                Value::U8(i),
                Value::I16(-(i as i16)),
                Value::F32(i as f32 * 0.5),
                Value::String(format!("row{i}")),
                Value::Data(vec![i; i as usize]),
                Value::I64(-1_000_000_000_000 * i as i64),
                Value::F64(1.25),
                Value::U32(0xFFFF_FFF0 + i as u32),
            ])
            .unwrap();
        }
        t
    }

    #[test]
    fn roundtrip_all_types() {
        let t = sample();
        let bytes = t.to_bytes().unwrap();
        assert_eq!(bytes.len() % 8, 0);
        let back = UtfTable::parse(&bytes).unwrap();
        assert_eq!(back, t);
        assert_eq!(back.get_str(2, "C"), Some("<NULL>"));
        assert_eq!(back.get(1, "D"), None);
        assert_eq!(back.get_str(2, "F"), Some("row2"));
        assert_eq!(back.get(2, "G").and_then(Value::as_data), Some(&[2u8, 2][..]));
        // Re-serialization is stable.
        assert_eq!(back.to_bytes().unwrap(), bytes);
    }

    #[test]
    fn masked_table() {
        let bytes = sample().to_bytes().unwrap();
        let mut masked = bytes.clone();
        let mut m: u32 = 0x655F;
        for b in &mut masked {
            *b ^= m as u8;
            m = m.wrapping_mul(0x4115);
        }
        assert_eq!(UtfTable::parse(&masked).unwrap(), sample());
    }

    #[test]
    fn malformed_input_is_an_error() {
        let bytes = sample().to_bytes().unwrap();
        for n in 0..bytes.len() {
            let _ = UtfTable::parse(&bytes[..n]); // must not panic
        }
        let mut corrupt = bytes.clone();
        corrupt[4..0x20].fill(0xFF);
        assert!(UtfTable::parse(&corrupt).is_err());
        assert!(UtfTable::parse(b"nope").is_err());
    }

    #[test]
    fn missing_row_value_is_an_error() {
        let mut t = UtfTable::new("x");
        t.add_column("A", ColumnType::U8, Storage::PerRow);
        t.rows.push(vec![None]);
        assert!(t.to_bytes().is_err());
        let mut t = UtfTable::new("x");
        t.add_column("A", ColumnType::U8, Storage::PerRow);
        t.push_row_values([Value::U32(300)]).unwrap();
        assert!(t.to_bytes().is_err());
    }
}
