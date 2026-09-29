//! A mod's text declarations (text-specific schema; files found by [`crate::fw::discover`]):
//!
//! * `text.toml`: `default_lang = "en"` and per-language tables `[<lang>.new]`, `[<lang>.replace]` (`<lang>` = one of
//!   the 9 folders or `all`);
//! * `text\<lang>.toml` (or `text\all.toml`): `[new]` and `[replace]` of that language.
//!
//! `[new]`: `name = "text"` or `name = { text = "…", table = "system_text", kind = "noun" }` — a NEW text whose key
//! is `<mod id>.<name>` (the engine gives it a stable id). `[replace]`: `"<key>" = "text"` — an existing text (retail
//! or another mod's), key grammar in [`crate::keys`]. Real line breaks become the game's two-character `\n`.

use crate::fw::discover::DeclFile;
use crate::lang::{self, ALL, LANGS};
use crate::table::Kind;
use std::collections::BTreeMap;

/// Engine name: `text.toml` + `text\`.
pub const NAME: &str = "text";

/// A new text of a mod (one language).
#[derive(Debug, Clone, PartialEq)]
pub struct NewDef {
    pub text: String,
    pub table: Option<String>,
    pub kind: Option<Kind>,
}

/// One language block (or `all`).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Block {
    pub new: BTreeMap<String, NewDef>,
    pub replace: BTreeMap<String, String>,
}

/// Precedence of a value for a language (see [`crate::fw::layer`]): written for that language > written for `all` >
/// the mod's default language > (new texts only) the first language the mod wrote.
pub const TIER_LANG: u8 = 3;
pub const TIER_ALL: u8 = 2;
pub const TIER_DEFAULT: u8 = 1;
pub const TIER_ANY: u8 = 0;

/// Every text declaration of one mod.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ModText {
    pub mod_id: String,
    /// Declared `default_lang`, else `en` when written, else the first language written (canonical order); None with
    /// `default_lang = "none"` (a language the mod does not write keeps the game's text).
    pub default_lang: Option<&'static str>,
    /// Language code (or `all`) → block.
    pub blocks: BTreeMap<String, Block>,
    pub warnings: Vec<String>,
}

/// `<mod id>.<name>` (a name that already starts with `<mod id>.` is kept).
pub fn full_key(mod_id: &str, name: &str) -> String {
    if name.starts_with(&format!("{mod_id}.")) {
        name.to_string()
    } else {
        format!("{mod_id}.{name}")
    }
}

/// A name under `[new]`: 1-128 of `A-Z a-z 0-9 _ . -`, not starting / ending with `.`.
pub fn valid_new_name(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 128
        && !s.starts_with('.')
        && !s.ends_with('.')
        && s.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'.' || b == b'-')
}

/// Game form of a modder's string: real line breaks → the two characters `\n`.
pub fn game_string(s: &str) -> String {
    s.replace("\r\n", "\n").replace('\n', "\\n")
}

fn block_name(s: &str) -> Option<String> {
    if s.eq_ignore_ascii_case(ALL) {
        Some(ALL.to_string())
    } else {
        lang::norm(s).map(str::to_string)
    }
}

