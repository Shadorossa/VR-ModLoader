//! Named, UI-friendly JSON view of RDBN lists ("tables → rows → named fields").
//!
//! The [`Rdbn`](super::Rdbn) model itself serialises rows positionally (`row[field][element]`
//! with tagged values) so it can be deserialised and written back losslessly. For display,
//! [`TableView`] serialises each row as an object keyed by field name, with plain JSON
//! values (numbers, strings, arrays, nested objects for embedded records):
//!
//! ```json
//! {
//!   "name": "m_SkillConfigList", "typeName": "SKILL_CONFIG",
//!   "columns": [{"key": "id", "type": "Hash", "count": 1, "category": 3, "size": 4}, ...],
//!   "rows": [{"id": 305419896, "power": 40, "cond": null, "pos": [1.0, 2.0]}, ...]
//! }
//! ```
//! Scalars (`count == 1`) are plain values; arrays are JSON arrays; `DataTuple` is
//! `[start, count]`; raw bytes are a hex string; duplicate field names get a `#n` suffix.

use serde::ser::{SerializeMap, SerializeSeq, SerializeStruct};
use serde::{Serialize, Serializer};

use super::{Field, List, Rdbn, Type, Value};

/// A column of a [`TableView`].
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ColumnView<'a> {
    /// Unique key used in the row objects (field name, `#n`-suffixed if repeated).
    pub key: String,
    /// Human-readable type name (`Hash`, `String`, `Struct`, …).
    #[serde(rename = "type")]
    pub type_name: &'static str,
    /// Nested type name for embedded records.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub struct_type: Option<&'a str>,
    pub count: u32,
    pub category: i16,
    pub size: i32,
    /// The underlying declaration.
    #[serde(skip)]
    pub field: &'a Field,
}

/// Named view of one list. Serialises as `{name, typeName, indexed, columns, rows}`.
#[derive(Debug, Clone)]
pub struct TableView<'a> {
    pub doc: &'a Rdbn,
    pub list: &'a List,
    pub ty: &'a Type,
    pub columns: Vec<ColumnView<'a>>,
}

fn unique_keys(fields: &[Field]) -> Vec<String> {
    let mut seen = std::collections::HashMap::<&str, usize>::new();
    fields
        .iter()
        .map(|f| {
            let n = seen.entry(f.name.as_str()).or_insert(0);
            *n += 1;
            if *n == 1 {
                f.name.clone()
            } else {
                format!("{}#{}", f.name, *n)
            }
        })
        .collect()
}

impl Rdbn {
    /// Named view of list `index` (`None` if out of range or its type is missing).
    pub fn table(&self, index: usize) -> Option<TableView<'_>> {
        let list = self.lists.get(index)?;
        let ty = self.types.get(list.type_index)?;
        let columns = unique_keys(&ty.fields)
            .into_iter()
            .zip(&ty.fields)
            .map(|(key, f)| ColumnView {
                key,
                type_name: f.type_name(),
                struct_type: f
                    .is_nested()
                    .then(|| usize::try_from(f.ty).ok().and_then(|t| self.types.get(t)))
                    .flatten()
                    .map(|t| t.name.as_str()),
                count: f.count,
                category: f.category,
                size: f.size,
                field: f,
            })
            .collect();
        Some(TableView {
            doc: self,
            list,
            ty,
            columns,
        })
    }

    /// Named views of every list.
    pub fn tables(&self) -> Vec<TableView<'_>> {
        (0..self.lists.len())
            .filter_map(|i| self.table(i))
            .collect()
    }
}

impl Serialize for TableView<'_> {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        let mut st = s.serialize_struct("TableView", 5)?;
        st.serialize_field("name", &self.list.name)?;
        st.serialize_field("typeName", &self.ty.name)?;
        st.serialize_field("indexed", &self.list.indexed)?;
        st.serialize_field("columns", &self.columns)?;
        let keys: Vec<&str> = self.columns.iter().map(|c| c.key.as_str()).collect();
        st.serialize_field(
            "rows",
            &Rows {
                doc: self.doc,
                fields: &self.ty.fields,
                keys: &keys,
                rows: &self.list.rows,
            },
        )?;
        st.end()
    }
}

struct Rows<'a> {
    doc: &'a Rdbn,
    fields: &'a [Field],
    keys: &'a [&'a str],
    rows: &'a [Vec<Vec<Value>>],
}

impl Serialize for Rows<'_> {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        let mut seq = s.serialize_seq(Some(self.rows.len()))?;
        for r in self.rows {
            seq.serialize_element(&Record {
                doc: self.doc,
                fields: self.fields,
                keys: Some(self.keys),
                row: r,
            })?;
        }
        seq.end()
    }
}

/// A record (row or embedded struct) as a JSON object.
struct Record<'a> {
    doc: &'a Rdbn,
    fields: &'a [Field],
    keys: Option<&'a [&'a str]>,
    row: &'a [Vec<Value>],
}

impl Serialize for Record<'_> {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        let owned: Vec<String>;
        let keys: Vec<&str> = match self.keys {
            Some(k) => k.to_vec(),
            None => {
                owned = unique_keys(self.fields);
                owned.iter().map(String::as_str).collect()
            }
        };
        let mut m = s.serialize_map(Some(self.row.len()))?;
        for ((f, k), cell) in self.fields.iter().zip(keys.iter()).zip(self.row) {
            m.serialize_entry(
                k,
                &Cell {
                    doc: self.doc,
                    field: f,
                    elems: cell,
                },
            )?;
        }
        m.end()
    }
}

struct Cell<'a> {
    doc: &'a Rdbn,
    field: &'a Field,
    elems: &'a [Value],
}

impl Serialize for Cell<'_> {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        if self.field.count == 1 && self.elems.len() == 1 {
            return Plain {
                doc: self.doc,
                field: self.field,
                v: &self.elems[0],
            }
            .serialize(s);
        }
        let mut seq = s.serialize_seq(Some(self.elems.len()))?;
        for v in self.elems {
            seq.serialize_element(&Plain {
                doc: self.doc,
                field: self.field,
                v,
            })?;
        }
        seq.end()
    }
}

struct Plain<'a> {
    doc: &'a Rdbn,
    field: &'a Field,
    v: &'a Value,
}

impl Serialize for Plain<'_> {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        match self.v {
            Value::Bool(b) => s.serialize_bool(*b),
            Value::Byte(b) => s.serialize_u8(*b),
            Value::SByte(b) => s.serialize_i8(*b),
            Value::Short(v) => s.serialize_i16(*v),
            Value::Int(v) => s.serialize_i32(*v),
            Value::Float(v) => s.serialize_f32(*v),
            Value::Hash(v) => s.serialize_u32(*v),
            Value::Vec2(a) => a.serialize(s),
            Value::Vec4(a) => a.serialize(s),
            Value::String(v) => v.serialize(s),
            Value::DanglingString(o) => {
                let mut m = s.serialize_map(Some(1))?;
                m.serialize_entry("danglingString", o)?;
                m.end()
            }
            Value::Tuple(a) => a.serialize(s),
            Value::Raw(b) => {
                s.serialize_str(&b.iter().map(|x| format!("{x:02x}")).collect::<String>())
            }
            Value::Struct(sub) => {
                let ty = usize::try_from(self.field.ty)
                    .ok()
                    .and_then(|t| self.doc.types.get(t));
                match ty {
                    Some(t) => Record {
                        doc: self.doc,
                        fields: &t.fields,
                        keys: None,
                        row: sub,
                    }
                    .serialize(s),
                    None => sub.serialize(s),
                }
            }
        }
    }
}
