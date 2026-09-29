//! Run-time lookups for Lua and other plugins (`CMND_EVT_TEXT_ID`, `CMND_EVT_TEXT_GET`, the DLL exports): the build
//! [`Index`] (new keys, the rows mods set) first, then the served / base tables, loaded on first use and kept.

use crate::build::{Index, Tables};
use crate::keys::{self, CharaIndex, KeyRef};
use crate::lang::{self, ROOT_TABLES};
use crate::table::TextTable;
use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;

/// What a lookup is given.
#[derive(Debug, Clone, PartialEq)]
pub enum Query<'a> {
    Key(&'a str),
    Id(u32),
}

/// Where tables come from at run time (the served slot when there is one, else the base).
pub trait Lazy: Send {
    fn table(&mut self, lang: &str, table: &str) -> Option<Vec<u8>>;
    fn chara_base(&mut self) -> Option<Vec<u8>>;
}

/// Lazy loader over the files: served slots, else a [`Tables`] factory (game / other mods).
pub struct FileLazy<T: Tables + Send> {
    pub served: BTreeMap<(String, String), PathBuf>,
    pub base: T,
}

impl<T: Tables + Send> Lazy for FileLazy<T> {
    fn table(&mut self, lang: &str, table: &str) -> Option<Vec<u8>> {
        if let Some(p) = self.served.get(&(lang.to_string(), table.to_string())) {
            if let Ok(b) = std::fs::read(p) {
                return Some(b);
            }
        }
        self.base.table(lang, table).ok().flatten().map(|b| b.bytes)
    }
    fn chara_base(&mut self) -> Option<Vec<u8>> {
        self.base.chara_base().ok()
    }
}

/// The run-time index.
pub struct Runtime {
    pub index: Index,
    lazy: std::sync::Mutex<LazyState>,
}

struct LazyState {
    src: Box<dyn Lazy>,
    tables: HashMap<(String, String), Option<TextTable>>,
    chara: Option<Option<CharaIndex>>,
}

impl Runtime {
    pub fn new(index: Index, src: Box<dyn Lazy>) -> Runtime {
        Runtime { index, lazy: std::sync::Mutex::new(LazyState { src, tables: HashMap::new(), chara: None }) }
    }

    fn parse(&self, key: &str) -> Result<KeyRef, String> {
        keys::parse_key(key, &|k| self.index.keys.contains_key(k))
    }

    /// Id of a key (any form of [`crate::keys`]); None = unknown mod key / character, or a bad key.
    pub fn text_id(&self, key: &str) -> Option<u32> {
        self.locate(key).map(|(_, id, _)| id)
    }

    /// `(table if known, id, variant)` of a key.
    pub fn locate(&self, key: &str) -> Option<(Option<String>, u32, i32)> {
        if let Some(k) = self.index.keys.get(key) {
            return Some((Some(k.table.clone()), k.id, 0));
        }
        if let Some(r) = self.index.refs.get(key) {
            return Some((Some(r.table.clone()), r.id, r.variant));
        }
        match self.parse(key).ok()? {
            KeyRef::Mod(k) => self.index.keys.get(&k).map(|k| (Some(k.table.clone()), k.id, 0)),
            KeyRef::Table { table, id, variant } => Some((Some(table), id, variant)),
            KeyRef::Bare { id, variant } => Some((None, id, variant)),
            KeyRef::Alias { chara, field } => {
                let mut st = self.lazy.lock().unwrap_or_else(|e| e.into_inner());
                let st = &mut *st;
                if st.chara.is_none() {
                    st.chara = Some(st.src.chara_base().and_then(|b| CharaIndex::from_chara_base(&b).ok()));
                }
                let n = st.chara.as_ref().and_then(|c| c.as_ref()).and_then(|c| c.get(&chara).copied())?;
                let (t, _, id, v) = field.target(&n);
                Some((Some(t.to_string()), id, v))
            }
        }
    }

    /// The text of `q` in `lang` (a language folder). Mod texts come from the index; others from the tables (a key
    /// without a table searches the root tables of the language).
    pub fn text_get(&self, q: Query, lang: &str) -> Option<String> {
        let lang = lang::norm(lang)?;
        let (table, id, variant) = match q {
            Query::Key(k) => self.locate(k)?,
            Query::Id(id) => (None, id, 0),
        };
        if let Some(s) = self.index.text(lang, table.as_deref(), id, variant) {
            return Some(s.to_string());
        }
        let mut st = self.lazy.lock().unwrap_or_else(|e| e.into_inner());
        let st = &mut *st;
        let candidates: Vec<String> = match table {
            Some(t) => vec![t],
            None => ROOT_TABLES.iter().map(|t| t.to_string()).collect(),
        };
        for t in candidates {
            let k = (lang.to_string(), t.clone());
            if !st.tables.contains_key(&k) {
                let parsed = st.src.table(lang, &t).and_then(|b| TextTable::parse(&b).ok());
                st.tables.insert(k.clone(), parsed);
            }
            if let Some(tt) = st.tables.get(&k).and_then(Option::as_ref) {
                if let Some((_, i)) = tt.find(None, id, variant) {
                    return tt.text(i).map(str::to_string);
                }
            }
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::build::tests::{modin, Fake};

    struct FakeLazy(Fake);
    impl Lazy for FakeLazy {
        fn table(&mut self, lang: &str, table: &str) -> Option<Vec<u8>> {
            self.0.table(lang, table).ok().flatten().map(|b| b.bytes)
        }
        fn chara_base(&mut self) -> Option<Vec<u8>> {
            self.0.chara_base().ok()
        }
    }

    #[test]
    fn lookups() {
        let a = modin("moda", 0, &[("text/en.toml", "[new]\ngreeting = \"Hello\"\n[replace]\n\"menu_text:10\" = \"Hi\"\n"), ("text/es.toml", "[new]\ngreeting = \"Hola\"\n")]);
        let built = crate::build::build(&[a], &mut Fake { missing: vec![], reads: 0 });
        let rt = Runtime::new(built.index, Box::new(FakeLazy(Fake { missing: vec![], reads: 0 })));
        let gid = l5_core::hash::crc32_str("moda.greeting");
        assert_eq!(rt.text_id("moda.greeting"), Some(gid));
        assert_eq!(rt.text_id("chara.c01000010.name"), Some(0xE5530F12));
        assert_eq!(rt.text_id("chara.c09999999.name"), None);
        assert_eq!(rt.text_id("menu_text:sysmes_x"), Some(l5_core::hash::crc32_str("sysmes_x")));
        assert_eq!(rt.text_id("Bad:1"), None);
        assert_eq!(rt.text_get(Query::Key("moda.greeting"), "es").as_deref(), Some("Hola"));
        assert_eq!(rt.text_get(Query::Key("moda.greeting"), "zh-Hant").as_deref(), Some("Hello"));
        assert_eq!(rt.text_get(Query::Id(gid), "fr").as_deref(), Some("Hello"));
        // retail rows: from the tables (menu_text:10 was set by the mod → index; 20 → the table)
        assert_eq!(rt.text_get(Query::Key("menu_text:10"), "de").as_deref(), Some("Hi"));
        assert_eq!(rt.text_get(Query::Id(20), "de").as_deref(), Some("World"));
        assert_eq!(rt.text_get(Query::Key("chara.c01000010.surname"), "en").as_deref(), Some("Evans"));
        assert_eq!(rt.text_get(Query::Id(12345), "en"), None);
        assert_eq!(rt.text_get(Query::Id(20), "xx"), None);
    }
}