impl ModText {
    /// Parse the declaration files of mod `mod_id` (order: `text.toml`, then `text\*.toml`; a later file overrides the
    /// same entry of an earlier one).
    pub fn parse(mod_id: &str, files: &[DeclFile]) -> ModText {
        let mut m = ModText { mod_id: mod_id.to_string(), ..Default::default() };
        let mut declared: Option<&'static str> = None;
        let mut no_default = false;
        for f in files {
            let t: toml::Table = match f.text.parse() {
                Ok(t) => t,
                Err(e) => {
                    m.warnings.push(format!("{}: not valid TOML ({}): ignored", f.rel, e.message()));
                    continue;
                }
            };
            let stem = f.rel.strip_prefix("text/").and_then(|r| r.rsplit_once('.').map(|(s, _)| s));
            match stem {
                None => {
                    // text.toml
                    for (k, v) in &t {
                        if k == "default_lang" {
                            match (v.as_str(), v.as_str().and_then(lang::norm)) {
                                (_, Some(l)) => (declared, no_default) = (Some(l), false),
                                (Some("none"), None) => (declared, no_default) = (None, true),
                                _ => m.warnings.push(format!("{}: default_lang {v}: not one of {}, none", f.rel, LANGS.join(", "))),
                            }
                            continue;
                        }
                        let Some(b) = block_name(k) else {
                            m.warnings.push(format!("{}: unknown key `{k}` (expected default_lang or a language: {}, all)", f.rel, LANGS.join(", ")));
                            continue;
                        };
                        match v.as_table() {
                            Some(sub) => m.read_block(&f.rel, &b, sub),
                            None => m.warnings.push(format!("{}: `{k}` must be a table ([{k}.new] / [{k}.replace])", f.rel)),
                        }
                    }
                }
                Some(stem) => {
                    let Some(b) = block_name(stem) else {
                        m.warnings.push(format!("{}: file name is not a language ({}, all): ignored", f.rel, LANGS.join(", ")));
                        continue;
                    };
                    m.read_block(&f.rel, &b, &t);
                }
            }
        }
        m.default_lang = declared.or_else(|| {
            if no_default {
                None
            } else if m.blocks.contains_key("en") {
                Some("en")
            } else {
                LANGS.iter().copied().find(|l| m.blocks.contains_key(*l))
            }
        });
        m
    }

    fn read_block(&mut self, rel: &str, block: &str, t: &toml::Table) {
        for (k, v) in t {
            match k.as_str() {
                "new" => {
                    let Some(tab) = v.as_table() else {
                        self.warnings.push(format!("{rel}: [{block}] new must be a table"));
                        continue;
                    };
                    for (name, val) in tab {
                        if !valid_new_name(name) {
                            self.warnings.push(format!("{rel}: new `{name}`: names use A-Z a-z 0-9 _ . - (skipped)"));
                            continue;
                        }
                        match parse_new(val) {
                            Ok(d) => {
                                self.blocks.entry(block.to_string()).or_default().new.insert(name.clone(), d);
                            }
                            Err(e) => self.warnings.push(format!("{rel}: new `{name}`: {e} (skipped)")),
                        }
                    }
                }
                "replace" => {
                    let Some(tab) = v.as_table() else {
                        self.warnings.push(format!("{rel}: [{block}] replace must be a table"));
                        continue;
                    };
                    for (key, val) in tab {
                        match val.as_str() {
                            Some(s) => {
                                self.blocks.entry(block.to_string()).or_default().replace.insert(key.clone(), game_string(s));
                            }
                            None => self.warnings.push(format!("{rel}: replace `{key}`: the value must be a string (skipped)")),
                        }
                    }
                }
                other => self.warnings.push(format!("{rel}: unknown key `{other}` in [{block}] (expected new / replace)")),
            }
        }
    }

    /// Every name under `[new]` (any block), sorted.
    pub fn new_names(&self) -> Vec<&str> {
        let mut v: Vec<&str> = self.blocks.values().flat_map(|b| b.new.keys().map(String::as_str)).collect();
        v.sort();
        v.dedup();
        v
    }

    /// Every key under `[replace]` (any block), sorted.
    pub fn replace_keys(&self) -> Vec<&str> {
        let mut v: Vec<&str> = self.blocks.values().flat_map(|b| b.replace.keys().map(String::as_str)).collect();
        v.sort();
        v.dedup();
        v
    }

