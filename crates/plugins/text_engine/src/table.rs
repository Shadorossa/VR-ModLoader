//! A game text table (`data/common/text/<lang>/*.cfg.bin`, T2B, UTF-8; docs/game/data/text-localization.md §2):
//!
//! * `TEXT_INFO_BEGIN [n]` … `TEXT_INFO(textId, variant, text[, int])` … `TEXT_INFO_END`
//! * `NOUN_INFO_BEGIN [n]` … `NOUN_INFO(textId, form, 8 string slots (only [5] used), 5 ints)` … `NOUN_INFO_END`
//!
//! Text id = crc32 of a label (`sysmes_…`, `evt_mods_t_…`) or an opaque id referenced by gamedata. `variant` is the
//! line index of multi-line texts (0 for single texts); `form` of `chara_text` nouns: 0 full name, 11 surname, 12
//! given name. New rows go before the list's END and raise the BEGIN count (no sort index in text tables; the same
//! shape the Python builders ship, e.g. research/scripts/l2r2_cycle_text_build.py, working in game).

use l5_core::hash::crc32_str;
use l5_core::t2b::{Entry, T2b, Value};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Row type of a text table.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    Text,
    Noun,
}

impl Kind {
    pub fn row(self) -> &'static str {
        match self {
            Kind::Text => "TEXT_INFO",
            Kind::Noun => "NOUN_INFO",
        }
    }
    fn begin(self) -> &'static str {
        match self {
            Kind::Text => "TEXT_INFO_BEGIN",
            Kind::Noun => "NOUN_INFO_BEGIN",
        }
    }
    fn end(self) -> &'static str {
        match self {
            Kind::Text => "TEXT_INFO_END",
            Kind::Noun => "NOUN_INFO_END",
        }
    }
    /// Index of the string value.
    pub fn slot(self) -> usize {
        match self {
            Kind::Text => 2,
            Kind::Noun => 5,
        }
    }
    pub fn parse(s: &str) -> Option<Kind> {
        match s.trim().to_ascii_lowercase().as_str() {
            "text" => Some(Kind::Text),
            "noun" | "name" => Some(Kind::Noun),
            _ => None,
        }
    }
    /// Default kind of a NEW text in `table`: nouns for the name tables, texts elsewhere.
    pub fn default_for(table: &str) -> Kind {
        match table {
            "chara_text" | "chara_text_roma" => Kind::Noun,
            _ => Kind::Text,
        }
    }
}

/// Row address inside one table.
pub type RowKey = (u32, i32);

/// A parsed text table.
#[derive(Debug, Clone)]
pub struct TextTable {
    pub doc: T2b,
    index: HashMap<(Kind, u32, i32), usize>,
}

fn kind_of_hash(h: u32) -> Option<Kind> {
    if h == crc32_str("TEXT_INFO") {
        Some(Kind::Text)
    } else if h == crc32_str("NOUN_INFO") {
        Some(Kind::Noun)
    } else {
        None
    }
}

impl TextTable {
    pub fn parse(b: &[u8]) -> Result<TextTable, String> {
        let doc = T2b::parse(b).map_err(|e| format!("not a T2B text table: {e}"))?;
        let mut t = TextTable { doc, index: HashMap::new() };
        if !t.has_list(Kind::Text) && !t.has_list(Kind::Noun) {
            return Err("not a text table (no TEXT_INFO / NOUN_INFO list)".into());
        }
        t.reindex();
        Ok(t)
    }

    fn reindex(&mut self) {
        self.index.clear();
        for (i, e) in self.doc.entries.iter().enumerate() {
            if let (Some(k), Some(id), Some(v)) = (kind_of_hash(e.hash), e.values.first().and_then(Value::as_int), e.values.get(1).and_then(Value::as_int)) {
                self.index.entry((k, id as u32, v)).or_insert(i);
            }
        }
    }

    pub fn has_list(&self, k: Kind) -> bool {
        let b = crc32_str(k.begin());
        self.doc.entries.iter().any(|e| e.hash == b)
    }

    /// Row of `(id, variant)`; `kind` None = a text row first, else a noun row.
    pub fn find(&self, kind: Option<Kind>, id: u32, variant: i32) -> Option<(Kind, usize)> {
        let ks: &[Kind] = match kind {
            Some(Kind::Text) => &[Kind::Text],
            Some(Kind::Noun) => &[Kind::Noun],
            None => &[Kind::Text, Kind::Noun],
        };
        ks.iter().find_map(|&k| self.index.get(&(k, id, variant)).map(|&i| (k, i)))
    }

