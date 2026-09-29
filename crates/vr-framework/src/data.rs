//! Strict TOML data files. The rules (`crates/vr-framework/README.md`, «Format rules»):
//!
//! * every data file starts with its **schema**: `schema = "<kind>/<version>"` (`"example_engine.item/1"`,
//!   `"vr_framework.character/1"`), a top-level line before any `[table]`; the engine accepts versions
//!   `1..=<its version>` of its kind and refuses a newer one ("update the engine") or another kind;
//! * **unknown keys are errors** naming the file, the line and the key (a typo never goes silently unused);
//! * keys and values are English, own ids are `<mod_id>.<name>` ([`crate::ids`]).
//!
//! The engine describes a file with a serde type and **must** put `#[serde(deny_unknown_fields)]` on every struct of
//! it (serde is where unknown keys are detected); `schema` is handled here, so the type does not declare it:
//!
//! ```
//! #[derive(Debug, serde::Deserialize)]
//! #[serde(deny_unknown_fields)]
//! struct Item {
//!     name: String,
//!     #[serde(default)]
//!     price: u32,
//! }
//! let text = "schema = \"example_engine.item/1\"\nname = \"Potion\"\nprise = 3\n";
//! let err = vr_framework::data::parse_strict::<Item>("mods/a/example/potion.toml", text, "example_engine.item", 1).unwrap_err();
//! assert_eq!(err.line, Some(3));
//! assert_eq!(err.key.as_deref(), Some("prise"));
//! ```

use crate::diag::{line_of, Diagnostic, Notes};
use crate::ModDir;
use serde::de::DeserializeOwned;
use std::fmt;

/// A parsed `schema` value: `<kind>/<version>`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Schema {
    /// `<namespace>.<file kind>`: lower-case ASCII, digits, `_`, `.`.
    pub kind: String,
    /// 1, 2, …
    pub version: u32,
}

impl Schema {
    pub fn parse(s: &str) -> Option<Schema> {
        let (kind, v) = s.trim().rsplit_once('/')?;
        let ok = !kind.is_empty() && kind.bytes().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'_' || c == b'.');
        let version: u32 = v.parse().ok().filter(|&n| n >= 1)?;
        ok.then(|| Schema { kind: kind.to_string(), version })
    }
}

impl fmt::Display for Schema {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}/{}", self.kind, self.version)
    }
}

/// One loaded data file.
#[derive(Debug, Clone, PartialEq)]
pub struct DataFile<T> {
    pub mod_id: String,
    pub load_index: u32,
    /// `mods/<mod>/<dir>/<file>.toml` (for messages).
    pub file: String,
    /// The version the file declares (<= the engine's).
    pub version: u32,
    pub value: T,
}

/// The key a line of TOML sets (`key = …`, `"key" = …`), if any.
fn line_key(line: &str) -> Option<&str> {
    let t = line.trim_start();
    let (k, rest) = if let Some(r) = t.strip_prefix('"') {
        let e = r.find('"')?;
        (&r[..e], &r[e + 1..])
    } else if let Some(r) = t.strip_prefix('\'') {
        let e = r.find('\'')?;
        (&r[..e], &r[e + 1..])
    } else {
        let e = t.find(|c: char| !(c.is_ascii_alphanumeric() || c == '_' || c == '-'))?;
        (&t[..e], &t[e..])
    };
    rest.trim_start().starts_with('=').then_some(k)
}

/// Line (1-based) and byte range of the top-level `schema` line (before any `[table]`).
fn schema_line(text: &str) -> Option<(usize, std::ops::Range<usize>)> {
    let mut at = 0;
    for (i, line) in text.split_inclusive('\n').enumerate() {
        let body = line.trim_end_matches(['\n', '\r']);
        if body.trim_start().starts_with('[') {
            return None;
        }
        if line_key(body) == Some("schema") {
            return Some((i + 1, at..at + body.len()));
        }
        at += line.len();
    }
    None
}

/// First line that sets `key` (fallback position of an error without a span).
fn find_key_line(text: &str, key: &str) -> Option<usize> {
    text.lines().position(|l| line_key(l) == Some(key)).map(|i| i + 1)
}

