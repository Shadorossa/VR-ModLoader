//! CRI `@UTF` table reader (docs/formats/audio-acb-awb-hca.md §1). All integers are big-endian.

use std::borrow::Cow;

use crate::{Error, Result};

/// One cell of a `@UTF` table.
#[derive(Debug, Clone, PartialEq)]
pub enum Value<'a> {
    /// Storage kind 0x10: no bytes, value 0 / empty.
    Null,
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
    Str(Cow<'a, str>),
    /// Binary blob (a nested `@UTF` table, an AFS2 archive, a hash, a work area…).
    Data(&'a [u8]),
}

impl<'a> Value<'a> {
    pub fn as_i64(&self) -> Option<i64> {
        Some(match self {
            Value::Null => 0,
            Value::U8(v) => *v as i64,
            Value::I8(v) => *v as i64,
            Value::U16(v) => *v as i64,
            Value::I16(v) => *v as i64,
            Value::U32(v) => *v as i64,
            Value::I32(v) => *v as i64,
            Value::U64(v) => *v as i64,
            Value::I64(v) => *v,
            _ => return None,
        })
    }

    pub fn as_f64(&self) -> Option<f64> {
        match self {
            Value::F32(v) => Some(*v as f64),
            Value::F64(v) => Some(*v),
            other => other.as_i64().map(|n| n as f64),
        }
    }

    pub fn as_str(&self) -> Option<&str> {
        match self {
            Value::Str(s) => Some(s),
            _ => None,
        }
    }

    pub fn as_data(&self) -> Option<&'a [u8]> {
        match self {
            Value::Data(d) => Some(d),
            _ => None,
        }
    }
}

#[derive(Debug, Clone)]
pub struct Column {
    pub name: String,
    /// Storage nibble: 0x10 zero, 0x30 / 0x70 constant, 0x50 per row.
    pub storage: u8,
    /// Type nibble 0..=0xB (see [`Value`]).
    pub ty: u8,
}

/// A parsed `@UTF` table. Cells are decoded on demand (`get`); the table only keeps offsets.
#[derive(Debug, Clone)]
pub struct Table<'a> {
    data: &'a [u8],
    pub name: String,
    pub columns: Vec<Column>,
    /// Constant values (storage 0x30 / 0x70) by column, `Null` for the others.
    constants: Vec<Value<'a>>,
    /// Byte offset of each per-row column inside a row (0 for the others).
    row_offsets: Vec<usize>,
    rows_off: usize,
    row_width: usize,
    pub row_count: usize,
    strings_off: usize,
    data_off: usize,
}

fn past(o: usize) -> Error {
    Error::Utf(format!("read past the end at 0x{o:X}"))
}

fn be16(d: &[u8], o: usize) -> Result<u16> {
    d.get(o..o + 2).map(|b| u16::from_be_bytes([b[0], b[1]])).ok_or_else(|| past(o))
}

fn be32(d: &[u8], o: usize) -> Result<u32> {
    d.get(o..o + 4).map(|b| u32::from_be_bytes([b[0], b[1], b[2], b[3]])).ok_or_else(|| past(o))
}

fn be64(d: &[u8], o: usize) -> Result<u64> {
    d.get(o..o + 8).map(|b| u64::from_be_bytes(b.try_into().unwrap())).ok_or_else(|| past(o))
}

/// Byte size of a value of type nibble `ty` inside a row / the schema (0 for unknown types).
fn type_size(ty: u8) -> usize {
    match ty {
        0 | 1 => 1,
        2 | 3 => 2,
        4 | 5 | 8 => 4,
        6 | 7 | 9 => 8,
        0xA => 4,
        0xB => 8,
        _ => 0,
    }
}

fn cstr<'a>(d: &'a [u8], start: usize) -> Result<Cow<'a, str>> {
    let tail = d.get(start..).ok_or_else(|| past(start))?;
    let end = tail.iter().position(|b| *b == 0).ok_or_else(|| Error::Utf(format!("unterminated string at 0x{start:X}")))?;
    Ok(String::from_utf8_lossy(&tail[..end]))
}

impl<'a> Table<'a> {
    /// Is this blob a `@UTF` table?
    pub fn is_utf(d: &[u8]) -> bool {
        d.len() >= 0x20 && &d[..4] == b"@UTF"
    }

    pub fn parse(data: &'a [u8]) -> Result<Table<'a>> {
        if !Self::is_utf(data) {
            return Err(Error::Utf("missing @UTF magic".into()));
        }
        let size = be32(data, 4)? as usize + 8;
        let data = if size <= data.len() { &data[..size] } else { data };
        let rows_off = be16(data, 0x0A)? as usize + 8;
        let strings_off = be32(data, 0x0C)? as usize + 8;
        let data_off = be32(data, 0x10)? as usize + 8;
        let name_off = be32(data, 0x14)? as usize;
        let column_count = be16(data, 0x18)? as usize;
        let row_width = be16(data, 0x1A)? as usize;
        let row_count = be32(data, 0x1C)? as usize;
        let mut columns = Vec::with_capacity(column_count);
        let mut constants = Vec::with_capacity(column_count);
        let mut row_offsets = Vec::with_capacity(column_count);
        let mut p = 0x20;
        let mut row_pos = 0;
        for _ in 0..column_count {
            let flags = *data.get(p).ok_or_else(|| Error::Utf("schema past the end".into()))?;
            let name = cstr(data, strings_off + be32(data, p + 1)? as usize)?.into_owned();
            p += 5;
            let storage = flags & 0xF0;
            let ty = flags & 0x0F;
            let constant = if storage == 0x30 || storage == 0x70 {
                let v = read_value(data, p, ty, strings_off, data_off)?;
                p += type_size(ty);
                v
            } else {
                Value::Null
            };
            row_offsets.push(row_pos);
            if storage == 0x50 {
                row_pos += type_size(ty);
            }
            columns.push(Column { name, storage, ty });
            constants.push(constant);
        }
        let name = cstr(data, strings_off + name_off)?.into_owned();
        Ok(Table { data, name, columns, constants, row_offsets, rows_off, row_width, row_count, strings_off, data_off })
    }

    pub fn column_index(&self, name: &str) -> Option<usize> {
        self.columns.iter().position(|c| c.name == name)
    }

    /// Cell `(row, column)`; `Null` when the column has no storage. Errors only on a truncated file.
    pub fn get(&self, row: usize, col: usize) -> Result<Value<'a>> {
        let c = self.columns.get(col).ok_or_else(|| Error::Utf(format!("column {col} out of range")))?;
        if row >= self.row_count {
            return Err(Error::Utf(format!("row {row} out of range")));
        }
        match c.storage {
            0x50 => read_value(self.data, self.rows_off + row * self.row_width + self.row_offsets[col], c.ty, self.strings_off, self.data_off),
            0x30 | 0x70 => Ok(self.constants[col].clone()),
            _ => Ok(Value::Null),
        }
    }

