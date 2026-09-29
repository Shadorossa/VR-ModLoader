//! Mod options: **every setting a mod adds lives in the Opciones tab «Opciones de mods»**
//! (to the right of the graphics tab), declared in the mod's `options.toml`. The tab itself is the `mod_options`
//! component (planned); this module is the shared API: engines and mods
//! parse the declarations and read the current values the same way.
//!
//! ```toml
//! schema = "vr_framework.options/1"
//!
//! [[option]]
//! key = "shot_zone"                     # storage key (unique in the mod; lower-case English)
//! type = "toggle"                       # toggle | list | number
//! label = "my_mod.options.shot_zone"     # text key of the label (text engine), shown in the player's language
//! help = "my_mod.options.shot_zone_help" # optional text key of the help line
//! default = true
//!
//! [[option]]
//! key = "difficulty"
//! type = "list"
//! label = "my_mod.options.difficulty"
//! values = ["easy", "normal", "hard"]   # stored values (English)
//! labels = ["my_mod.options.easy", "my_mod.options.normal", "my_mod.options.hard"]   # optional text keys per value
//! default = "normal"
//!
//! [[option]]
//! key = "half_minutes"
//! type = "number"
//! label = "my_mod.options.half_minutes"
//! min = 1
//! max = 45
//! step = 1
//! default = 5
//! ```
//!
//! **Values** (reserved location, to align with `mod_options`): `evt_loader\mod_options\<mod id>.json` =
//! `{"<key>": value}`. A missing or invalid stored value reads as the default ([`ModOptions::load`]).