    /// Language order of the fallback chain for `lang`: (block, tier).
    fn chain<'a>(&'a self, lang: &'a str) -> Vec<(&'a str, u8)> {
        let mut c = vec![(lang, TIER_LANG), (ALL, TIER_ALL)];
        if let Some(d) = self.default_lang.filter(|d| *d != lang) {
            c.push((d, TIER_DEFAULT));
        }
        c
    }

    /// The new text `name` for `lang`: specific → `all` → default language → first language written.
    pub fn resolve_new(&self, name: &str, lang: &str) -> Option<(&NewDef, u8)> {
        for (b, tier) in self.chain(lang) {
            if let Some(d) = self.blocks.get(b).and_then(|x| x.new.get(name)) {
                return Some((d, tier));
            }
        }
        LANGS.iter().find_map(|l| self.blocks.get(*l).and_then(|x| x.new.get(name))).map(|d| (d, TIER_ANY))
    }

    /// The replacement of `key` for `lang`: specific → `all` → default language → none (the text stays as it is).
    pub fn resolve_replace(&self, key: &str, lang: &str) -> Option<(&str, u8)> {
        self.chain(lang).into_iter().find_map(|(b, tier)| self.blocks.get(b).and_then(|x| x.replace.get(key)).map(|s| (s.as_str(), tier)))
    }

    /// Table / kind of new text `name`: the first definition that sets them (canonical language order, then `all`);
    /// defaults `menu_text` / [`Kind::default_for`]. Disagreeing definitions → warning.
    pub fn new_meta(&self, name: &str) -> (String, Kind, Vec<String>) {
        let mut warn = Vec::new();
        let defs: Vec<(&str, &NewDef)> = LANGS
            .iter()
            .copied()
            .chain(std::iter::once(ALL))
            .filter_map(|l| self.blocks.get(l).and_then(|b| b.new.get(name)).map(|d| (l, d)))
            .collect();
        let table = defs.iter().find_map(|(_, d)| d.table.clone()).unwrap_or_else(|| "menu_text".to_string());
        for (l, d) in &defs {
            if let Some(t) = &d.table {
                if *t != table {
                    warn.push(format!("new `{name}`: table `{t}` in {l} differs from `{table}` (used: {table})"));
                }
            }
        }
        let kind = defs.iter().find_map(|(_, d)| d.kind).unwrap_or_else(|| Kind::default_for(&table));
        (table, kind, warn)
    }
}