    /// Cell by column name (`None` when the column does not exist).
    pub fn cell(&self, row: usize, name: &str) -> Option<Value<'a>> {
        self.column_index(name).and_then(|c| self.get(row, c).ok())
    }

    pub fn int(&self, row: usize, name: &str) -> Option<i64> {
        self.cell(row, name).and_then(|v| v.as_i64())
    }

    pub fn str(&self, row: usize, name: &str) -> Option<String> {
        self.cell(row, name).and_then(|v| v.as_str().map(str::to_string))
    }

    pub fn data(&self, row: usize, name: &str) -> Option<&'a [u8]> {
        self.cell(row, name).and_then(|v| v.as_data())
    }

    /// Nested `@UTF` table stored in a data cell (`None` when empty or not a table).
    pub fn table(&self, row: usize, name: &str) -> Option<Table<'a>> {
        let d = self.data(row, name)?;
        if Table::is_utf(d) {
            Table::parse(d).ok()
        } else {
            None
        }
    }

    /// Every cell of a row as `(column name, value)` pairs, for dumps and tests.
    pub fn row(&self, row: usize) -> Result<Vec<(String, Value<'a>)>> {
        (0..self.columns.len()).map(|c| Ok((self.columns[c].name.clone(), self.get(row, c)?))).collect()
    }
}

fn read_value<'a>(d: &'a [u8], o: usize, ty: u8, strings_off: usize, data_off: usize) -> Result<Value<'a>> {
    Ok(match ty {
        0 => Value::U8(*d.get(o).ok_or_else(|| past(o))?),
        1 => Value::I8(*d.get(o).ok_or_else(|| past(o))? as i8),
        2 => Value::U16(be16(d, o)?),
        3 => Value::I16(be16(d, o)? as i16),
        4 => Value::U32(be32(d, o)?),
        5 => Value::I32(be32(d, o)? as i32),
        6 => Value::U64(be64(d, o)?),
        7 => Value::I64(be64(d, o)? as i64),
        8 => Value::F32(f32::from_bits(be32(d, o)?)),
        9 => Value::F64(f64::from_bits(be64(d, o)?)),
        0xA => Value::Str(cstr(d, strings_off + be32(d, o)? as usize)?),
        0xB => {
            let start = data_off + be32(d, o)? as usize;
            let len = be32(d, o + 4)? as usize;
            Value::Data(d.get(start..start + len).ok_or_else(|| Error::Utf(format!("bad data range at 0x{o:X}")))?)
        }
        other => return Err(Error::Utf(format!("unknown column type {other}"))),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a tiny table by hand: 2 columns (u16 per-row `A`, string constant `N`), 2 rows.
    fn sample() -> Vec<u8> {
        let strings = b"T\0A\0N\0hi\0";
        let schema = [0x52u8, 0, 0, 0, 2, 0x3A, 0, 0, 0, 4, 0, 0, 0, 6];
        let rows_off = 0x20 + schema.len();
        let rows = [0u8, 1, 0, 2];
        let strings_off = rows_off + rows.len();
        let data_off = strings_off + strings.len();
        let mut d = Vec::new();
        d.extend_from_slice(b"@UTF");
        d.extend_from_slice(&((data_off - 8) as u32).to_be_bytes());
        d.extend_from_slice(&1u16.to_be_bytes());
        d.extend_from_slice(&((rows_off - 8) as u16).to_be_bytes());
        d.extend_from_slice(&((strings_off - 8) as u32).to_be_bytes());
        d.extend_from_slice(&((data_off - 8) as u32).to_be_bytes());
        d.extend_from_slice(&0u32.to_be_bytes());
        d.extend_from_slice(&2u16.to_be_bytes());
        d.extend_from_slice(&2u16.to_be_bytes());
        d.extend_from_slice(&2u32.to_be_bytes());
        d.extend_from_slice(&schema);
        d.extend_from_slice(&rows);
        d.extend_from_slice(strings);
        d
    }

    #[test]
    fn parses_hand_made_table() {
        let d = sample();
        let t = Table::parse(&d).unwrap();
        assert_eq!(t.name, "T");
        assert_eq!(t.row_count, 2);
        assert_eq!(t.int(0, "A"), Some(1));
        assert_eq!(t.int(1, "A"), Some(2));
        assert_eq!(t.str(1, "N").as_deref(), Some("hi"));
        assert!(t.cell(0, "missing").is_none());
    }
}