use crate::data::{self, DataFile};
use crate::diag::{Diagnostic, Notes};
use serde::Deserialize;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// The options file of a mod (in the mod folder).
pub const FILE: &str = "options.toml";
/// Its schema kind (`schema = "vr_framework.options/1"`).
pub const KIND: &str = "vr_framework.options";
pub const VERSION: u32 = 1;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawFile {
    #[serde(default)]
    option: Vec<RawOption>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawOption {
    key: String,
    #[serde(rename = "type")]
    kind: RawKind,
    label: String,
    #[serde(default)]
    help: Option<String>,
    default: toml::Value,
    #[serde(default)]
    values: Vec<String>,
    #[serde(default)]
    labels: Vec<String>,
    #[serde(default)]
    min: Option<f64>,
    #[serde(default)]
    max: Option<f64>,
    #[serde(default)]
    step: Option<f64>,
}

#[derive(Debug, Deserialize, Clone, Copy)]
#[serde(rename_all = "lowercase")]
enum RawKind {
    Toggle,
    List,
    Number,
}

/// A value of an option.
#[derive(Debug, Clone, PartialEq)]
pub enum OptionValue {
    Toggle(bool),
    /// One of the list's `values`.
    List(String),
    Number(f64),
}

impl OptionValue {
    pub fn as_bool(&self) -> Option<bool> {
        if let OptionValue::Toggle(b) = self { Some(*b) } else { None }
    }
    pub fn as_str(&self) -> Option<&str> {
        if let OptionValue::List(s) = self { Some(s) } else { None }
    }
    pub fn as_f64(&self) -> Option<f64> {
        if let OptionValue::Number(n) = self { Some(*n) } else { None }
    }
    fn to_json(&self) -> serde_json::Value {
        match self {
            OptionValue::Toggle(b) => (*b).into(),
            OptionValue::List(s) => s.clone().into(),
            OptionValue::Number(n) => crate::lua::num_value(*n).unwrap_or(serde_json::Value::Null),
        }
    }
}

/// What kind of control and its limits.
#[derive(Debug, Clone, PartialEq)]
pub enum OptionKind {
    Toggle,
    /// `values` (stored, English) and optional text keys per value (same length).
    List { values: Vec<String>, labels: Vec<String> },
    Number { min: f64, max: f64, step: f64 },
}

/// One declared option.
#[derive(Debug, Clone, PartialEq)]
pub struct OptionSpec {
    /// Storage key (unique in the mod).
    pub key: String,
    /// Text keys (text engine) of the label and the help line.
    pub label: String,
    pub help: Option<String>,
    pub kind: OptionKind,
    pub default: OptionValue,
}

impl OptionSpec {
    /// `v` as a valid value of this option (None = wrong type, not in the list, out of range).
    pub fn accept(&self, v: &serde_json::Value) -> Option<OptionValue> {
        match &self.kind {
            OptionKind::Toggle => v.as_bool().map(OptionValue::Toggle),
            OptionKind::List { values, .. } => v.as_str().filter(|s| values.iter().any(|x| x == s)).map(|s| OptionValue::List(s.to_string())),
            OptionKind::Number { min, max, .. } => v.as_f64().filter(|n| (*min..=*max).contains(n)).map(OptionValue::Number),
        }
    }
}

fn spec(o: RawOption) -> Result<OptionSpec, String> {
    if !crate::ids::valid_name(&o.key) {
        return Err(format!("option key `{}`: lower-case English (a-z 0-9 _)", o.key));
    }
    let kind = match o.kind {
        RawKind::Toggle => OptionKind::Toggle,
        RawKind::List => {
            if o.values.is_empty() {
                return Err(format!("option `{}`: a list needs `values`", o.key));
            }
            if !o.labels.is_empty() && o.labels.len() != o.values.len() {
                return Err(format!("option `{}`: `labels` needs one text key per value ({})", o.key, o.values.len()));
            }
            OptionKind::List { values: o.values, labels: o.labels }
        }
        RawKind::Number => {
            let (min, max) = (o.min.ok_or(format!("option `{}`: a number needs `min`", o.key))?, o.max.ok_or(format!("option `{}`: a number needs `max`", o.key))?);
            if min > max {
                return Err(format!("option `{}`: min > max", o.key));
            }
            OptionKind::Number { min, max, step: o.step.unwrap_or(1.0) }
        }
    };
    let mut s = OptionSpec { key: o.key, label: o.label, help: o.help, kind, default: OptionValue::Toggle(false) };
    let dj = serde_json::to_value(&o.default).map_err(|e| e.to_string())?;
    s.default = s.accept(&dj).ok_or_else(|| format!("option `{}`: default {} does not fit its type / values / range", s.key, o.default))?;
    Ok(s)
}

/// Parse an `options.toml` text (strict, see [`crate::data`]). `file` names it in messages.
pub fn parse(file: &str, text: &str) -> Result<Vec<OptionSpec>, Diagnostic> {
    let (_, raw) = data::parse_strict::<RawFile>(file, text, KIND, VERSION)?;
    let mut out: Vec<OptionSpec> = Vec::new();
    for o in raw.option {
        let key = o.key.clone();
        let s = spec(o).map_err(|m| Diagnostic::error(file, None, Some(key.clone()), m))?;
        if out.iter().any(|x| x.key == s.key) {
            return Err(Diagnostic::error(file, None, Some(key), format!("option `{}` declared twice", s.key)));
        }
        out.push(s);
    }
    Ok(out)
}

/// Where the values of mod `mod_id` are kept: `<loader_dir>\mod_options\<mod id>.json`.
pub fn values_path(loader_dir: &Path, mod_id: &str) -> PathBuf {
    loader_dir.join("mod_options").join(format!("{mod_id}.json"))
}

/// The options of one mod with their current values.
#[derive(Debug, Clone, Default)]
pub struct ModOptions {
    pub specs: Vec<OptionSpec>,
    values: BTreeMap<String, OptionValue>,
    pub path: PathBuf,
}

impl ModOptions {
    /// Declarations of `<mod_dir>\options.toml` (none = empty) + the stored values (missing / invalid = default, with
    /// a note).
    pub fn load(loader_dir: &Path, mod_id: &str, mod_dir: &Path, notes: &mut Notes) -> ModOptions {
        let path = values_path(loader_dir, mod_id);
        let mut m = ModOptions { path: path.clone(), ..Default::default() };
        let Ok(text) = std::fs::read_to_string(mod_dir.join(FILE)) else { return m };
        match parse(&format!("mods/{mod_id}/{FILE}"), &text) {
            Ok(s) => m.specs = s,
            Err(d) => {
                notes.diag(&d);
                return m;
            }
        }
        let stored: BTreeMap<String, serde_json::Value> = crate::cache::read_json(&path).unwrap_or_default();
        for s in &m.specs {
            let v = match stored.get(&s.key) {
                None => s.default.clone(),
                Some(j) => s.accept(j).unwrap_or_else(|| {
                    notes.warn(format!("{}: {} = {j} is not valid now: default used", path.display(), s.key));
                    s.default.clone()
                }),
            };
            m.values.insert(s.key.clone(), v);
        }
        m
    }

    /// The current value of `key` (None = no such option).
    pub fn get(&self, key: &str) -> Option<&OptionValue> {
        self.values.get(key)
    }

    /// Set a value (checked against its declaration) and write the values file atomically.
    pub fn set(&mut self, key: &str, v: &serde_json::Value) -> Result<(), String> {
        let s = self.specs.iter().find(|s| s.key == key).ok_or_else(|| format!("no option `{key}`"))?;
        let v = s.accept(v).ok_or_else(|| format!("{key} = {v}: not a valid value"))?;
        self.values.insert(key.to_string(), v);
        let doc: BTreeMap<&str, serde_json::Value> = self.values.iter().map(|(k, v)| (k.as_str(), v.to_json())).collect();
        crate::cache::write_json(&self.path, &doc)
    }
}

/// The `options.toml` of every mod (load order), parsed; files with errors are left out with a note.
pub fn declared(mods: &[crate::ModDir], notes: &mut Notes) -> Vec<DataFile<Vec<OptionSpec>>> {
    let mut out = Vec::new();
    for m in crate::in_load_order(mods) {
        let Ok(text) = std::fs::read_to_string(m.dir.join(FILE)) else { continue };
        let file = format!("mods/{}/{FILE}", m.id);
        match parse(&file, &text) {
            Ok(v) => out.push(DataFile { mod_id: m.id.clone(), load_index: m.load_index, file, version: VERSION, value: v }),
            Err(d) => notes.diag(&d),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const DOC: &str = "schema = \"vr_framework.options/1\"\n\
[[option]]\nkey = \"shot_zone\"\ntype = \"toggle\"\nlabel = \"m.o.shot\"\ndefault = true\n\
[[option]]\nkey = \"difficulty\"\ntype = \"list\"\nlabel = \"m.o.diff\"\nvalues = [\"easy\", \"normal\"]\ndefault = \"normal\"\n\
[[option]]\nkey = \"half_minutes\"\ntype = \"number\"\nlabel = \"m.o.half\"\nhelp = \"m.o.half_help\"\nmin = 1\nmax = 45\ndefault = 5\n";

    #[test]
    fn parse_and_read_values() {
        let s = parse("options.toml", DOC).unwrap();
        assert_eq!(s.len(), 3);
        assert_eq!(s[1].kind, OptionKind::List { values: vec!["easy".into(), "normal".into()], labels: vec![] });
        assert_eq!(s[2].default, OptionValue::Number(5.0));
        let e = parse("options.toml", &DOC.replace("default = \"normal\"", "default = \"hard\"")).unwrap_err();
        assert_eq!(e.key.as_deref(), Some("difficulty"));
        let e = parse("options.toml", &DOC.replace("type = \"toggle\"", "type = \"switch\"")).unwrap_err();
        assert!(e.message.contains("switch"), "{e}");
        let e = parse("options.toml", &DOC.replace("label = \"m.o.shot\"", "lable = \"m.o.shot\"")).unwrap_err();
        assert_eq!((e.line, e.key.as_deref()), (Some(5), Some("lable")), "{e}");

        let d = std::env::temp_dir().join(format!("vr-fw-options-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        let (loader, md) = (d.join("evt_loader"), d.join("mods").join("m"));
        std::fs::create_dir_all(&md).unwrap();
        std::fs::write(md.join(FILE), DOC).unwrap();
        let mut n = Notes::default();
        let mut o = ModOptions::load(&loader, "m", &md, &mut n);
        assert_eq!(o.get("shot_zone"), Some(&OptionValue::Toggle(true)));
        o.set("half_minutes", &serde_json::json!(10)).unwrap();
        assert!(o.set("half_minutes", &serde_json::json!(99)).is_err());
        assert!(o.set("difficulty", &serde_json::json!("nope")).is_err());
        let o = ModOptions::load(&loader, "m", &md, &mut n);
        assert_eq!(o.get("half_minutes").and_then(OptionValue::as_f64), Some(10.0));
        // a stored value that no longer fits (the mod narrowed the range): default, with a note
        std::fs::write(values_path(&loader, "m"), br#"{"half_minutes": 60}"#).unwrap();
        let o = ModOptions::load(&loader, "m", &md, &mut n);
        assert_eq!(o.get("half_minutes").and_then(OptionValue::as_f64), Some(5.0));
        assert_eq!(n.0.len(), 1);
        let _ = std::fs::remove_dir_all(&d);
    }
}