    /// Every row id (both kinds).
    pub fn ids(&self) -> impl Iterator<Item = u32> + '_ {
        self.index.keys().map(|(_, id, _)| *id)
    }

    pub fn rows(&self) -> usize {
        self.index.len()
    }

    /// The string of row `idx` (None for null strings / non-rows).
    pub fn text(&self, idx: usize) -> Option<&str> {
        let e = self.doc.entries.get(idx)?;
        e.values.get(kind_of_hash(e.hash)?.slot())?.as_str()
    }

    /// Set the string of row `idx`; false when unchanged or not a row.
    pub fn set_text(&mut self, idx: usize, s: &str) -> bool {
        let Some(e) = self.doc.entries.get_mut(idx) else { return false };
        let Some(k) = kind_of_hash(e.hash) else { return false };
        let slot = k.slot();
        if e.values.len() <= slot {
            e.values.resize(slot + 1, Value::String(None));
        }
        if e.values[slot].as_str() == Some(s) {
            return false;
        }
        e.values[slot] = Value::String(Some(s.to_string()));
        true
    }

    /// Append rows `(id, variant, text)` of `kind` at the end of its list (before END; BEGIN count raised). Rows are
    /// cloned from the first row of the list (keeps the file's value layout, e.g. `iisi`), else a default shape.
    pub fn add_rows(&mut self, kind: Kind, rows: &[(u32, i32, String)]) -> Result<(), String> {
        if rows.is_empty() {
            return Ok(());
        }
        let (hb, he, hr) = (crc32_str(kind.begin()), crc32_str(kind.end()), crc32_str(kind.row()));
        let entries = &self.doc.entries;
        let b = entries.iter().position(|e| e.hash == hb).ok_or_else(|| format!("the table has no {} list", kind.row()))?;
        let e_idx = entries[b..].iter().position(|e| e.hash == he).map(|i| b + i).ok_or_else(|| format!("{} without {}", kind.begin(), kind.end()))?;
        let proto: Entry = match entries[b..e_idx].iter().find(|e| e.hash == hr) {
            Some(p) => p.clone(),
            None => Entry {
                name: Some(kind.row().to_string()),
                hash: hr,
                values: match kind {
                    Kind::Text => vec![Value::Int(0), Value::Int(0), Value::String(None)],
                    Kind::Noun => {
                        let mut v = vec![Value::Int(0), Value::Int(0)];
                        v.extend(std::iter::repeat_n(Value::String(None), 8));
                        v.extend(std::iter::repeat_n(Value::Int(0), 5));
                        v
                    }
                },
            },
        };
        let slot = kind.slot();
        let new: Vec<Entry> = rows
            .iter()
            .map(|(id, var, s)| {
                let mut e = proto.clone();
                if e.values.len() <= slot {
                    e.values.resize(slot + 1, Value::String(None));
                }
                for (i, v) in e.values.iter_mut().enumerate() {
                    *v = match (i, &*v) {
                        (0, _) => Value::Int(*id as i32),
                        (1, _) => Value::Int(*var),
                        (i, _) if i == slot => Value::String(Some(s.clone())),
                        (_, Value::String(_)) => Value::String(None),
                        (_, Value::Int(_)) => Value::Int(0),
                        (_, Value::Float(_)) => Value::Float(0.0),
                    };
                }
                e
            })
            .collect();
        let n = new.len() as i32;
        self.doc.entries.splice(e_idx..e_idx, new);
        let begin = &mut self.doc.entries[b];
        match begin.values.first() {
            Some(Value::Int(c)) => begin.values[0] = Value::Int(c + n),
            _ => return Err(format!("{} has no count", kind.begin())),
        }
        self.reindex();
        Ok(())
    }

    pub fn to_bytes(&self) -> Result<Vec<u8>, String> {
        self.doc.to_bytes().map_err(|e| e.to_string())
    }
}