/// A serde / TOML error as a diagnostic: `unknown field` → unknown key, `missing field` → missing key.
fn toml_diag(file: &str, text: &str, e: &toml::de::Error) -> Diagnostic {
    let msg = e.message().trim().to_string();
    let quoted = |m: &str, p: &str| m.strip_prefix(p).and_then(|r| r.strip_prefix('`')).and_then(|r| r.split_once('`')).map(|(k, rest)| (k.to_string(), rest.to_string()));
    let (key, message) = if let Some((k, rest)) = quoted(&msg, "unknown field ") {
        let expected = rest.trim_start_matches(',').trim();
        let m = if expected.is_empty() { format!("unknown key `{k}`") } else { format!("unknown key `{k}` ({expected})") };
        (Some(k), m)
    } else if let Some((k, _)) = quoted(&msg, "missing field ") {
        (Some(k.clone()), format!("missing key `{k}`"))
    } else if let Some((k, rest)) = quoted(&msg, "unknown variant ") {
        (None, format!("unknown value `{k}`{rest}"))
    } else {
        (None, msg)
    };
    let line = match e.span() {
        Some(s) => Some(line_of(text, s.start)),
        None => key.as_deref().and_then(|k| find_key_line(text, k)),
    };
    Diagnostic::error(file, line, key, message)
}

/// Parse one data file strictly (see the module doc). `file` names it in messages; `kind` / `version` = what the
/// engine reads (versions `1..=version`). Ok = `(declared version, value)`.
pub fn parse_strict<T: DeserializeOwned>(file: &str, text: &str, kind: &str, version: u32) -> Result<(u32, T), Diagnostic> {
    let table: toml::Table = toml::from_str(text).map_err(|e| toml_diag(file, text, &e))?;
    let want = format!("schema = \"{kind}/{version}\"");
    let Some(v) = table.get("schema") else {
        return Err(Diagnostic::error(file, Some(1), Some("schema".into()), format!("no schema: the file must start with `{want}`")));
    };
    let Some((line, range)) = schema_line(text) else {
        return Err(Diagnostic::error(file, find_key_line(text, "schema"), Some("schema".into()), format!("`schema` must be a line of its own before any [table] (`{want}`)")));
    };
    let s = v.as_str().and_then(Schema::parse).ok_or_else(|| Diagnostic::error(file, Some(line), Some("schema".into()), format!("schema must be \"<kind>/<version>\" (`{want}`)")))?;
    if s.kind != kind {
        return Err(Diagnostic::error(file, Some(line), Some("schema".into()), format!("schema `{s}` is not a {kind} file (`{want}`)")));
    }
    if s.version > version {
        return Err(Diagnostic::error(
            file,
            Some(line),
            Some("schema".into()),
            format!("schema version {} is newer than this engine reads ({kind} 1..={version}): update the engine", s.version),
        ));
    }
    // the rest without the schema line (same line numbers): the engine's type does not declare `schema`
    let mut rest = String::with_capacity(text.len());
    rest.push_str(&text[..range.start]);
    rest.push_str(&text[range.end..]);
    let value = toml::from_str::<T>(&rest).map_err(|e| toml_diag(file, &rest, &e))?;
    Ok((s.version, value))
}