fn parse_new(v: &toml::Value) -> Result<NewDef, String> {
    match v {
        toml::Value::String(s) => Ok(NewDef { text: game_string(s), table: None, kind: None }),
        toml::Value::Table(t) => {
            let text = t.get("text").and_then(|x| x.as_str()).ok_or("needs text = \"…\"")?;
            let table = match t.get("table") {
                None => None,
                Some(x) => {
                    let s = x.as_str().ok_or("table must be a string")?;
                    if !lang::valid_table(s) {
                        return Err(format!("table `{s}`: lower-case names like menu_text, event/ev00_00010"));
                    }
                    Some(s.to_string())
                }
            };
            let kind = match t.get("kind") {
                None => None,
                Some(x) => Some(x.as_str().and_then(Kind::parse).ok_or("kind is \"text\" or \"noun\"")?),
            };
            if let Some(k) = t.keys().find(|k| !["text", "table", "kind"].contains(&k.as_str())) {
                return Err(format!("unknown key `{k}` (text, table, kind)"));
            }
            Ok(NewDef { text: game_string(text), table, kind })
        }
        _ => Err("a string or { text = …, table = …, kind = … }".into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn f(rel: &str, text: &str) -> DeclFile {
        DeclFile { rel: rel.into(), text: text.into() }
    }

    #[test]
    fn both_layouts_and_fallback_chain() {
        let files = [
            f(
                "text.toml",
                r#"
default_lang = "en"
[en.new]
greeting = "Hello"
title = { text = "Title", table = "system_text" }
[all.replace]
"chara.c01000010.name" = "Mark ALL"
[bogus]
x = 1
"#,
            ),
            f("text/es.toml", "[new]\ngreeting = \"Hola\"\n[replace]\n\"menu_text:foo\" = \"\"\"Línea 1\nLínea 2\"\"\"\n"),
            f("text/fr.toml", "[replace]\n\"chara.c01000010.name\" = \"Marc\"\n"),
            f("text/xx.toml", "[new]\na = \"b\"\n"),
        ];
        let m = ModText::parse("mymod", &files);
        assert_eq!(m.default_lang, Some("en"));
        assert_eq!(m.warnings.len(), 2, "{:?}", m.warnings);
        assert_eq!(m.new_names(), ["greeting", "title"]);
        assert_eq!(m.resolve_new("greeting", "es").map(|(d, t)| (d.text.as_str(), t)), Some(("Hola", TIER_LANG)));
        assert_eq!(m.resolve_new("greeting", "de").map(|(d, t)| (d.text.as_str(), t)), Some(("Hello", TIER_DEFAULT)));
        assert_eq!(m.resolve_new("greeting", "en").map(|(_, t)| t), Some(TIER_LANG));
        assert_eq!(m.resolve_replace("chara.c01000010.name", "fr"), Some(("Marc", TIER_LANG)));
        assert_eq!(m.resolve_replace("chara.c01000010.name", "ja"), Some(("Mark ALL", TIER_ALL)));
        assert_eq!(m.resolve_replace("menu_text:foo", "es"), Some(("Línea 1\\nLínea 2", TIER_LANG)));
        assert_eq!(m.resolve_replace("menu_text:foo", "de"), None);
        let (t, k, w) = m.new_meta("title");
        assert_eq!((t.as_str(), k, w.len()), ("system_text", Kind::Text, 0));
        assert_eq!(m.new_meta("greeting").0, "menu_text");
        assert_eq!(full_key("mymod", "greeting"), "mymod.greeting");
        assert_eq!(full_key("mymod", "mymod.greeting"), "mymod.greeting");
    }

    #[test]
    fn default_language_without_declaration_and_first_language_fallback() {
        let m = ModText::parse("m", &[f("text/es.toml", "[new]\nhi = \"Hola\"\n")]);
        assert_eq!(m.default_lang, Some("es"));
        assert_eq!(m.resolve_new("hi", "ja").map(|(d, t)| (d.text.as_str(), t)), Some(("Hola", TIER_DEFAULT)));
        // declared default without that language: new texts fall back to the first language written
        let m = ModText::parse("m", &[f("text.toml", "default_lang = \"de\"\n[fr.new]\nhi = \"Salut\"\n[fr.replace]\n\"x:1\" = \"y\"\n")]);
        assert_eq!(m.resolve_new("hi", "ja").map(|(d, t)| (d.text.as_str(), t)), Some(("Salut", TIER_ANY)));
        assert_eq!(m.resolve_replace("x:1", "ja"), None);
        // default_lang = "none": no fallback for replacements (new texts still get the first language written)
        let m = ModText::parse("m", &[f("text.toml", "default_lang = \"none\"
[es.replace]
\"x:1\" = \"y\"
[es.new]
hi = \"Hola\"
")]);
        assert_eq!(m.default_lang, None);
        assert_eq!((m.resolve_replace("x:1", "es"), m.resolve_replace("x:1", "fr")), (Some(("y", TIER_LANG)), None));
        assert_eq!(m.resolve_new("hi", "fr").map(|(_, t)| t), Some(TIER_ANY));
        // bad entries
        let m = ModText::parse(
            "m",
            &[f("text/en.toml", "[new]\n\"bad key\" = \"x\"\nok = { text = \"t\", table = \"Menu\" }\nk2 = { text = \"t\", kind = \"verb\" }\n[replace]\n\"a:1\" = 3\n")],
        );
        assert_eq!(m.warnings.len(), 4, "{:?}", m.warnings);
        assert!(m.new_names().is_empty() && m.replace_keys().is_empty());
    }
}