/// Pad a T2B file to exactly `size` bytes: the value-string pool grows by a zero tail (the header's string length
/// counts it; no string points there), the key table and footer follow unchanged. The same trick the audio engine
/// uses on RDBN (`sound_queue_sheet`); readers look strings up by offset and find the key table at
/// `align16(stringOffset + stringLength)`. `size` and the file length must be multiples of 16.
pub fn pad_t2b(bytes: &[u8], size: usize) -> Result<Vec<u8>, String> {
    if bytes.len() == size {
        return Ok(bytes.to_vec());
    }
    if bytes.len() > size {
        return Err(format!("{} bytes do not fit a slot of {size}", bytes.len()));
    }
    if !size.is_multiple_of(16) || !bytes.len().is_multiple_of(16) || bytes.len() < 0x30 {
        return Err(format!("sizes must be multiples of 16 (file {}, slot {size})", bytes.len()));
    }
    let u32_at = |o: usize| u32::from_le_bytes(bytes[o..o + 4].try_into().expect("4 bytes"));
    let s_off = u32_at(4) as usize;
    let s_len = u32_at(8) as usize;
    let pool_end = s_off + s_len;
    let k_base = pool_end.div_ceil(16) * 16;
    if k_base > bytes.len() {
        return Err("string pool past the end of the file".into());
    }
    let tail = &bytes[k_base..];
    let new_end = size - tail.len();
    let mut out = Vec::with_capacity(size);
    out.extend_from_slice(&bytes[..pool_end]);
    out.resize(new_end, 0);
    out.extend_from_slice(tail);
    let new_len = u32::try_from(new_end - s_off).map_err(|_| "string pool too big")?;
    out[8..12].copy_from_slice(&new_len.to_le_bytes());
    debug_assert_eq!(out.len(), size);
    Ok(out)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// A small table like `menu_text`: 2 texts (one `iisi`) + 1 noun.
    pub fn sample() -> Vec<u8> {
        let e = |n: &str, v: Vec<Value>| Entry { name: Some(n.into()), hash: crc32_str(n), values: v };
        let s = |x: &str| Value::String(Some(x.into()));
        let mut noun = vec![Value::Int(30), Value::Int(0), Value::String(None), Value::String(None), Value::String(None), s("Noun")];
        noun.extend(std::iter::repeat_n(Value::String(None), 4));
        noun.extend(std::iter::repeat_n(Value::Int(0), 5));
        let doc = T2b {
            entries: vec![
                e("TEXT_INFO_BEGIN", vec![Value::Int(2)]),
                e("TEXT_INFO", vec![Value::Int(10), Value::Int(0), s("Hello")]),
                e("TEXT_INFO", vec![Value::Int(20), Value::Int(0), s("World"), Value::Int(0)]),
                e("TEXT_INFO_END", vec![]),
                e("NOUN_INFO_BEGIN", vec![Value::Int(1)]),
                e("NOUN_INFO", noun),
                e("NOUN_INFO_END", vec![]),
            ],
            ..Default::default()
        };
        doc.to_bytes().unwrap()
    }

    #[test]
    fn find_set_add_and_round_trip() {
        let mut t = TextTable::parse(&sample()).unwrap();
        assert_eq!(t.rows(), 3);
        let (k, i) = t.find(None, 20, 0).unwrap();
        assert_eq!((k, t.text(i)), (Kind::Text, Some("World")));
        assert_eq!(t.find(Some(Kind::Noun), 20, 0), None);
        let (k, i) = t.find(None, 30, 0).unwrap();
        assert_eq!((k, t.text(i)), (Kind::Noun, Some("Noun")));
        assert!(t.set_text(i, "Renamed"));
        assert!(!t.set_text(i, "Renamed"));
        t.add_rows(Kind::Text, &[(40, 0, "New 1".into()), (41, 0, "New 2".into())]).unwrap();
        t.add_rows(Kind::Noun, &[(50, 11, "Surname".into())]).unwrap();
        let b = t.to_bytes().unwrap();
        let t2 = TextTable::parse(&b).unwrap();
        assert_eq!(t2.rows(), 6);
        assert_eq!(t2.text(t2.find(None, 41, 0).unwrap().1), Some("New 2"));
        assert_eq!(t2.text(t2.find(Some(Kind::Noun), 50, 11).unwrap().1), Some("Surname"));
        assert_eq!(t2.text(t2.find(None, 30, 0).unwrap().1), Some("Renamed"));
        let c = |n: &str| t2.doc.entries.iter().find(|e| e.name.as_deref() == Some(n)).unwrap().values[0].clone();
        assert_eq!((c("TEXT_INFO_BEGIN"), c("NOUN_INFO_BEGIN")), (Value::Int(4), Value::Int(2)));
        // the new text rows copy the first row's layout (3 values), END stays last of its list
        let names: Vec<&str> = t2.doc.entries.iter().map(|e| e.name.as_deref().unwrap()).collect();
        assert_eq!(names[..6], ["TEXT_INFO_BEGIN", "TEXT_INFO", "TEXT_INFO", "TEXT_INFO", "TEXT_INFO", "TEXT_INFO_END"]);
        // a table without nouns cannot take one
        let mut only_text = TextTable::parse(&sample()).unwrap();
        only_text.doc.entries.truncate(4);
        assert!(only_text.add_rows(Kind::Noun, &[(1, 0, "x".into())]).is_err());
    }

    #[test]
    fn padding_keeps_the_table() {
        let b = sample();
        let size = (b.len() + 4096).div_ceil(4096) * 4096;
        let p = pad_t2b(&b, size).unwrap();
        assert_eq!(p.len(), size);
        let t = TextTable::parse(&p).unwrap();
        let o = TextTable::parse(&b).unwrap();
        assert_eq!(t.doc.entries, o.doc.entries);
        // a padded file is a valid base again and re-pads to itself
        assert_eq!(pad_t2b(&t.to_bytes().unwrap(), size).unwrap(), p);
        assert!(pad_t2b(&b, b.len() - 16).is_err());
        assert!(pad_t2b(&b, size + 3).is_err());
    }
}