/// Every `<mod>\<dir>\*.toml` of the mods (load order; files by name, `_*.toml` skipped), parsed strictly. A file
/// that fails is left out with an error note (`mods/<mod>/<dir>/<file>:<line>: …`); the others still load.
pub fn load_dir<T: DeserializeOwned>(mods: &[ModDir], dir: &str, kind: &str, version: u32, notes: &mut Notes) -> Vec<DataFile<T>> {
    let mut out = Vec::new();
    for m in crate::in_load_order(mods) {
        for p in crate::discover::toml_files(&m.dir.join(dir)) {
            let file = format!("mods/{}/{dir}/{}", m.id, p.file_name().unwrap_or_default().to_string_lossy());
            let text = match std::fs::read_to_string(&p) {
                Ok(t) => t,
                Err(e) => {
                    notes.error(format!("{file}: not readable ({e})"));
                    continue;
                }
            };
            match parse_strict::<T>(&file, &text, kind, version) {
                Ok((v, value)) => out.push(DataFile { mod_id: m.id.clone(), load_index: m.load_index, file, version: v, value }),
                Err(d) => notes.diag(&d),
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::Deserialize;

    #[derive(Debug, Deserialize, PartialEq)]
    #[serde(deny_unknown_fields)]
    struct Item {
        name: String,
        #[serde(default)]
        price: u32,
        #[serde(default)]
        tags: Vec<Tag>,
    }

    #[derive(Debug, Deserialize, PartialEq)]
    #[serde(deny_unknown_fields)]
    struct Tag {
        id: String,
        #[serde(default)]
        kind: Kind,
    }

    #[derive(Debug, Deserialize, PartialEq, Default)]
    #[serde(rename_all = "lowercase")]
    enum Kind {
        #[default]
        Fire,
        Wind,
    }

    fn p(text: &str) -> Result<(u32, Item), Diagnostic> {
        parse_strict::<Item>("mods/a/example/x.toml", text, "example_engine.item", 2)
    }

    #[test]
    fn schema_rules() {
        assert_eq!(Schema::parse("vr_framework.character/1"), Some(Schema { kind: "vr_framework.character".into(), version: 1 }));
        for bad in ["x", "x/0", "X/1", "/1", "a/b"] {
            assert_eq!(Schema::parse(bad), None, "{bad}");
        }
        let (v, it) = p("# comment\nschema = \"example_engine.item/1\"\nname = \"Potion\"\n").unwrap();
        assert_eq!((v, it.name.as_str(), it.price), (1, "Potion", 0));
        let e = p("name = \"x\"\n").unwrap_err();
        assert_eq!((e.line, e.key.as_deref()), (Some(1), Some("schema")));
        assert!(e.message.contains("schema = \"example_engine.item/2\""), "{e}");
        let e = p("schema = \"example_engine.item/3\"\nname = \"x\"\n").unwrap_err();
        assert!(e.message.contains("update the engine"), "{e}");
        let e = p("schema = \"other.item/1\"\nname = \"x\"\n").unwrap_err();
        assert!(e.message.contains("is not a example_engine.item file"), "{e}");
        let e = p("name = \"x\"\n[t]\nschema = \"example_engine.item/1\"\n").unwrap_err();
        assert_eq!(e.key.as_deref(), Some("schema"));
    }

    #[test]
    fn unknown_keys_name_file_line_and_key() {
        let e = p("schema = \"example_engine.item/2\"\nname = \"x\"\n\nprise = 3\n").unwrap_err();
        assert_eq!((e.line, e.key.as_deref()), (Some(4), Some("prise")), "{e}");
        assert!(e.to_string().starts_with("mods/a/example/x.toml:4: unknown key `prise`"), "{e}");
        // nested (array of tables)
        let e = p("schema = \"example_engine.item/2\"\nname = \"x\"\n[[tags]]\nid = \"a\"\n[[tags]]\nid = \"b\"\nkidn = \"fire\"\n").unwrap_err();
        assert_eq!((e.line, e.key.as_deref()), (Some(7), Some("kidn")), "{e}");
        assert_eq!(e.message, "unknown key `kidn` (expected `id` or `kind`)");
        // English values only: a value outside the enum
        let e = p("schema = \"example_engine.item/2\"\nname = \"x\"\n[[tags]]\nid = \"a\"\nkind = \"fuego\"\n").unwrap_err();
        assert_eq!(e.line, Some(5), "{e}");
        assert!(e.message.contains("fuego"), "{e}");
        // missing key, syntax error
        let e = p("schema = \"example_engine.item/2\"\nprice = 1\n").unwrap_err();
        assert_eq!(e.key.as_deref(), Some("name"), "{e}");
        let e = p("schema = \"example_engine.item/2\"\nname = \n").unwrap_err();
        assert_eq!(e.line, Some(2), "{e}");
    }

    #[test]
    fn load_dir_in_load_order_with_notes() {
        let d = std::env::temp_dir().join(format!("vr-fw-data-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        let w = |m: &str, f: &str, t: &str| {
            let p = d.join(m).join("example").join(f);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, t).unwrap();
        };
        w("b", "one.toml", "schema = \"example_engine.item/1\"\nname = \"B\"\n");
        w("a", "one.toml", "schema = \"example_engine.item/1\"\nname = \"A\"\n");
        w("a", "two.toml", "schema = \"example_engine.item/1\"\nnmae = \"A2\"\n");
        w("a", "_draft.toml", "not toml at all [");
        let mods = vec![ModDir::new("b", d.join("b"), 1), ModDir::new("a", d.join("a"), 0)];
        let mut n = Notes::default();
        let got = load_dir::<Item>(&mods, "example", "example_engine.item", 1, &mut n);
        assert_eq!(got.iter().map(|f| (f.mod_id.as_str(), f.value.name.as_str())).collect::<Vec<_>>(), [("a", "A"), ("b", "B")]);
        assert_eq!(n.0.len(), 1);
        assert!(n.0[0].1.starts_with("mods/a/example/two.toml:2: unknown key `nmae`"), "{:?}", n);
        let _ = std::fs::remove_dir_all(&d);
    }
}
