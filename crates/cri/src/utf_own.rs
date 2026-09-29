//! Owned, editable `@UTF` tables: parse → edit → write, **byte-exact** with the retail writer (port of
//! `tools/py/acb.py` `parse_utf` / `build_utf`; layout rules there): header 0x20 | schema | rows | string pool | [pad]
//! | data blobs | [pad]. Strings: table name, then per column its name (+ constant string), then row strings, one pool
//! entry per reference. Root tables align the data area and every blob to 32 (always 1..32 pad bytes); nested tables
//! pad the whole table to 4.

use crate::{Error, Result};

pub const STORAGE_ZERO: u8 = 0x10;
pub const STORAGE_CONST: u8 = 0x30;
pub const STORAGE_ROW: u8 = 0x50;
pub const STORAGE_CONST2: u8 = 0x70;
const ROOT_ALIGN: usize = 32;
const NESTED_PAD: usize = 4;

/// One cell.
#[derive(Debug, Clone, PartialEq)]
pub enum Val {
    /// Zero-storage column (no bytes).
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
    Str(String),
    Data(Vec<u8>),
}

impl Val {
    pub fn as_i64(&self) -> Option<i64> {
        Some(match self {
            Val::Null => 0,
            Val::U8(v) => *v as i64,
            Val::I8(v) => *v as i64,
            Val::U16(v) => *v as i64,
            Val::I16(v) => *v as i64,
            Val::U32(v) => *v as i64,
            Val::I32(v) => *v as i64,
            Val::U64(v) => *v as i64,
            Val::I64(v) => *v,
            _ => return None,
        })
    }
    pub fn as_str(&self) -> Option<&str> {
        match self {
            Val::Str(s) => Some(s),
            _ => None,
        }
    }
    pub fn as_data(&self) -> Option<&[u8]> {
        match self {
            Val::Data(d) => Some(d),
            Val::Null => Some(&[]),
            _ => None,
        }
    }
    /// A value of type nibble `ty` holding integer `v` (strings / data: empty).
    pub fn of_type(ty: u8, v: i64) -> Val {
        match ty {
            0 => Val::U8(v as u8),
            1 => Val::I8(v as i8),
            2 => Val::U16(v as u16),
            3 => Val::I16(v as i16),
            4 => Val::U32(v as u32),
            5 => Val::I32(v as i32),
            6 => Val::U64(v as u64),
            7 => Val::I64(v),
            8 => Val::F32(v as f32),
            9 => Val::F64(v as f64),
            0xA => Val::Str(String::new()),
            _ => Val::Data(Vec::new()),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Col {
    pub name: String,
    pub flags: u8,
    pub constant: Option<Val>,
}

impl Col {
    pub fn storage(&self) -> u8 {
        self.flags & 0xF0
    }
    pub fn ty(&self) -> u8 {
        self.flags & 0x0F
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Table {
    pub name: String,
    pub version: u16,
    pub root: bool,
    pub columns: Vec<Col>,
    pub rows: Vec<Vec<Val>>,
}

fn size_of(ty: u8) -> Result<usize> {
    Ok(match ty {
        0 | 1 => 1,
        2 | 3 => 2,
        4 | 5 | 8 | 0xA => 4,
        6 | 7 | 9 | 0xB => 8,
        t => return Err(Error::Utf(format!("unknown column type {t}"))),
    })
}

fn get<const K: usize>(d: &[u8], o: usize) -> Result<[u8; K]> {
    d.get(o..o + K).map(|b| b.try_into().unwrap()).ok_or_else(|| Error::Utf(format!("read past the end at 0x{o:X}")))
}

fn cstr(d: &[u8], o: usize) -> Result<String> {
    let t = d.get(o..).ok_or_else(|| Error::Utf(format!("string at 0x{o:X} out of range")))?;
    let e = t.iter().position(|&b| b == 0).ok_or_else(|| Error::Utf("unterminated string".into()))?;
    String::from_utf8(t[..e].to_vec()).map_err(|_| Error::Utf("string not UTF-8".into()))
}

impl Table {
    pub fn parse(d: &[u8], root: bool) -> Result<Table> {
        if d.len() < 0x20 || &d[..4] != b"@UTF" {
            return Err(Error::Utf("not a @UTF table".into()));
        }
        let size = u32::from_be_bytes(get(d, 4)?) as usize + 8;
        if size > d.len() {
            return Err(Error::Utf(format!("size {size} > buffer {}", d.len())));
        }
        let version = u16::from_be_bytes(get(d, 8)?);
        let row_off = u16::from_be_bytes(get(d, 0x0A)?) as usize + 8;
        let str_off = u32::from_be_bytes(get(d, 0x0C)?) as usize + 8;
        let dat_off = u32::from_be_bytes(get(d, 0x10)?) as usize + 8;
        let name_off = u32::from_be_bytes(get(d, 0x14)?) as usize;
        let ncol = u16::from_be_bytes(get(d, 0x18)?) as usize;
        let rsize = u16::from_be_bytes(get(d, 0x1A)?) as usize;
        let nrows = u32::from_be_bytes(get(d, 0x1C)?) as usize;
        let read = |ty: u8, o: usize| -> Result<Val> {
            Ok(match ty {
                0 => Val::U8(d[o]),
                1 => Val::I8(d[o] as i8),
                2 => Val::U16(u16::from_be_bytes(get(d, o)?)),
                3 => Val::I16(i16::from_be_bytes(get(d, o)?)),
                4 => Val::U32(u32::from_be_bytes(get(d, o)?)),
                5 => Val::I32(i32::from_be_bytes(get(d, o)?)),
                6 => Val::U64(u64::from_be_bytes(get(d, o)?)),
                7 => Val::I64(i64::from_be_bytes(get(d, o)?)),
                8 => Val::F32(f32::from_be_bytes(get(d, o)?)),
                9 => Val::F64(f64::from_be_bytes(get(d, o)?)),
                0xA => Val::Str(cstr(d, str_off + u32::from_be_bytes(get(d, o)?) as usize)?),
                0xB => {
                    let ro = u32::from_be_bytes(get(d, o)?) as usize;
                    let sz = u32::from_be_bytes(get(d, o + 4)?) as usize;
                    Val::Data(d.get(dat_off + ro..dat_off + ro + sz).ok_or_else(|| Error::Utf("data out of range".into()))?.to_vec())
                }
                t => return Err(Error::Utf(format!("unknown column type {t}"))),
            })
        };
        let mut columns = Vec::with_capacity(ncol);
        let mut c = 0x20;
        for _ in 0..ncol {
            let flags = *d.get(c).ok_or_else(|| Error::Utf("schema truncated".into()))?;
            let name = cstr(d, str_off + u32::from_be_bytes(get(d, c + 1)?) as usize)?;
            c += 5;
            let constant = match flags & 0xF0 {
                STORAGE_CONST | STORAGE_CONST2 => {
                    let v = read(flags & 0xF, c)?;
                    c += size_of(flags & 0xF)?;
                    Some(v)
                }
                STORAGE_ZERO | STORAGE_ROW => None,
                s => return Err(Error::Utf(format!("unknown storage 0x{s:02X} of column {name}"))),
            };
            columns.push(Col { name, flags, constant });
        }
        if c != row_off {
            return Err(Error::Utf(format!("schema ends at {c}, rows start at {row_off}")));
        }
        let mut rows = Vec::with_capacity(nrows);
        for r in 0..nrows {
            let mut p = row_off + r * rsize;
            let mut row = Vec::with_capacity(ncol);
            for col in &columns {
                row.push(match col.storage() {
                    STORAGE_ROW => {
                        let v = read(col.ty(), p)?;
                        p += size_of(col.ty())?;
                        v
                    }
                    STORAGE_ZERO => Val::Null,
                    _ => col.constant.clone().unwrap_or(Val::Null),
                });
            }
            rows.push(row);
        }
        Ok(Table { name: cstr(d, str_off + name_off)?, version, root, columns, rows })
    }

    pub fn col(&self, name: &str) -> Option<usize> {
        self.columns.iter().position(|c| c.name == name)
    }

    pub fn get(&self, row: usize, name: &str) -> Option<&Val> {
        self.rows.get(row)?.get(self.col(name)?)
    }

    pub fn int(&self, row: usize, name: &str) -> Option<i64> {
        self.get(row, name)?.as_i64()
    }

    pub fn data(&self, row: usize, name: &str) -> Option<&[u8]> {
        self.get(row, name)?.as_data()
    }

    /// The @UTF table nested in data column `name` of `row`.
    pub fn nested(&self, name: &str, row: usize) -> Result<Table> {
        let d = self.data(row, name).ok_or_else(|| Error::Utf(format!("{}.{name}: no data column", self.name)))?;
        Table::parse(d, false)
    }

    pub fn set_nested(&mut self, name: &str, row: usize, mut t: Table) -> Result<()> {
        t.root = false;
        let b = t.build()?;
        self.set(row, name, Val::Data(b))
    }

    /// Set a cell; a constant / zero column whose value would change becomes a per-row column (acb.py `set_val`).
    pub fn set(&mut self, row: usize, name: &str, v: Val) -> Result<()> {
        let ci = self.col(name).ok_or_else(|| Error::Utf(format!("{}: no column {name}", self.name)))?;
        let ty = self.columns[ci].ty();
        let v = coerce(ty, v)?;
        if self.columns[ci].storage() != STORAGE_ROW && self.rows[row][ci] != v {
            for r in self.rows.iter_mut() {
                if r[ci] == Val::Null {
                    r[ci] = Val::of_type(ty, 0);
                }
            }
            self.columns[ci].flags = STORAGE_ROW | ty;
            self.columns[ci].constant = None;
        }
        self.rows[row][ci] = v;
        Ok(())
    }

    /// Append a copy of row `tpl`; returns its index.
    pub fn push_copy(&mut self, tpl: usize) -> usize {
        let r = self.rows[tpl].clone();
        self.rows.push(r);
        self.rows.len() - 1
    }

    pub fn row_size(&self) -> usize {
        self.columns.iter().filter(|c| c.storage() == STORAGE_ROW).map(|c| size_of(c.ty()).unwrap_or(0)).sum()
    }

    /// Serialize (retail layout, byte-exact for unmodified tables).
    pub fn build(&self) -> Result<Vec<u8>> {
        let root = self.root;
        let mut strings: Vec<u8> = Vec::new();
        let mut data: Vec<u8> = Vec::new();
        let add_str = |s: &str, strings: &mut Vec<u8>| -> u32 {
            let o = strings.len() as u32;
            strings.extend_from_slice(s.as_bytes());
            strings.push(0);
            o
        };
        fn add_data(b: &[u8], data: &mut Vec<u8>, root: bool) -> (u32, u32) {
            if b.is_empty() {
                return (0, 0);
            }
            if root && !data.is_empty() {
                let pad = ROOT_ALIGN - data.len() % ROOT_ALIGN;
                data.extend(std::iter::repeat(0).take(pad));
            }
            let o = data.len() as u32;
            data.extend_from_slice(b);
            (o, b.len() as u32)
        }
        let pack = |ty: u8, v: &Val, out: &mut Vec<u8>, strings: &mut Vec<u8>, data: &mut Vec<u8>| -> Result<()> {
            let v = coerce(ty, v.clone())?;
            match v {
                Val::U8(x) => out.push(x),
                Val::I8(x) => out.push(x as u8),
                Val::U16(x) => out.extend_from_slice(&x.to_be_bytes()),
                Val::I16(x) => out.extend_from_slice(&x.to_be_bytes()),
                Val::U32(x) => out.extend_from_slice(&x.to_be_bytes()),
                Val::I32(x) => out.extend_from_slice(&x.to_be_bytes()),
                Val::U64(x) => out.extend_from_slice(&x.to_be_bytes()),
                Val::I64(x) => out.extend_from_slice(&x.to_be_bytes()),
                Val::F32(x) => out.extend_from_slice(&x.to_be_bytes()),
                Val::F64(x) => out.extend_from_slice(&x.to_be_bytes()),
                Val::Str(s) => out.extend_from_slice(&add_str(&s, strings).to_be_bytes()),
                Val::Data(b) => {
                    let (o, n) = add_data(&b, data, root);
                    out.extend_from_slice(&o.to_be_bytes());
                    out.extend_from_slice(&n.to_be_bytes());
                }
                Val::Null => return Err(Error::Utf("null value in a stored column".into())),
            }
            Ok(())
        };
        let name_off = add_str(&self.name, &mut strings);
        let mut schema = Vec::new();
        for c in &self.columns {
            schema.push(c.flags);
            let o = add_str(&c.name, &mut strings);
            schema.extend_from_slice(&o.to_be_bytes());
            if matches!(c.storage(), STORAGE_CONST | STORAGE_CONST2) {
                pack(c.ty(), c.constant.as_ref().unwrap_or(&Val::Null), &mut schema, &mut strings, &mut data)?;
            }
        }
        let mut rows = Vec::new();
        for r in &self.rows {
            for (ci, c) in self.columns.iter().enumerate() {
                if c.storage() == STORAGE_ROW {
                    pack(c.ty(), &r[ci], &mut rows, &mut strings, &mut data)?;
                }
            }
        }
        let row_off = 0x20 + schema.len();
        let str_off = row_off + rows.len();
        let mut dat_off = str_off + strings.len();
        let total;
        if root {
            dat_off += ROOT_ALIGN - dat_off % ROOT_ALIGN;
            let end = dat_off + data.len();
            total = end + ROOT_ALIGN - end % ROOT_ALIGN;
        } else {
            total = (dat_off + data.len()).div_ceil(NESTED_PAD) * NESTED_PAD;
        }
        let mut out = Vec::with_capacity(total);
        out.extend_from_slice(b"@UTF");
        out.extend_from_slice(&((total - 8) as u32).to_be_bytes());
        out.extend_from_slice(&self.version.to_be_bytes());
        out.extend_from_slice(&((row_off - 8) as u16).to_be_bytes());
        out.extend_from_slice(&((str_off - 8) as u32).to_be_bytes());
        out.extend_from_slice(&((dat_off - 8) as u32).to_be_bytes());
        out.extend_from_slice(&name_off.to_be_bytes());
        out.extend_from_slice(&(self.columns.len() as u16).to_be_bytes());
        out.extend_from_slice(&(self.row_size() as u16).to_be_bytes());
        out.extend_from_slice(&(self.rows.len() as u32).to_be_bytes());
        out.extend_from_slice(&schema);
        out.extend_from_slice(&rows);
        out.extend_from_slice(&strings);
        out.resize(dat_off, 0);
        out.extend_from_slice(&data);
        out.resize(total, 0);
        Ok(out)
    }
}

/// `v` as a value of column type `ty` (integers convert between widths; Null = zero / empty).
fn coerce(ty: u8, v: Val) -> Result<Val> {
    Ok(match (ty, v) {
        (0xA, Val::Str(s)) => Val::Str(s),
        (0xA, Val::Null) => Val::Str(String::new()),
        (0xB, Val::Data(d)) => Val::Data(d),
        (0xB, Val::Null) => Val::Data(Vec::new()),
        (8, Val::F32(x)) => Val::F32(x),
        (9, Val::F64(x)) => Val::F64(x),
        (8, Val::F64(x)) => Val::F32(x as f32),
        (9, Val::F32(x)) => Val::F64(x as f64),
        (t, v) if t <= 7 => Val::of_type(t, v.as_i64().ok_or_else(|| Error::Utf(format!("value {v:?} for an integer column")))?),
        (t, v) => return Err(Error::Utf(format!("value {v:?} for a column of type {t}"))),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const DUMP: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../assets/v7.1.2/data/common/sound_asset");

    fn plain(p: &std::path::Path) -> Option<Vec<u8>> {
        let b = std::fs::read(p).ok()?;
        if &b[..4] == b"@UTF" {
            return Some(b);
        }
        Some(crate::crypt::xor(&b, crate::crypt::loose_key(p.file_name()?.to_str()?), 0))
    }

    fn roundtrip(d: &[u8], root: bool, label: &str) {
        let t = Table::parse(d, root).unwrap();
        assert_eq!(t.build().unwrap(), d, "{label}");
        for r in 0..t.rows.len() {
            for c in 0..t.columns.len() {
                if let Val::Data(b) = &t.rows[r][c] {
                    if b.len() >= 0x20 && &b[..4] == b"@UTF" {
                        roundtrip(b, false, &format!("{label}/{}[{r}]", t.columns[c].name));
                    }
                }
            }
        }
    }

    #[test]
    fn retail_banks_round_trip_byte_exact() {
        let dir = std::path::Path::new(DUMP);
        if !dir.is_dir() {
            return;
        }
        let mut n = 0;
        for name in ["bgm_title.acb", "common.acb", "sr.acb", "ja/c01000010.acb", "ja/c05020700.acb", "en/c01000070.acb"] {
            let Some(d) = plain(&dir.join(name)) else { continue };
            roundtrip(&d, true, name);
            n += 1;
        }
        assert!(n >= 4, "only {n} banks found");
    }

    #[test]
    fn set_turns_a_constant_column_per_row() {
        let t = Table {
            name: "T".into(),
            version: 1,
            root: false,
            columns: vec![
                Col { name: "A".into(), flags: STORAGE_CONST | 4, constant: Some(Val::U32(7)) },
                Col { name: "S".into(), flags: STORAGE_ROW | 0xA, constant: None },
            ],
            rows: vec![vec![Val::U32(7), Val::Str("x".into())], vec![Val::U32(7), Val::Str("y".into())]],
        };
        let b = t.build().unwrap();
        let mut back = Table::parse(&b, false).unwrap();
        assert_eq!(back, t);
        back.set(1, "A", Val::I64(9)).unwrap();
        assert_eq!(back.columns[0].storage(), STORAGE_ROW);
        let again = Table::parse(&back.build().unwrap(), false).unwrap();
        assert_eq!(again.int(0, "A"), Some(7));
        assert_eq!(again.int(1, "A"), Some(9));
    }
}
