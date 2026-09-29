//! Readable text keys (text-specific). A key names one text row:
//!
//! | Form | Meaning |
//! |---|---|
//! | `chara.<chara>.<field>` | a character's name texts through `chara_base` (`<chara>` = `c01000010` or its id): `name` (full, `chara_text` form 0), `surname` (form 11), `given` (form 12), `short`, `upper`, `description` (`chara_description_text`) |
//! | `<table>:<label or id>[#n]` | a row of `text/<lang>/<table>.cfg.bin`: `menu_text:sysmes_foo` (id = crc32 of the label), `system_text:1389146809`, `skill_text:0x0323535B`; `#n` = variant / form (default 0) |
//! | `<mod id>.<name>` | a text a mod added under `[new]` |
//! | `<label or id>[#n]` | a row in any root table of the language (searched: menu_text, system_text, …) |
//!
//! Ids are unsigned 32-bit (a negative decimal is the same id, as the game's signed cells).

use crate::lang;
use crate::table::Kind;
use l5_core::hash::crc32_str;
use std::collections::HashMap;

/// A name field of a character.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CharaField {
    Name,
    Surname,
    Given,
    Short,
    Upper,
    Description,
}

impl CharaField {
    pub fn parse(s: &str) -> Option<CharaField> {
        Some(match s {
            "name" | "full" | "full_name" => CharaField::Name,
            "surname" | "last" | "last_name" => CharaField::Surname,
            "given" | "first" | "first_name" => CharaField::Given,
            "short" | "short_name" => CharaField::Short,
            "upper" | "upper_name" => CharaField::Upper,
            "description" | "desc" => CharaField::Description,
            _ => return None,
        })
    }

    /// `(table, kind, id, variant)` of the field for a `chara_base` row.
    pub fn target(self, n: &CharaNames) -> (&'static str, Kind, u32, i32) {
        match self {
            CharaField::Name => ("chara_text", Kind::Noun, n.name, 0),
            CharaField::Surname => ("chara_text", Kind::Noun, n.name, 11),
            CharaField::Given => ("chara_text", Kind::Noun, n.name, 12),
            CharaField::Short => ("chara_text", Kind::Noun, n.short, 0),
            CharaField::Upper => ("chara_text", Kind::Noun, n.upper, 0),
            CharaField::Description => ("chara_description_text", Kind::Text, n.desc, 0),
        }
    }

    /// Table of the field (without `chara_base`).
    pub fn table(self) -> &'static str {
        match self {
            CharaField::Description => "chara_description_text",
            _ => "chara_text",
        }
    }
}

/// A parsed key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KeyRef {
    Alias { chara: String, field: CharaField },
    Table { table: String, id: u32, variant: i32 },
    /// A text of a mod (`<mod id>.<name>`).
    Mod(String),
    Bare { id: u32, variant: i32 },
}

/// A number (`123`, `-123`, `0x7B`) → that id; anything else → crc32 of the label (the framework's game-id rule).
pub use vr_framework::ids::parse_id;

fn split_variant(s: &str) -> Result<(&str, i32), String> {
    match s.rsplit_once('#') {
        Some((a, v)) => v.trim().parse::<i32>().map(|n| (a, n)).map_err(|_| format!("`#{v}`: the variant / form is a number")),
        None => Ok((s, 0)),
    }
}

/// Parse `key`; `is_mod_key` tells whether a dotted key is a known mod text.
pub fn parse_key(key: &str, is_mod_key: &dyn Fn(&str) -> bool) -> Result<KeyRef, String> {
    let k = key.trim();
    if k.is_empty() {
        return Err("empty key".into());
    }
    if let Some(rest) = k.strip_prefix("chara.") {
        if let Some((chara, field)) = rest.rsplit_once('.') {
            if let Some(f) = CharaField::parse(field) {
                if chara.is_empty() {
                    return Err(format!("`{k}`: chara.<c01000010>.{field}"));
                }
                return Ok(KeyRef::Alias { chara: chara.to_string(), field: f });
            }
        }
    }
    if let Some((table, id)) = k.split_once(':') {
        if !lang::valid_table(table) {
            return Err(format!("`{k}`: table `{table}` (lower-case names like menu_text, event/ev00_00010)"));
        }
        let (id, variant) = split_variant(id)?;
        if id.trim().is_empty() {
            return Err(format!("`{k}`: no id / label after `:`"));
        }
        return Ok(KeyRef::Table { table: table.to_string(), id: parse_id(id), variant });
    }
    if is_mod_key(k) {
        return Ok(KeyRef::Mod(k.to_string()));
    }
    let (id, variant) = split_variant(k)?;
    Ok(KeyRef::Bare { id: parse_id(id), variant })
}

/// Text ids of one `chara_base` row (`CHARA_BASE_INFO[3]`, `[4]`, `[5]`, `[19]`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CharaNames {
    pub name: u32,
    pub short: u32,
    pub upper: u32,
    pub desc: u32,
}

/// `chara_base` by character id (`crc32("c01000010")`).
#[derive(Debug, Clone, Default)]
pub struct CharaIndex {
    map: HashMap<u32, CharaNames>,
}

impl CharaIndex {
    pub fn from_chara_base(b: &[u8]) -> Result<CharaIndex, String> {
        let doc = l5_core::t2b::T2b::parse(b).map_err(|e| format!("chara_base: {e}"))?;
        let h = crc32_str("CHARA_BASE_INFO");
        let mut map = HashMap::new();
        for e in doc.entries.iter().filter(|e| e.hash == h) {
            let v = |i: usize| e.values.get(i).and_then(|x| x.as_int()).map(|x| x as u32);
            if let (Some(id), Some(name), Some(short), Some(upper), Some(desc)) = (v(0), v(3), v(4), v(5), v(19)) {
                map.entry(id).or_insert(CharaNames { name, short, upper, desc });
            }
        }
        if map.is_empty() {
            return Err("chara_base: no CHARA_BASE_INFO rows".into());
        }
        Ok(CharaIndex { map })
    }

    /// Row of `chara` (key `c01000010` or an id).
    pub fn get(&self, chara: &str) -> Option<&CharaNames> {
        self.map.get(&parse_id(chara))
    }

    pub fn len(&self) -> usize {
        self.map.len()
    }
    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }
}

/// `(table, kind hint, id, variant)`.
pub type Target = (String, Option<Kind>, u32, i32);

/// Resolve a table-or-alias key to `(table, kind hint, id, variant)` (None for mod / bare keys). An alias needs the
/// `chara_base` index (`Err` with the reason when it could not be loaded).
pub fn target_of(k: &KeyRef, chara: Result<&CharaIndex, &str>) -> Result<Option<Target>, String> {
    Ok(match k {
        KeyRef::Alias { chara: c, field } => {
            let idx = chara.map_err(|e| format!("chara_base not available ({e})"))?;
            let n = idx.get(c).ok_or_else(|| format!("character `{c}` is not in chara_base"))?;
            let (t, kind, id, v) = field.target(n);
            Some((t.to_string(), Some(kind), id, v))
        }
        KeyRef::Table { table, id, variant } => Some((table.clone(), None, *id, *variant)),
        KeyRef::Mod(_) | KeyRef::Bare { .. } => None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn key_grammar() {
        let none = |_: &str| false;
        assert_eq!(parse_key("chara.c01000010.name", &none).unwrap(), KeyRef::Alias { chara: "c01000010".into(), field: CharaField::Name });
        assert_eq!(parse_key("chara.c01000010.surname", &none).unwrap(), KeyRef::Alias { chara: "c01000010".into(), field: CharaField::Surname });
        assert_eq!(
            parse_key("system_text:sysmes_notification_log_get_item", &none).unwrap(),
            KeyRef::Table { table: "system_text".into(), id: 1389146809, variant: 0 }
        );
        assert_eq!(parse_key("chara_text:-447611118#11", &none).unwrap(), KeyRef::Table { table: "chara_text".into(), id: (-447611118i32) as u32, variant: 11 });
        assert_eq!(parse_key("menu_text:0x10", &none).unwrap(), KeyRef::Table { table: "menu_text".into(), id: 16, variant: 0 });
        assert_eq!(parse_key("mymod.greeting", &|k| k == "mymod.greeting").unwrap(), KeyRef::Mod("mymod.greeting".into()));
        assert_eq!(parse_key("mymod.greeting", &none).unwrap(), KeyRef::Bare { id: crc32_str("mymod.greeting"), variant: 0 });
        assert_eq!(parse_key("1389146809", &none).unwrap(), KeyRef::Bare { id: 1389146809, variant: 0 });
        // chara.<x>.<not a field> is a plain key
        assert!(matches!(parse_key("chara.c01000010.hair", &none).unwrap(), KeyRef::Bare { .. }));
        assert!(parse_key("Menu:1", &none).is_err());
        assert!(parse_key("menu_text:", &none).is_err());
        assert!(parse_key("menu_text:1#x", &none).is_err());
        assert!(parse_key("", &none).is_err());
        assert_eq!(parse_id("-1"), u32::MAX);
        assert_eq!(parse_id("ev00_00010_010_010"), 1683216631);
    }
}
