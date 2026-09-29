//! Mod folder format of VR-ModLoader (sdk/README.md). Pure Rust, no game access:
//! shared by the loader (`vr-loader` module `mods`), the manager and installer, and the `evt-mod` CLI.
//!
//! ```text
//! <game>/mods/enabled.toml                 enabled = ["a", "b"]   (missing file = every mod enabled)
//! <game>/mods/load_order.toml              order = ["top", "next", ...]   top = highest priority (loads last, wins
//!                                          conflicts); ids not listed load before every listed one (by priority)
//! <game>/mods/profile.toml                 active = "Principal"    (the active profile)
//! <game>/mods/profiles/<name>.toml         enabled = [...], order = [...]   saved state of each profile
//! <game>/mods/<id>/mod.toml                id, name, version, author, description, priority, requires, conflicts,
//!                                          loader_modules, tags, updated  (requires: "id" or "id>=1.2"),
//!                                          plugin ("x.dll"), provides ("name" / "name=ver"), loader_min
//!                                          (docs/app/modloader-plugins.md)
//! <game>/mods/<id>/<plugin>.dll            native plugin of the mod (loaded by the ModLoader, C ABI)
//! <game>/mods/<id>/preview.png             optional preview (16:9); the app converts it to preview.g4tx, which
//!                                          the loader serves as data/dx11/menu/evt_mods/preview/<id>.g4tx
//! <game>/mods/<id>/lua/<script>/*.lua      Lua patches (same runner as evt_loader/lua_patches)
//! <game>/mods/<id>/files/data/...          whole-file overrides / new files (path redirection, no cpk_list edit)
//! <game>/mods/<id>/data/<table>.toml       cell deltas ([[set]] / [[add]]; merged by the loader at boot, module
//!                                          `mods::merge`: one cfg.bin per table file, every mod's cells)
//! <game>/mods/<id>/voice/<lang>/<bank>/<suffix>.<mp3|ogg|flac|wav>   voice pack SOURCES (authoring only, never read
//!                                          by the loader; a voice pack build turns them into
//!                                          files/data/common/sound_asset/<lang>/<bank>.acb/.awb)
//! ```
//!
//! A mod whose `mod.toml` has `voice_language = { code = "es", name = "Español" }` is a **voice pack**: its banks
//! `files/data/common/sound_asset/<code>/<bank>.acb/.awb` replace `ja/<bank>` (and `en/<bank>`) while the player's
//! voice language is `<code>` (loader module `mods`, docs/game/media/voice-packs.md).
//!
//! Load order: the mods not listed in `load_order.toml` first (`priority` ascending, then `id`), then the listed ones
//! from the bottom of the list to its top; then every mod is moved after the mods it `requires` (stable topological
//! order; a cycle skips its mods). A mod loaded later wins file and cell conflicts.

use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::path::{Path, PathBuf};

pub const MANIFEST: &str = "mod.toml";
pub const ENABLED_FILE: &str = "enabled.toml";
/// `order = [...]`, top = highest priority (docs/game/engine/mods-menu.md §6).
pub const ORDER_FILE: &str = "load_order.toml";
/// `active = "<profile name>"`.
pub const PROFILE_FILE: &str = "profile.toml";
pub const PROFILES_DIR: &str = "profiles";
/// The profile that exists implicitly (no profiles folder yet).
pub const DEFAULT_PROFILE: &str = "Principal";
/// Optional preview picture of a mod (any size; shown 16:9) and its game texture (built by the app).
pub const PREVIEW_PNG: &str = "preview.png";
pub const PREVIEW_G4TX: &str = "preview.g4tx";
pub const LUA_DIR: &str = "lua";
pub const FILES_DIR: &str = "files";
pub const DATA_DIR: &str = "data";
/// Voice pack sources (`voice/<lang>/<bank>/<suffix>.<ext>`): authoring only, not scanned as game files.
pub const VOICE_SRC_DIR: &str = "voice";
/// Key prefix of the voice banks (`data/common/sound_asset/<lang>/<bank>.acb|awb`).
pub const SOUND_ASSET_KEY: &str = "data/common/sound_asset/";
/// Voice languages shipped by the game (`<VLG>` = `ja` / `en`); a voice pack uses any other code.
pub const RETAIL_VOICE_LANGS: &[&str] = &["ja", "en"];
/// Audio files the voice pack builder accepts as sources (ffmpeg reads them).
pub const VOICE_SRC_EXTS: &[&str] = &["mp3", "ogg", "flac", "wav", "m4a", "opus"];
/// Folder of the game install holding the mods.
pub const MODS_DIR: &str = "mods";

// ---------------------------------------------------------------- issues

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Severity {
    Warning,
    Error,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Issue {
    pub severity: Severity,
    /// Mod id (or folder name when the manifest is unreadable).
    pub module: Option<String>,
    pub path: Option<PathBuf>,
    pub msg: String,
}

impl Issue {
    pub fn new(severity: Severity, module: Option<&str>, path: Option<&Path>, msg: impl Into<String>) -> Self {
        Issue { severity, module: module.map(str::to_string), path: path.map(Path::to_path_buf), msg: msg.into() }
    }
    pub fn err(module: Option<&str>, path: Option<&Path>, msg: impl Into<String>) -> Self {
        Self::new(Severity::Error, module, path, msg)
    }
    pub fn warn(module: Option<&str>, path: Option<&Path>, msg: impl Into<String>) -> Self {
        Self::new(Severity::Warning, module, path, msg)
    }
}

impl fmt::Display for Issue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = match self.severity {
            Severity::Warning => "warning",
            Severity::Error => "error",
        };
        write!(f, "{s}")?;
        if let Some(m) = &self.module {
            write!(f, " [{m}]")?;
        }
        if let Some(p) = &self.path {
            write!(f, " {}", p.display())?;
        }
        write!(f, ": {}", self.msg)
    }
}

// ---------------------------------------------------------------- mod.toml

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Manifest {
    pub id: String,
    #[serde(default)]
    pub name: String,
    pub version: String,
    #[serde(default)]
    pub author: String,
    #[serde(default)]
    pub description: String,
    /// Load order: lower first; a later mod wins conflicts.
    #[serde(default)]
    pub priority: i32,
    /// Mod ids that must be enabled too.
    #[serde(default)]
    pub requires: Vec<String>,
    /// Mod ids this mod cannot run with (this mod is skipped when one of them is active).
    #[serde(default)]
    pub conflicts: Vec<String>,
    /// vr-loader `[modules]` switches the mod needs: since ModLoader 1.0.0 the loader ENABLES them (it does not just warn).
    #[serde(default)]
    pub loader_modules: Vec<String>,
    /// Voice pack: the voice language its banks provide (`voice_language = { code = "es", name = "Español" }`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub voice_language: Option<VoiceLanguage>,
    /// Categories shown as chips and used by the in-game filters (`["Gráficos", "Jugadores"]`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
    /// Date of the last update, `YYYY-MM-DD` (empty = the date of mod.toml on disk).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub updated: String,
    /// Native plugin of the mod: a DLL file name in the mod folder (`"quit_fix.dll"`), loaded by the ModLoader
    /// (docs/app/modloader-plugins.md). Empty = no plugin.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub plugin: String,
    /// Capabilities this mod offers besides its own id: `"name"` or `"name=version"` (`"match_engine_api=1"`).
    /// A `requires` entry is satisfied by a mod with that id **or** a mod that provides that name. A plugin mod that
    /// provides the name of a built-in loader module replaces that module (the built-in yields).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub provides: Vec<String>,
    /// Lowest ModLoader version this mod runs with (`"1.0.0"`); an older loader skips the mod. Empty = any.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub loader_min: String,
}

/// `voice_language` of a voice pack: `code` = folder of its banks (`sound_asset/<code>/`), `name` = what the in-game
/// «Idioma de voz» row shows.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VoiceLanguage {
    pub code: String,
    pub name: String,
}

const MANIFEST_KEYS: &[&str] = &[
    "id",
    "name",
    "version",
    "author",
    "description",
    "priority",
    "requires",
    "conflicts",
    "loader_modules",
    "voice_language",
    "tags",
    "updated",
    "plugin",
    "provides",
    "loader_min",
];

/// Version comparison of a `requires` entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VerOp {
    Ge,
    Gt,
    Eq,
    Le,
    Lt,
}

/// One `requires` entry: `id` or `id<op><version>` (`>=`, `>`, `=`, `==`, `<=`, `<`; spaces allowed).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Requirement {
    pub id: String,
    pub op: Option<(VerOp, String)>,
}

impl Requirement {
    /// The installed `version` satisfies the requirement.
    pub fn accepts(&self, version: &str) -> bool {
        use std::cmp::Ordering::*;
        let Some((op, want)) = &self.op else { return true };
        let c = compare_versions(version, want);
        match op {
            VerOp::Ge => c != Less,
            VerOp::Gt => c == Greater,
            VerOp::Eq => c == Equal,
            VerOp::Le => c != Greater,
            VerOp::Lt => c == Less,
        }
    }
    /// `>= 1.2` (empty without a version).
    pub fn constraint(&self) -> String {
        match &self.op {
            None => String::new(),
            Some((op, v)) => {
                let o = match op {
                    VerOp::Ge => ">=",
                    VerOp::Gt => ">",
                    VerOp::Eq => "=",
                    VerOp::Le => "<=",
                    VerOp::Lt => "<",
                };
                format!("{o} {v}")
            }
        }
    }
}

pub fn parse_requirement(s: &str) -> Result<Requirement, String> {
    let s = s.trim();
    let i = s.find(['<', '>', '=']).unwrap_or(s.len());
    let id = s[..i].trim().to_string();
    validate_id(&id)?;
    let rest = s[i..].trim();
    if rest.is_empty() {
        return Ok(Requirement { id, op: None });
    }
    let ops = [(">=", VerOp::Ge), ("<=", VerOp::Le), ("==", VerOp::Eq), (">", VerOp::Gt), ("<", VerOp::Lt), ("=", VerOp::Eq)];
    let (op, v) = ops
        .iter()
        .find_map(|(t, op)| rest.strip_prefix(t).map(|v| (*op, v.trim())))
        .ok_or_else(|| format!("bad requirement {s:?}"))?;
    if v.is_empty() || v.contains(char::is_whitespace) || v.contains(['<', '>', '=']) {
        return Err(format!("bad version in requirement {s:?}"));
    }
    Ok(Requirement { id, op: Some((op, v.to_string())) })
}

/// Versions compared part by part (`.`, `-`, `+` separators): numbers numerically, other parts as text, a missing part
/// counts as 0 (`1.2` = `1.2.0` < `1.10`); a leading `v` is ignored.
pub fn compare_versions(a: &str, b: &str) -> std::cmp::Ordering {
    let split = |s: &str| -> Vec<String> { s.trim().trim_start_matches(['v', 'V']).split(['.', '-', '+']).map(str::to_string).collect() };
    let (pa, pb) = (split(a), split(b));
    for k in 0..pa.len().max(pb.len()) {
        let x = pa.get(k).map(String::as_str).unwrap_or("0");
        let y = pb.get(k).map(String::as_str).unwrap_or("0");
        let c = match (x.parse::<u64>(), y.parse::<u64>()) {
            (Ok(i), Ok(j)) => i.cmp(&j),
            _ => x.cmp(y),
        };
        if c != std::cmp::Ordering::Equal {
            return c;
        }
    }
    std::cmp::Ordering::Equal
}

/// `YYYY-MM-DD` of a unix time (UTC, civil-from-days).
pub fn iso_date(unix: u64) -> String {
    let days = (unix / 86_400) as i64;
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!("{y:04}-{m:02}-{d:02}")
}

/// Voice language code: 2-3 lowercase letters with an optional `-variant` (`es`, `fr`, `pt-br`, `es-mx`), never a
/// retail one (`ja` / `en`: banks there would override the game's own voices for every player).
pub fn validate_voice_code(code: &str) -> Result<(), String> {
    let (base, variant) = match code.split_once('-') {
        Some((b, v)) => (b, Some(v)),
        None => (code, None),
    };
    let base_ok = (2..=3).contains(&base.len()) && base.bytes().all(|b| b.is_ascii_lowercase());
    let var_ok = variant.map_or(true, |v| (1..=8).contains(&v.len()) && v.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit()));
    if !base_ok || !var_ok {
        return Err(format!("invalid voice language code {code:?}: use 2-3 letters a-z, optionally -variant (es, fr, pt-br)"));
    }
    if RETAIL_VOICE_LANGS.contains(&code) {
        return Err(format!("voice language code {code:?} is a retail one (ja / en): pick another code"));
    }
    Ok(())
}

/// One `provides` entry: `name` (the mod's own version) or `name=version`. Returns (name, explicit version).
pub fn parse_provide(s: &str) -> Result<(String, Option<String>), String> {
    let (name, ver) = match s.split_once('=') {
        Some((n, v)) => (n.trim(), Some(v.trim())),
        None => (s.trim(), None),
    };
    validate_id(name).map_err(|e| format!("provides {s:?}: {e}"))?;
    match ver {
        Some(v) if v.is_empty() || v.contains(char::is_whitespace) || v.contains(['<', '>', '=']) => {
            Err(format!("provides {s:?}: bad version"))
        }
        v => Ok((name.to_string(), v.map(str::to_string))),
    }
}

/// `plugin` file name: a plain `*.dll` name inside the mod folder (no path, no `..`).
pub fn validate_plugin_name(s: &str) -> Result<(), String> {
    let ok = s.to_ascii_lowercase().ends_with(".dll")
        && s.len() > 4
        && s.len() <= 64
        && !s.contains(['/', '\\', ':'])
        && !s.starts_with('.')
        && s.bytes().all(|b| b.is_ascii_alphanumeric() || b"_.-".contains(&b));
    if ok {
        Ok(())
    } else {
        Err(format!("plugin {s:?}: use a plain DLL file name in the mod folder (a-z 0-9 _ . -, ends in .dll)"))
    }
}

/// `[a-z0-9][a-z0-9_.-]{0,63}`.
pub fn validate_id(id: &str) -> Result<(), String> {
    let ok_first = id.bytes().next().is_some_and(|b| b.is_ascii_lowercase() || b.is_ascii_digit());
    let ok_rest = id.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b"_.-".contains(&b));
    if !ok_first || !ok_rest || id.len() > 64 {
        return Err(format!("invalid mod id {id:?}: use 1-64 chars a-z 0-9 _ . - (first a-z / 0-9)"));
    }
    Ok(())
}

/// Parse and validate `mod.toml`. Ok = (manifest, warnings).
pub fn parse_manifest(text: &str) -> Result<(Manifest, Vec<String>), String> {
    let table: toml::Table = text.parse().map_err(|e: toml::de::Error| e.message().to_string())?;
    let mut warnings: Vec<String> =
        table.keys().filter(|k| !MANIFEST_KEYS.contains(&k.as_str())).map(|k| format!("unknown key `{k}`")).collect();
    let m: Manifest = toml::Value::Table(table).try_into().map_err(|e: toml::de::Error| e.message().to_string())?;
    validate_id(&m.id)?;
    if m.version.trim().is_empty() {
        return Err("`version` is empty".into());
    }
    let mut req_ids = Vec::new();
    for r in &m.requires {
        req_ids.push(parse_requirement(r)?.id);
    }
    for c in &m.conflicts {
        validate_id(c)?;
    }
    if req_ids.contains(&m.id) || m.conflicts.contains(&m.id) {
        return Err("a mod cannot require or conflict with itself".into());
    }
    if let Some(x) = req_ids.iter().find(|r| m.conflicts.contains(r)) {
        return Err(format!("`{x}` is both required and conflicting"));
    }
    for t in &m.tags {
        if t.trim().is_empty() || t.chars().count() > 24 {
            warnings.push(format!("tag {t:?}: use 1-24 characters"));
        }
    }
    if m.tags.len() > 6 {
        warnings.push("more than 6 tags: the game shows the first 3".into());
    }
    let u = m.updated.as_bytes();
    let date_ok = u.len() == 10 && u.iter().enumerate().all(|(i, b)| if i == 4 || i == 7 { *b == b'-' } else { b.is_ascii_digit() });
    if !m.updated.is_empty() && !date_ok {
        warnings.push(format!("`updated` {:?} is not YYYY-MM-DD", m.updated));
    }
    if !m.plugin.is_empty() {
        validate_plugin_name(&m.plugin)?;
    }
    // (a mod provides its own id anyway; naming it explicitly is how a plugin replaces a built-in loader module of
    // the same name)
    for p in &m.provides {
        parse_provide(p)?;
    }
    let lm = m.loader_min.trim();
    if !m.loader_min.is_empty() && (lm.is_empty() || lm.contains(char::is_whitespace) || lm.contains(['<', '>', '='])) {
        return Err(format!("`loader_min` {:?} is not a version", m.loader_min));
    }
    if let Some(v) = &m.voice_language {
        validate_voice_code(&v.code)?;
        if v.name.trim().is_empty() {
            return Err("`voice_language.name` is empty (it is what the in-game «Idioma de voz» row shows)".into());
        }
    }
    if m.name.trim().is_empty() {
        warnings.push("`name` is empty".into());
    }
    Ok((m, warnings))
}

/// Default `mod.toml` text for a new mod.
pub fn manifest_template(id: &str) -> String {
    format!(
        r#"id = "{id}"
name = "{id}"
version = "0.1.0"
author = ""
description = ""
priority = 0            # load order: lower first; a later mod wins file / cell conflicts
requires = []           # mod ids that must be enabled too ("base" or "base>=1.2")
conflicts = []          # mod ids this mod cannot run with
loader_modules = []     # vr-loader [modules] switches this mod needs, e.g. ["lua_bridge"]
tags = []               # in-game filters / chips, e.g. ["Gráficos", "Jugadores"]
updated = ""            # YYYY-MM-DD of the last update (empty = the date of this file)
# preview.png next to this file = the picture of the in-game Mods menu (16:9)
# plugin = "{id}.dll"   # native ModLoader plugin in this folder (docs/app/modloader-plugins.md)
# provides = []         # extra names `requires` can use: "name" or "name=1.0"
# loader_min = "1.0.0"  # lowest ModLoader version this mod runs with
# voice_language = {{ code = "es", name = "Español" }}   # only for a voice pack (docs/game/media/voice-packs.md)
"#
    )
}

// ---------------------------------------------------------------- enabled.toml

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct EnabledToml {
    enabled: Vec<String>,
}

/// `enabled = ["a", "b"]`.
pub fn parse_enabled(text: &str) -> Result<Vec<String>, String> {
    let e: EnabledToml = toml::from_str(text).map_err(|e| e.message().to_string())?;
    Ok(e.enabled)
}

pub fn enabled_text(ids: &[String]) -> String {
    let list: Vec<String> = ids.iter().map(|i| format!("{i:?}")).collect();
    format!("enabled = [{}]\n", list.join(", "))
}

/// `<root>/enabled.toml`: Ok(None) when missing (= all mods enabled).
pub fn read_enabled(root: &Path) -> Result<Option<Vec<String>>, String> {
    match std::fs::read_to_string(root.join(ENABLED_FILE)) {
        Ok(t) => parse_enabled(&t).map(Some),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e.to_string()),
    }
}

/// The `enabled.toml` list after switching `id` on or off (the one rule shared by the in-game «Mods» menu of the
/// loader and the app's Mods card, so both write the same file the same way). `current` = the file's list (`None`
/// when there is no file or it cannot be read: every installed mod counts as enabled, in `installed` order);
/// ids of the file that are not installed are kept; `id` is removed, and appended at the end when `on`.
pub fn toggle_enabled(current: Option<Vec<String>>, installed: &[String], id: &str, on: bool) -> Vec<String> {
    let mut list = current.unwrap_or_else(|| {
        let mut v: Vec<String> = Vec::new();
        for i in installed {
            if !v.contains(i) {
                v.push(i.clone());
            }
        }
        v
    });
    list.retain(|x| x != id);
    if on {
        list.push(id.to_string());
    }
    list
}

// ---------------------------------------------------------------- data deltas

/// Cell value of a delta.
#[derive(Debug, Clone, PartialEq)]
pub enum Cell {
    Int(i64),
    Float(f64),
    Str(String),
}

impl fmt::Display for Cell {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Cell::Int(v) => write!(f, "{v}"),
            Cell::Float(v) => write!(f, "{v}"),
            Cell::Str(v) => write!(f, "{v:?}"),
        }
    }
}

/// Column by name (schema column) or by zero-based index.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Column {
    Name(String),
    Index(u32),
}

impl fmt::Display for Column {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Column::Name(n) => f.write_str(n),
            Column::Index(i) => write!(f, "#{i}"),
        }
    }
}

/// `[[set]] table=.. key=.. column=.. value=..`: change one cell of an existing row.
#[derive(Debug, Clone, PartialEq)]
pub struct SetOp {
    pub table: String,
    pub key: String,
    pub column: Column,
    pub value: Cell,
}

/// `[[add]] table=.. key=.. from=.. [add.values] col = value`: new row (optionally cloned from row `from`).
#[derive(Debug, Clone, PartialEq)]
pub struct AddOp {
    pub table: String,
    pub key: String,
    pub from: Option<String>,
    pub values: BTreeMap<String, Cell>,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct DeltaFile {
    pub set: Vec<SetOp>,
    pub add: Vec<AddOp>,
}

/// Standard CRC-32 (zlib / IEEE), the name hash of the game (`l5_core::hash::crc32`); bitwise, no table: only used
/// for a few delta keys.
pub fn crc32(data: &[u8]) -> u32 {
    let mut c = 0xFFFF_FFFFu32;
    for &b in data {
        c ^= b as u32;
        for _ in 0..8 {
            c = if c & 1 != 0 { (c >> 1) ^ 0xEDB8_8320 } else { c >> 1 };
        }
    }
    !c
}

/// A delta's row key (`key` / `from`), as the loader matches it against the table's key column
/// (docs/game/engine/mod-format.md, «Claves de fila»):
/// * a number (`"1234"`, `"-1046485492"`, `"0xC1A0B2E4"`) = the key cell's 32-bit value (signed and unsigned
///   spellings are the same key);
/// * anything else = a name: a text key cell equal to it, or an integer key cell equal to `crc32(name)`
///   (`"pc_para_c01000010"`).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum RowKey {
    Num(u32),
    Name(String),
}

impl RowKey {
    pub fn parse(s: &str) -> RowKey {
        let t = s.trim();
        let num = if let Some(h) = t.strip_prefix("0x").or_else(|| t.strip_prefix("0X")) {
            u32::from_str_radix(h, 16).ok()
        } else if t.bytes().enumerate().all(|(i, b)| b.is_ascii_digit() || (i == 0 && b == b'-' && t.len() > 1)) && !t.is_empty() {
            t.parse::<i64>().ok().filter(|v| (i32::MIN as i64..=u32::MAX as i64).contains(v)).map(|v| v as u32)
        } else {
            None
        };
        match num {
            Some(n) => RowKey::Num(n),
            None => RowKey::Name(s.to_string()),
        }
    }
    /// The 32-bit value an integer key cell must hold.
    pub fn hash(&self) -> u32 {
        match self {
            RowKey::Num(n) => *n,
            RowKey::Name(s) => crc32(s.as_bytes()),
        }
    }
}

impl DeltaFile {
    /// Tables this file touches (sorted, once each).
    pub fn tables(&self) -> Vec<&str> {
        let mut v: Vec<&str> = self.set.iter().map(|s| s.table.as_str()).chain(self.add.iter().map(|a| a.table.as_str())).collect();
        v.sort();
        v.dedup();
        v
    }
}

/// `character/chara_param`: lowercase path segments `[a-z0-9_.-]`, separated by `/`.
pub fn validate_table(t: &str) -> Result<(), String> {
    let ok = !t.is_empty()
        && t.split('/').all(|s| {
            !s.is_empty() && s != "." && s != ".." && s.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b"_.-".contains(&b))
        });
    if ok {
        Ok(())
    } else {
        Err(format!("invalid table name {t:?} (expected e.g. \"character/chara_param\")"))
    }
}

fn cell(v: &toml::Value, what: &str) -> Result<Cell, String> {
    match v {
        toml::Value::Integer(i) => Ok(Cell::Int(*i)),
        toml::Value::Float(x) => Ok(Cell::Float(*x)),
        toml::Value::String(s) => Ok(Cell::Str(s.clone())),
        toml::Value::Boolean(b) => Ok(Cell::Int(*b as i64)),
        other => Err(format!("{what}: unsupported value type {}", other.type_str())),
    }
}

fn str_field(t: &toml::Table, k: &str, what: &str) -> Result<Option<String>, String> {
    match t.get(k) {
        None => Ok(None),
        Some(toml::Value::String(s)) if !s.is_empty() => Ok(Some(s.clone())),
        Some(_) => Err(format!("{what}: `{k}` must be a non-empty string")),
    }
}

fn op_table(t: &toml::Table, default_table: Option<&str>, what: &str) -> Result<String, String> {
    let table = str_field(t, "table", what)?.or(default_table.map(str::to_string)).ok_or(format!("{what}: `table` missing"))?;
    validate_table(&table).map_err(|e| format!("{what}: {e}"))?;
    Ok(table)
}

/// Parse a delta file. `default_table` = the table implied by the file path (`data/character/chara_param.toml`),
/// used when an entry has no `table`. Ok = (ops, warnings).
pub fn parse_delta(text: &str, default_table: Option<&str>) -> Result<(DeltaFile, Vec<String>), String> {
    let root: toml::Table = text.parse().map_err(|e: toml::de::Error| e.message().to_string())?;
    let mut out = DeltaFile::default();
    let mut warnings = Vec::new();
    for k in root.keys().filter(|k| *k != "set" && *k != "add") {
        warnings.push(format!("unknown key `{k}` (expected [[set]] / [[add]])"));
    }
    let arr = |k: &str| -> Result<Vec<toml::Table>, String> {
        match root.get(k) {
            None => Ok(vec![]),
            Some(toml::Value::Array(a)) => a
                .iter()
                .enumerate()
                .map(|(i, v)| v.as_table().cloned().ok_or(format!("{k}[{i}] is not a table (use [[{k}]])")))
                .collect(),
            Some(_) => Err(format!("`{k}` must be an array of tables ([[{k}]])")),
        }
    };
    for (i, t) in arr("set")?.iter().enumerate() {
        let what = format!("set[{i}]");
        for k in t.keys().filter(|k| !["table", "key", "column", "value"].contains(&k.as_str())) {
            warnings.push(format!("{what}: unknown key `{k}`"));
        }
        let table = op_table(t, default_table, &what)?;
        let key = str_field(t, "key", &what)?.ok_or(format!("{what}: `key` missing"))?;
        let column = match t.get("column") {
            Some(toml::Value::String(s)) if !s.is_empty() => Column::Name(s.clone()),
            Some(toml::Value::Integer(n)) if (0..=u32::MAX as i64).contains(n) => Column::Index(*n as u32),
            Some(_) => return Err(format!("{what}: `column` must be a name or a column index >= 0")),
            None => return Err(format!("{what}: `column` missing")),
        };
        let value = cell(t.get("value").ok_or(format!("{what}: `value` missing"))?, &what)?;
        out.set.push(SetOp { table, key, column, value });
    }
    for (i, t) in arr("add")?.iter().enumerate() {
        let what = format!("add[{i}]");
        for k in t.keys().filter(|k| !["table", "key", "from", "values"].contains(&k.as_str())) {
            warnings.push(format!("{what}: unknown key `{k}`"));
        }
        let table = op_table(t, default_table, &what)?;
        let key = str_field(t, "key", &what)?.ok_or(format!("{what}: `key` missing"))?;
        let from = str_field(t, "from", &what)?;
        let mut values = BTreeMap::new();
        match t.get("values") {
            None => {}
            Some(toml::Value::Table(v)) => {
                for (c, x) in v {
                    values.insert(c.clone(), cell(x, &format!("{what}.values.{c}"))?);
                }
            }
            Some(_) => return Err(format!("{what}: `values` must be a table")),
        }
        if from.is_none() && values.is_empty() {
            warnings.push(format!("{what}: no `from` and no `values`: the row would be all defaults"));
        }
        out.add.push(AddOp { table, key, from, values });
    }
    Ok((out, warnings))
}

// ---------------------------------------------------------------- scanning a mod folder

/// One whole-file override `files/<key>`.
#[derive(Debug, Clone, PartialEq)]
pub struct FileEntry {
    /// Normalised logical path: lowercase, `/`, starts with `data/` (the cpk_list key form).
    pub key: String,
    /// Path relative to `files/` as found on disk.
    pub rel: String,
    pub path: PathBuf,
    pub size: u64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct DeltaSource {
    /// Path relative to the mod's `data/` folder.
    pub rel: String,
    pub delta: DeltaFile,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ModInfo {
    pub dir: PathBuf,
    pub manifest: Manifest,
    /// Script folders under `lua/` that hold at least one `.lua` file.
    pub lua_scripts: Vec<String>,
    pub files: Vec<FileEntry>,
    pub deltas: Vec<DeltaSource>,
    /// Bytes of every file in the folder.
    pub size: u64,
    /// Unix time of mod.toml (fallback of `updated`).
    pub mtime: u64,
    /// `preview.g4tx` (the game texture built from preview.png), when present.
    pub preview: Option<PathBuf>,
    /// `preview.png`, when present.
    pub preview_png: Option<PathBuf>,
}

impl ModInfo {
    /// `updated` of the manifest, else the date of mod.toml.
    pub fn updated(&self) -> String {
        if self.manifest.updated.is_empty() && self.mtime > 0 { iso_date(self.mtime) } else { self.manifest.updated.clone() }
    }
    /// The parsed `requires` (invalid entries were refused by parse_manifest).
    pub fn requirements(&self) -> Vec<Requirement> {
        self.manifest.requires.iter().filter_map(|r| parse_requirement(r).ok()).collect()
    }
    pub fn id(&self) -> &str {
        &self.manifest.id
    }
    pub fn lua_dir(&self) -> PathBuf {
        self.dir.join(LUA_DIR)
    }
    pub fn label(&self) -> String {
        format!("{}@{}", self.manifest.id, self.manifest.version)
    }
}

/// Engine path -> normalised key (`data/...`, lowercase, `/`). `common/x` gets the `data/` prefix; `./` and
/// leading slashes are dropped. An absolute path (`D:/game/data/common/x`) keeps the part from its first `data/`
/// segment; None when it has none.
pub fn normalize_key(path: &str) -> Option<String> {
    let mut p = path.replace('\\', "/").to_ascii_lowercase();
    if p.as_bytes().get(1) == Some(&b':') {
        let i = p.find("/data/")?;
        return Some(p[i + 1..].to_string());
    }
    while let Some(r) = p.strip_prefix("./").or_else(|| p.strip_prefix('/')) {
        p = r.to_string();
    }
    if p.is_empty() {
        return None;
    }
    Some(if p.starts_with("data/") { p } else { format!("data/{p}") })
}

const JUNK: &[&str] = &["thumbs.db", "desktop.ini", ".ds_store"];

fn walk(dir: &Path, rel: &str, out: &mut Vec<(String, PathBuf)>) {
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    let mut ents: Vec<_> = rd.flatten().collect();
    ents.sort_by_key(|e| e.file_name());
    for e in ents {
        let name = e.file_name().to_string_lossy().into_owned();
        let r = if rel.is_empty() { name.clone() } else { format!("{rel}/{name}") };
        match e.file_type() {
            Ok(t) if t.is_dir() => walk(&e.path(), &r, out),
            Ok(t) if t.is_file() => out.push((r, e.path())),
            _ => {}
        }
    }
}

/// Read one mod folder. None when `mod.toml` is missing or invalid (the issues say why).
pub fn scan_mod(dir: &Path) -> (Option<ModInfo>, Vec<Issue>) {
    let folder = dir.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let mut issues = Vec::new();
    let mpath = dir.join(MANIFEST);
    let text = match std::fs::read_to_string(&mpath) {
        Ok(t) => t,
        Err(e) => {
            issues.push(Issue::err(Some(&folder), Some(&mpath), format!("cannot read: {e}")));
            return (None, issues);
        }
    };
    let manifest = match parse_manifest(&text) {
        Ok((m, w)) => {
            issues.extend(w.into_iter().map(|w| Issue::warn(Some(&m.id), Some(&mpath), w)));
            m
        }
        Err(e) => {
            issues.push(Issue::err(Some(&folder), Some(&mpath), e));
            return (None, issues);
        }
    };
    let id = manifest.id.clone();
    let idr = Some(id.as_str());
    if folder != id {
        issues.push(Issue::warn(idr, Some(dir), format!("folder name `{folder}` differs from the id")));
    }
    if !manifest.plugin.is_empty() && !dir.join(&manifest.plugin).is_file() {
        issues.push(Issue::err(idr, Some(&dir.join(&manifest.plugin)), "plugin DLL not found in the mod folder"));
    }
    // lua/<script>/*.lua
    let mut lua_scripts = Vec::new();
    if let Ok(rd) = std::fs::read_dir(dir.join(LUA_DIR)) {
        let mut ents: Vec<_> = rd.flatten().collect();
        ents.sort_by_key(|e| e.file_name());
        for e in ents {
            let name = e.file_name().to_string_lossy().into_owned();
            if e.path().is_dir() {
                let n = std::fs::read_dir(e.path())
                    .map(|rd| {
                        rd.flatten()
                            .filter(|f| f.path().is_file() && f.file_name().to_string_lossy().to_ascii_lowercase().ends_with(".lua"))
                            .count()
                    })
                    .unwrap_or(0);
                if n > 0 {
                    lua_scripts.push(name);
                } else {
                    issues.push(Issue::warn(idr, Some(&e.path()), "script folder without .lua files"));
                }
            } else if name.eq_ignore_ascii_case("_fingerprints.json") {
                // read by the loader's lua_patch (fingerprint match mode); menu_assemble.py builds ship it
            } else {
                issues.push(Issue::warn(idr, Some(&e.path()), "ignored: Lua files go in lua/<script>/*.lua"));
            }
        }
    }
    // files/data/...
    let mut raw = Vec::new();
    walk(&dir.join(FILES_DIR), "", &mut raw);
    let mut files: Vec<FileEntry> = Vec::new();
    for (rel, path) in raw {
        let base = rel.rsplit('/').next().unwrap_or("").to_ascii_lowercase();
        if JUNK.contains(&base.as_str()) {
            issues.push(Issue::warn(idr, Some(&path), "ignored junk file"));
            continue;
        }
        if !rel.to_ascii_lowercase().starts_with("data/") {
            issues.push(Issue::warn(idr, Some(&path), "ignored: override files go under files/data/"));
            continue;
        }
        let key = normalize_key(&rel).unwrap_or_default();
        if key.to_ascii_lowercase() == "data/cpk_list.cfg.bin" {
            issues.push(Issue::err(idr, Some(&path), "cpk_list.cfg.bin cannot be overridden by a mod"));
            continue;
        }
        if files.iter().any(|f| f.key == key) {
            issues.push(Issue::err(idr, Some(&path), format!("{key}: two files differ only by case")));
            continue;
        }
        let size = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
        files.push(FileEntry { key, rel, path, size });
    }
    // data/**.toml
    let mut raw = Vec::new();
    walk(&dir.join(DATA_DIR), "", &mut raw);
    let mut deltas = Vec::new();
    for (rel, path) in raw {
        let Some(stem) = rel.strip_suffix(".toml") else {
            issues.push(Issue::warn(idr, Some(&path), "ignored: delta files are data/<table>.toml"));
            continue;
        };
        let default_table = validate_table(stem).is_ok().then_some(stem);
        match std::fs::read_to_string(&path).map_err(|e| e.to_string()).and_then(|t| parse_delta(&t, default_table)) {
            Ok((delta, w)) => {
                issues.extend(w.into_iter().map(|w| Issue::warn(idr, Some(&path), w)));
                let mut seen = BTreeSet::new();
                for s in &delta.set {
                    if !seen.insert((s.table.clone(), s.key.clone(), s.column.clone())) {
                        issues.push(Issue::warn(idr, Some(&path), format!("{}[{}].{} set twice: last wins", s.table, s.key, s.column)));
                    }
                }
                deltas.push(DeltaSource { rel, delta });
            }
            Err(e) => issues.push(Issue::err(idr, Some(&path), e)),
        }
    }
    // voice pack: banks outside its own language folder are plain overrides (always active)
    if let Some(v) = &manifest.voice_language {
        for f in &files {
            if let Some((lang, _, _)) = voice_bank_of(&f.key) {
                if lang != v.code {
                    issues.push(Issue::warn(
                        idr,
                        Some(&f.path),
                        format!("voice bank of `{lang}` in a `{}` voice pack: it overrides that language for every player", v.code),
                    ));
                }
            }
        }
    }
    let mut all = Vec::new();
    walk(dir, "", &mut all);
    let size = all.iter().map(|(_, p)| std::fs::metadata(p).map(|m| m.len()).unwrap_or(0)).sum();
    let mtime = std::fs::metadata(&mpath)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map_or(0, |d| d.as_secs());
    let preview = Some(dir.join(PREVIEW_G4TX)).filter(|p| p.is_file());
    let preview_png = Some(dir.join(PREVIEW_PNG)).filter(|p| p.is_file());
    (Some(ModInfo { dir: dir.to_path_buf(), manifest, lua_scripts, files, deltas, size, mtime, preview, preview_png }), issues)
}

// ---------------------------------------------------------------- voice packs

/// Key of voice bank file `file` (`c01000010.acb`) in language folder `lang`.
pub fn voice_key(lang: &str, file: &str) -> String {
    format!("{SOUND_ASSET_KEY}{lang}/{}", file.to_ascii_lowercase())
}

/// `data/common/sound_asset/<lang>/<stem>.<acb|awb>` -> (lang, stem, ext). Only one folder level (the voice banks);
/// the language-independent banks (`sound_asset/bgm.acb`) are not voice banks.
pub fn voice_bank_of(key: &str) -> Option<(&str, &str, &str)> {
    let rest = key.strip_prefix(SOUND_ASSET_KEY)?;
    let (lang, file) = rest.split_once('/')?;
    if lang.is_empty() || file.contains('/') {
        return None;
    }
    let (stem, ext) = file.rsplit_once('.')?;
    (!stem.is_empty() && (ext == "acb" || ext == "awb")).then_some((lang, stem, ext))
}

/// `data/common/sound_asset/<bank>.<acb|awb>` — a language-independent bank at the root of `sound_asset/`
/// (`bgm`, `bgm_chronicle`, `anime_stream`…) — -> (stem, ext). None for a voice bank (one folder level down, see
/// [`voice_bank_of`]) or any other file.
pub fn root_bank_of(key: &str) -> Option<(&str, &str)> {
    let rest = key.strip_prefix(SOUND_ASSET_KEY)?;
    if rest.is_empty() || rest.contains('/') {
        return None;
    }
    let (stem, ext) = rest.rsplit_once('.')?;
    (!stem.is_empty() && (ext == "acb" || ext == "awb")).then_some((stem, ext))
}

impl ModInfo {
    /// Voice pack banks: stems with BOTH `.acb` and `.awb` under `sound_asset/<voice_language.code>/`, sorted.
    pub fn voice_banks(&self) -> Vec<String> {
        let Some(v) = &self.manifest.voice_language else { return Vec::new() };
        let mut acb = BTreeSet::new();
        let mut awb = BTreeSet::new();
        for f in &self.files {
            if let Some((lang, stem, ext)) = voice_bank_of(&f.key) {
                if lang == v.code {
                    if ext == "acb" { &mut acb } else { &mut awb }.insert(stem.to_string());
                }
            }
        }
        acb.intersection(&awb).cloned().collect()
    }
}

/// One source audio file of a voice pack: `voice/<lang>/<bank>/<suffix>.<ext>` -> cue `<bank>_<suffix>`.
#[derive(Debug, Clone, PartialEq)]
pub struct VoiceSource {
    pub bank: String,
    pub suffix: String,
    pub path: PathBuf,
}

/// `[A-Za-z0-9_]+`: bank stems (`c01000010`, `scoutMAA01`) and cue suffixes (`gl010`, `whd00020`, `k000510_inc`).
pub fn valid_voice_name(s: &str) -> bool {
    !s.is_empty() && s.len() <= 64 && s.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
}

/// The sources of `<mod>/voice/<lang>/` (bank folders -> audio files), sorted by bank then suffix, plus problems
/// (bad names, unknown extensions, two files for one cue). Missing folder = nothing, no issue. Same rules as
/// `research/scripts/voice_pack_build.py` (`sources()`).
pub fn voice_sources(mod_dir: &Path, lang: &str) -> (Vec<VoiceSource>, Vec<Issue>) {
    let mut out: Vec<VoiceSource> = Vec::new();
    let mut issues = Vec::new();
    let root = mod_dir.join(VOICE_SRC_DIR).join(lang);
    let mut raw = Vec::new();
    walk(&root, "", &mut raw);
    for (rel, path) in raw {
        let base = rel.rsplit('/').next().unwrap_or("").to_ascii_lowercase();
        if JUNK.contains(&base.as_str()) || base.starts_with('.') || base.starts_with('_') || base.ends_with(".md") || base.ends_with(".txt") {
            continue;
        }
        let parts: Vec<&str> = rel.split('/').collect();
        if parts.len() != 2 {
            issues.push(Issue::warn(None, Some(&path), "ignored: audio goes in voice/<lang>/<bank>/<suffix>.<ext>"));
            continue;
        }
        let (bank, file) = (parts[0], parts[1]);
        let Some((suffix, ext)) = file.rsplit_once('.') else {
            issues.push(Issue::warn(None, Some(&path), "ignored: no extension"));
            continue;
        };
        if !VOICE_SRC_EXTS.contains(&ext.to_ascii_lowercase().as_str()) {
            issues.push(Issue::warn(None, Some(&path), format!("ignored: .{ext} is not an audio format ({})", VOICE_SRC_EXTS.join(", "))));
            continue;
        }
        // a full cue name as file name is accepted too (c01000010_gl010.ogg)
        let prefix = format!("{bank}_");
        let suffix = suffix.strip_prefix(prefix.as_str()).unwrap_or(suffix);
        if !valid_voice_name(bank) || !valid_voice_name(suffix) {
            issues.push(Issue::err(None, Some(&path), "bank folder and file name may only use A-Z a-z 0-9 _"));
            continue;
        }
        if out.iter().any(|s| s.bank == bank && s.suffix.eq_ignore_ascii_case(suffix)) {
            issues.push(Issue::err(None, Some(&path), format!("two files for cue {bank}_{suffix}")));
            continue;
        }
        out.push(VoiceSource { bank: bank.to_string(), suffix: suffix.to_string(), path });
    }
    out.sort_by(|a, b| (&a.bank, &a.suffix).cmp(&(&b.bank, &b.suffix)));
    (out, issues)
}

/// A voice language offered by the active voice packs (in-game «Idioma de voz» values after «Original»).
#[derive(Debug, Clone, PartialEq)]
pub struct VoiceLang {
    pub code: String,
    /// Name of the first pack (load order) declaring the code.
    pub name: String,
    /// Packs providing it, in load order (a later one wins a bank both ship).
    pub mods: Vec<String>,
    /// Distinct banks (stems) over all those packs.
    pub banks: usize,
}

impl LoadPlan {
    /// Voice languages of the active voice packs, in load order of their first pack. Packs without banks count too
    /// (a pack being built still shows up), with `banks` 0.
    pub fn voice_languages(&self) -> Vec<VoiceLang> {
        let mut out: Vec<VoiceLang> = Vec::new();
        let mut stems: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
        for m in &self.mods {
            let Some(v) = &m.manifest.voice_language else { continue };
            stems.entry(v.code.clone()).or_default().extend(m.voice_banks());
            match out.iter_mut().find(|l| l.code == v.code) {
                Some(l) => l.mods.push(m.manifest.id.clone()),
                None => out.push(VoiceLang { code: v.code.clone(), name: v.name.clone(), mods: vec![m.manifest.id.clone()], banks: 0 }),
            }
        }
        for l in &mut out {
            l.banks = stems.get(&l.code).map_or(0, |s| s.len());
        }
        out
    }
}

fn toml_quote(s: &str) -> String {
    let mut o = String::with_capacity(s.len() + 2);
    o.push('"');
    for c in s.chars() {
        match c {
            '"' => o.push_str("\\\""),
            '\\' => o.push_str("\\\\"),
            '\n' => o.push_str("\\n"),
            '\r' => o.push_str("\\r"),
            '\t' => o.push_str("\\t"),
            c if (c as u32) < 0x20 || c == '\u{7f}' => o.push_str(&format!("\\u{:04X}", c as u32)),
            c => o.push(c),
        }
    }
    o.push('"');
    o
}

/// `mod.toml` of a new voice pack (the app's «Nuevo pack de voces»).
pub fn voice_pack_manifest(id: &str, name: &str, author: &str, code: &str, lang_name: &str) -> String {
    let desc = format!("Voces en {lang_name}. En el juego: Opciones > Ajustes del juego > Idioma de voz > {lang_name}.");
    format!(
        "id = {}\nname = {}\nversion = \"0.1.0\"\nauthor = {}\ndescription = {}\npriority = 0            # load order: lower first; a later pack wins a bank both ship\nrequires = []\nconflicts = []\nloader_modules = [\"mods\"]\n# voice pack: banks files/data/common/sound_asset/{code}/<bank>.acb/.awb, built from voice/{code}/<bank>/<suffix>.<ext>\nvoice_language = {{ code = {}, name = {} }}\n",
        toml_quote(id),
        toml_quote(name),
        toml_quote(author),
        toml_quote(&desc),
        toml_quote(code),
        toml_quote(lang_name),
    )
}

/// `README.md` written into a new voice pack for its contributors.
pub fn voice_pack_readme(name: &str, code: &str, lang_name: &str) -> String {
    format!(
        r#"# {name}: voice pack ({lang_name}, `{code}`)

This mod replaces game voices with recordings in {lang_name}.

## How to contribute

1. Every character has a **bank**: a folder inside `voice/{code}/` named after the bank id (`c01000010` = Mark
   Evans; generic voices are `scoutMAA01`...).
2. Inside it, one audio file per line, named after the **cue suffix**:
   `voice/{code}/c01000010/gl010.ogg` replaces the cue `c01000010_gl010` (goal celebration).
   Formats: .mp3 .ogg .flac .wav .m4a .opus; mono or stereo, any sample rate (converted to the bank's format).
3. Common suffixes: `bt010`-`bt080` during the match, `gl010`-`gl170` goal, `pa010`-`pa030` pass, `sh010`/`sh020`
   shot, `kp020`/`kp030` goalkeeper, `sp010`-`sp150` special lines, `wa010`/`wa020`/`fr010`/`jp010` reactions,
   `whs#####`/`whk#####`/`whd#####`/`who#####` the shout of each special move (the number is the move id).
4. Whatever you do not record keeps the game's original voice.
5. A voice pack build turns the folders into banks `files/data/common/sound_asset/{code}/<bank>.acb/.awb`. What you
   publish are those banks and `mod.toml`; the `voice/` folder is working material.

In game: Options > Game settings > **Voice language** > "{lang_name}".
"#
    )
}

/// Every mod under `root` (sub-folders with a `mod.toml`, by folder name). Folders starting with `.` or `_` are
/// skipped silently.
pub fn scan_root(root: &Path) -> (Vec<ModInfo>, Vec<Issue>) {
    let mut mods = Vec::new();
    let mut issues = Vec::new();
    let Ok(rd) = std::fs::read_dir(root) else { return (mods, issues) };
    let mut dirs: Vec<PathBuf> = rd.flatten().filter(|e| e.path().is_dir()).map(|e| e.path()).collect();
    dirs.sort();
    for d in dirs {
        let name = d.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        if name.starts_with('.') || name.starts_with('_') {
            continue;
        }
        if !d.join(MANIFEST).is_file() {
            issues.push(Issue::warn(Some(&name), Some(&d), "no mod.toml: not a mod"));
            continue;
        }
        let (m, i) = scan_mod(&d);
        issues.extend(i);
        mods.extend(m);
    }
    (mods, issues)
}

// ---------------------------------------------------------------- load plan

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum ConflictKind {
    /// Two mods override the same file.
    File { key: String },
    /// Two mods set the same cell.
    Cell { table: String, key: String, column: Column },
    /// Two mods add the same row.
    Row { table: String, key: String },
}

impl fmt::Display for ConflictKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ConflictKind::File { key } => write!(f, "file {key}"),
            ConflictKind::Cell { table, key, column } => write!(f, "cell {table}[{key}].{column}"),
            ConflictKind::Row { table, key } => write!(f, "new row {table}[{key}]"),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Conflict {
    pub kind: ConflictKind,
    /// Mod ids involved, in load order; the last one wins.
    pub mods: Vec<String>,
}

impl Conflict {
    pub fn winner(&self) -> &str {
        self.mods.last().map(String::as_str).unwrap_or("")
    }
}

impl fmt::Display for Conflict {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {} (winner: {})", self.kind, self.mods.join(", "), self.winner())
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Skipped {
    pub id: String,
    pub reason: String,
}

#[derive(Debug, Clone, Default)]
pub struct LoadPlan {
    /// Active mods in load order.
    pub mods: Vec<ModInfo>,
    pub skipped: Vec<Skipped>,
    pub conflicts: Vec<Conflict>,
    pub issues: Vec<Issue>,
    /// `(id, preview.g4tx)` of every installed mod with a preview (active or not; first folder of an id), for the
    /// in-game Mods menu (served as [`preview_key`]).
    pub previews: Vec<(String, PathBuf)>,
}

impl LoadPlan {
    /// `id@ver id@ver ...` in load order.
    pub fn summary(&self) -> String {
        self.mods.iter().map(ModInfo::label).collect::<Vec<_>>().join(" ")
    }
    /// Winning file of every overridden key: key -> (mod id, absolute path).
    pub fn file_overrides(&self) -> BTreeMap<String, (String, PathBuf)> {
        let mut m = BTreeMap::new();
        for md in &self.mods {
            for f in &md.files {
                m.insert(f.key.clone(), (md.manifest.id.clone(), f.path.clone()));
            }
        }
        m
    }
    /// `(mod id, <mod>/lua)` of every active mod with Lua patches, in load order.
    pub fn lua_roots(&self) -> Vec<(String, PathBuf)> {
        self.mods.iter().filter(|m| !m.lua_scripts.is_empty()).map(|m| (m.manifest.id.clone(), m.lua_dir())).collect()
    }
    pub fn has_errors(&self) -> bool {
        self.issues.iter().any(|i| i.severity == Severity::Error)
    }
}

/// Game path (cpk_list key form) under which the loader serves a mod's preview texture.
pub fn preview_key(id: &str) -> String {
    format!("data/dx11/menu/evt_mods/preview/{id}.g4tx")
}

/// Texture / sprite names inside a mod's preview.g4tx (unique per mod: the engine caches textures by name).
pub fn preview_names(id: &str) -> (String, String, String) {
    (format!("evt_mp_{id}"), format!("evt_mp_{id}_l"), format!("evt_mp_{id}_t"))
}

/// Load-order sort key: the mods of `order` (top = index 0) load last, from the bottom of the list up; the others
/// first, by (priority, id).
fn load_key(m: &ModInfo, order: Option<&[String]>) -> (u8, i64, i32, String) {
    match order.and_then(|o| o.iter().position(|x| x == m.id())) {
        Some(p) => (1, -(p as i64), 0, m.manifest.id.clone()),
        None => (0, 0, m.manifest.priority, m.manifest.id.clone()),
    }
}

/// Ids of `mods` from the highest priority (loads last, wins conflicts) to the lowest: what the in-game list shows
/// top to bottom. Duplicate ids appear once.
pub fn display_order(mods: &[ModInfo], order: Option<&[String]>) -> Vec<String> {
    let mut v: Vec<&ModInfo> = mods.iter().collect();
    v.sort_by(|a, b| load_key(b, order).cmp(&load_key(a, order)));
    let mut out: Vec<String> = Vec::new();
    for m in v {
        if !out.iter().any(|x| x == m.id()) {
            out.push(m.manifest.id.clone());
        }
    }
    out
}

/// Order, filter and check `mods`. `enabled` = the `enabled.toml` list (None = all enabled); load order by
/// `priority` then `id` (no `load_order.toml`).
pub fn build_plan(mods: Vec<ModInfo>, enabled: Option<&[String]>) -> LoadPlan {
    build_plan_ordered(mods, enabled, None)
}

/// [`build_plan`] with the `load_order.toml` list (`order`, top = highest priority).
pub fn build_plan_ordered(mods: Vec<ModInfo>, enabled: Option<&[String]>, order: Option<&[String]>) -> LoadPlan {
    build_plan_full(mods, enabled, order, None)
}

/// Versions under which `name` is available among `active`: a mod with that id (its version) and every mod that
/// `provides` it (the explicit version, else the mod's own). `(index, version)`.
fn providers(active: &[ModInfo], name: &str) -> Vec<(usize, String)> {
    let mut v = Vec::new();
    for (i, a) in active.iter().enumerate() {
        if a.manifest.id == name {
            v.push((i, a.manifest.version.clone()));
            continue;
        }
        for p in &a.manifest.provides {
            if let Ok((n, ver)) = parse_provide(p) {
                if n == name {
                    v.push((i, ver.unwrap_or_else(|| a.manifest.version.clone())));
                    break;
                }
            }
        }
    }
    v
}

/// Why `m` cannot run with `active` (loader_min / requires / conflicts), None = fine.
fn skip_reason(m: &ModInfo, active: &[ModInfo], loader: Option<&str>) -> Option<String> {
    if let Some(lv) = loader {
        let min = m.manifest.loader_min.trim();
        if !min.is_empty() && compare_versions(lv, min) == std::cmp::Ordering::Less {
            return Some(format!("needs ModLoader >= {min} (this one: {lv})"));
        }
    }
    for r in m.requirements() {
        let prov: Vec<(usize, String)> = providers(active, &r.id).into_iter().filter(|(i, _)| active[*i].id() != m.id()).collect();
        if prov.is_empty() {
            return Some(format!("requires `{}`, which is not enabled", r.id));
        }
        if !prov.iter().any(|(_, v)| r.accepts(v)) {
            let have: Vec<&str> = prov.iter().map(|(_, v)| v.as_str()).collect();
            return Some(format!("requires `{}` {} (installed: {})", r.id, r.constraint(), have.join(", ")));
        }
    }
    let ids: Vec<&str> = active.iter().map(|a| a.id()).collect();
    m.manifest.conflicts.iter().find(|c| ids.contains(&c.as_str())).map(|c| format!("conflicts with enabled mod `{c}`"))
}

/// Dependencies of `active[i]`: indices of the mods providing its `requires` (in a version it accepts).
fn deps_of(active: &[ModInfo], i: usize) -> Vec<usize> {
    let mut d = Vec::new();
    for r in active[i].requirements() {
        for (j, v) in providers(active, &r.id) {
            if j != i && r.accepts(&v) && !d.contains(&j) {
                d.push(j);
            }
        }
    }
    d
}

/// Stable topological order of `active` (indices): every mod after the mods it requires, otherwise the given order
/// (a mod held back by a dependency is placed right after its last one). Err = the ids of a dependency cycle
/// (`["a", "b", "a"]`).
pub fn dependency_order(active: &[ModInfo]) -> Result<Vec<usize>, Vec<String>> {
    let deps: Vec<Vec<usize>> = (0..active.len()).map(|i| deps_of(active, i)).collect();
    let mut placed: Vec<usize> = Vec::with_capacity(active.len());
    let mut done = vec![false; active.len()];
    while placed.len() < active.len() {
        match (0..active.len()).find(|&i| !done[i] && deps[i].iter().all(|&d| done[d])) {
            Some(i) => {
                done[i] = true;
                placed.push(i);
            }
            None => {
                // every remaining mod waits for another remaining one: follow the waits until one repeats
                let mut path: Vec<usize> = Vec::new();
                let mut cur = (0..active.len()).find(|&i| !done[i]).unwrap_or(0);
                while !path.contains(&cur) {
                    path.push(cur);
                    cur = deps[cur].iter().copied().find(|&d| !done[d]).unwrap_or(cur);
                }
                let start = path.iter().position(|&x| x == cur).unwrap_or(0);
                let mut ids: Vec<String> = path[start..].iter().map(|&x| active[x].manifest.id.clone()).collect();
                ids.push(active[cur].manifest.id.clone());
                return Err(ids);
            }
        }
    }
    Ok(placed)
}

/// [`build_plan_ordered`] plus the ModLoader's own checks: `loader` = the running ModLoader version (a mod whose
/// `loader_min` is higher is skipped; None = not checked, e.g. the app). The active mods are then ordered so each one
/// loads after what it `requires` (topological, stable); a dependency cycle skips its mods (error issue).
pub fn build_plan_full(mods: Vec<ModInfo>, enabled: Option<&[String]>, order: Option<&[String]>, loader: Option<&str>) -> LoadPlan {
    let mut plan = LoadPlan::default();
    for m in &mods {
        if let Some(p) = &m.preview {
            if !plan.previews.iter().any(|(id, _)| id == m.id()) {
                plan.previews.push((m.manifest.id.clone(), p.clone()));
            }
        }
    }
    let mut active: Vec<ModInfo> = Vec::new();
    for m in mods {
        if active.iter().any(|a| a.manifest.id == m.manifest.id) {
            plan.skipped.push(Skipped { id: m.manifest.id.clone(), reason: format!("duplicate id (folder {})", m.dir.display()) });
        } else if enabled.is_some_and(|e| !e.contains(&m.manifest.id)) {
            plan.skipped.push(Skipped { id: m.manifest.id.clone(), reason: "not in enabled.toml".into() });
        } else {
            active.push(m);
        }
    }
    if let Some(e) = enabled {
        for id in e.iter().filter(|id| !active.iter().any(|a| &a.manifest.id == *id) && !plan.skipped.iter().any(|s| &s.id == *id)) {
            plan.issues.push(Issue::warn(Some(id), None, "listed in enabled.toml but not installed"));
        }
    }
    active.sort_by(|a, b| load_key(a, order).cmp(&load_key(b, order)));
    loop {
        // loader_min / requires / declared conflicts, until stable (skipping one mod can break another's requirement)
        let bad = active.iter().enumerate().find_map(|(i, m)| skip_reason(m, &active, loader).map(|r| (i, r)));
        if let Some((i, reason)) = bad {
            let m = active.remove(i);
            plan.skipped.push(Skipped { id: m.manifest.id, reason });
            continue;
        }
        // each mod after its dependencies; a cycle disables its mods (their dependents follow on the next pass)
        match dependency_order(&active) {
            Ok(idx) => {
                let mut slots: Vec<Option<ModInfo>> = std::mem::take(&mut active).into_iter().map(Some).collect();
                active = idx.into_iter().filter_map(|i| slots[i].take()).collect();
                break;
            }
            Err(cycle) => {
                let chain = cycle.join(" -> ");
                plan.issues.push(Issue::err(Some(&cycle[0]), None, format!("dependency cycle {chain}: mods skipped")));
                active.retain(|a| {
                    if cycle.contains(&a.manifest.id) {
                        plan.skipped.push(Skipped { id: a.manifest.id.clone(), reason: format!("dependency cycle {chain}") });
                        false
                    } else {
                        true
                    }
                });
            }
        }
    }
    // content conflicts
    let mut owners: BTreeMap<ConflictKind, Vec<String>> = BTreeMap::new();
    for m in &active {
        let mut kinds: BTreeSet<ConflictKind> = BTreeSet::new();
        kinds.extend(m.files.iter().map(|f| ConflictKind::File { key: f.key.clone() }));
        for d in &m.deltas {
            kinds.extend(
                d.delta.set.iter().map(|s| ConflictKind::Cell { table: s.table.clone(), key: s.key.clone(), column: s.column.clone() }),
            );
            kinds.extend(d.delta.add.iter().map(|a| ConflictKind::Row { table: a.table.clone(), key: a.key.clone() }));
        }
        for k in kinds {
            owners.entry(k).or_default().push(m.manifest.id.clone());
        }
    }
    plan.conflicts = owners.into_iter().filter(|(_, v)| v.len() > 1).map(|(kind, mods)| Conflict { kind, mods }).collect();
    plan.mods = active;
    plan
}

/// Scan `root` (= `<game>/mods`), read `enabled.toml` and `load_order.toml` and build the plan.
pub fn plan_root(root: &Path) -> LoadPlan {
    plan_root_for(root, None)
}

/// [`plan_root`] for the ModLoader `loader` version (`loader_min` checked; None = not checked).
pub fn plan_root_for(root: &Path, loader: Option<&str>) -> LoadPlan {
    let (mods, mut issues) = scan_root(root);
    let enabled = match read_enabled(root) {
        Ok(e) => e,
        Err(e) => {
            issues.push(Issue::err(None, Some(&root.join(ENABLED_FILE)), format!("{e}: every mod enabled")));
            None
        }
    };
    let order = match read_order(root) {
        Ok(o) => o,
        Err(e) => {
            issues.push(Issue::err(None, Some(&root.join(ORDER_FILE)), format!("{e}: order by priority")));
            None
        }
    };
    let mut plan = build_plan_full(mods, enabled.as_deref(), order.as_deref(), loader);
    issues.append(&mut plan.issues);
    plan.issues = issues;
    plan
}

// ---------------------------------------------------------------- load order, switching, profiles (file level)

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct OrderToml {
    order: Vec<String>,
}

/// `order = ["a", "b"]` (top = highest priority).
pub fn parse_order(text: &str) -> Result<Vec<String>, String> {
    let o: OrderToml = toml::from_str(text).map_err(|e| e.message().to_string())?;
    Ok(o.order)
}

pub fn order_text(ids: &[String]) -> String {
    let list: Vec<String> = ids.iter().map(|i| format!("{i:?}")).collect();
    format!("# top = highest priority: loads last and wins file / cell conflicts\norder = [{}]\n", list.join(", "))
}

/// `<root>/load_order.toml`: Ok(None) when missing (= order by `priority`).
pub fn read_order(root: &Path) -> Result<Option<Vec<String>>, String> {
    match std::fs::read_to_string(root.join(ORDER_FILE)) {
        Ok(t) => parse_order(&t).map(Some),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e.to_string()),
    }
}

/// Write `path` atomically (temp file + rename; the folder is created if missing).
pub fn write_atomic(path: &Path, text: &str) -> Result<(), String> {
    if let Some(d) = path.parent() {
        std::fs::create_dir_all(d).map_err(|e| format!("{}: {e}", d.display()))?;
    }
    let tmp = path.with_extension("toml.tmp");
    std::fs::write(&tmp, text).map_err(|e| format!("{}: {e}", tmp.display()))?;
    std::fs::rename(&tmp, path).map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        format!("{}: {e}", path.display())
    })
}

/// State of the mods folder the switch / move / profile operations work on.
#[derive(Debug, Clone)]
pub struct ModsState {
    /// Installed ids, first folder of each id, scan order.
    pub installed: Vec<String>,
    /// The enabled list (every installed id when there is no enabled.toml; unknown ids of the file kept).
    pub enabled: Vec<String>,
    /// Every installed id, top (highest priority) first.
    pub display: Vec<String>,
    pub has_enabled_file: bool,
    pub has_order_file: bool,
}

impl ModsState {
    pub fn read(root: &Path) -> Result<ModsState, String> {
        let (mods, _) = scan_root(root);
        let mut installed: Vec<String> = Vec::new();
        for m in &mods {
            if !installed.iter().any(|x| x == m.id()) {
                installed.push(m.manifest.id.clone());
            }
        }
        let file = read_enabled(root).map_err(|e| format!("{}: {e}", root.join(ENABLED_FILE).display()))?;
        let order = read_order(root).map_err(|e| format!("{}: {e}", root.join(ORDER_FILE).display()))?;
        Ok(ModsState {
            enabled: file.clone().unwrap_or_else(|| installed.clone()),
            display: display_order(&mods, order.as_deref()),
            installed,
            has_enabled_file: file.is_some(),
            has_order_file: order.is_some(),
        })
    }

    /// The enabled ids in display order (the in-game «Activos» column).
    pub fn active_display(&self) -> Vec<String> {
        self.display.iter().filter(|d| self.enabled.contains(d)).cloned().collect()
    }

    fn inactive_display(&self) -> Vec<String> {
        self.display.iter().filter(|d| !self.enabled.contains(d)).cloned().collect()
    }

    /// Switch `id` on (added to the enabled list and put at the TOP of the load order: highest priority) or off
    /// (removed; it goes to the top of the inactive part of the order). Returns (enabled list, order list).
    pub fn toggled(&self, id: &str, on: bool) -> (Vec<String>, Vec<String>) {
        let enabled = toggle_enabled(Some(self.enabled.clone()), &self.installed, id, on);
        let mut act: Vec<String> = self.active_display().into_iter().filter(|x| x != id).collect();
        let mut ina: Vec<String> = self.inactive_display().into_iter().filter(|x| x != id).collect();
        if on {
            act.insert(0, id.to_string());
        } else {
            ina.insert(0, id.to_string());
        }
        act.extend(ina);
        (enabled, act)
    }

    /// Move enabled mod `id` one step up (`delta` < 0: higher priority) or down in the «Activos» order. None when it
    /// cannot move (not enabled, already at that end). Returns the new order list (actives first, then the rest).
    pub fn moved(&self, id: &str, delta: i32) -> Option<Vec<String>> {
        let mut act = self.active_display();
        let i = act.iter().position(|x| x == id)?;
        let j = i as i64 + i64::from(delta.signum());
        if j < 0 || j as usize >= act.len() || delta == 0 {
            return None;
        }
        act.swap(i, j as usize);
        act.extend(self.inactive_display());
        Some(act)
    }
}

/// Switch `id` on / off: rewrites `enabled.toml` and `load_order.toml` (the one rule of the in-game menu and the
/// app's Mods card).
pub fn apply_toggle(root: &Path, id: &str, on: bool) -> Result<(), String> {
    let st = ModsState::read(root)?;
    if !st.installed.iter().any(|x| x == id) {
        return Err(format!("mod `{id}` is not installed"));
    }
    let (enabled, order) = st.toggled(id, on);
    write_atomic(&root.join(ENABLED_FILE), &enabled_text(&enabled))?;
    write_atomic(&root.join(ORDER_FILE), &order_text(&order))
}

/// Move an enabled mod one step in the load order (`delta` -1 = up = higher priority). Ok(false) = it cannot move.
pub fn apply_move(root: &Path, id: &str, delta: i32) -> Result<bool, String> {
    let st = ModsState::read(root)?;
    match st.moved(id, delta) {
        Some(order) => write_atomic(&root.join(ORDER_FILE), &order_text(&order)).map(|_| true),
        None => Ok(false),
    }
}

#[derive(Debug, Default, Deserialize, Serialize)]
struct ProfileToml {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    enabled: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    order: Option<Vec<String>>,
}

#[derive(Debug, Default, Deserialize, Serialize)]
struct ActiveProfileToml {
    active: String,
}

/// Profile names (`profiles/<name>.toml`, [`DEFAULT_PROFILE`] always first) and the index of the active one.
pub fn profiles(root: &Path) -> (Vec<String>, usize) {
    let mut names: Vec<String> = std::fs::read_dir(root.join(PROFILES_DIR))
        .map(|rd| {
            rd.flatten()
                .filter_map(|e| {
                    let p = e.path();
                    (p.extension().is_some_and(|x| x == "toml")).then(|| p.file_stem().map(|s| s.to_string_lossy().into_owned())).flatten()
                })
                .collect()
        })
        .unwrap_or_default();
    names.retain(|n| n != DEFAULT_PROFILE);
    names.sort();
    names.insert(0, DEFAULT_PROFILE.to_string());
    let active = std::fs::read_to_string(root.join(PROFILE_FILE))
        .ok()
        .and_then(|t| toml::from_str::<ActiveProfileToml>(&t).ok())
        .map(|a| a.active)
        .unwrap_or_else(|| DEFAULT_PROFILE.to_string());
    if !names.contains(&active) {
        names.push(active.clone());
    }
    let i = names.iter().position(|n| *n == active).unwrap_or(0);
    (names, i)
}

/// Profile name rules (it is the file name of `profiles/<name>.toml`): 1-32 characters, no path or reserved
/// characters, no `.`.
pub fn valid_profile_name(name: &str) -> bool {
    !name.trim().is_empty()
        && name.trim() == name
        && name.chars().count() <= 32
        && !name.chars().any(char::is_control)
        && !name.contains(['/', '\\', ':', '*', '?', '"', '<', '>', '|', '.'])
}

fn save_profile(root: &Path, name: &str) -> Result<(), String> {
    let p = ProfileToml { enabled: read_enabled(root)?, order: read_order(root)? };
    let text = toml::to_string(&p).map_err(|e| e.to_string())?;
    write_atomic(&root.join(PROFILES_DIR).join(format!("{name}.toml")), &text)
}

/// Switch to profile `name`: the current enabled / order lists are saved into the active profile, then the target's
/// lists become `enabled.toml` / `load_order.toml` (a list the profile does not have = the file is removed: every
/// mod enabled / order by priority).
pub fn switch_profile(root: &Path, name: &str) -> Result<(), String> {
    if !valid_profile_name(name) {
        return Err(format!("invalid profile name {name:?}"));
    }
    let (names, cur) = profiles(root);
    save_profile(root, &names[cur])?;
    let target = root.join(PROFILES_DIR).join(format!("{name}.toml"));
    let p: ProfileToml = match std::fs::read_to_string(&target) {
        Ok(t) => toml::from_str(&t).map_err(|e| format!("{}: {}", target.display(), e.message()))?,
        Err(_) => ProfileToml::default(),
    };
    for (file, list, text) in [(ENABLED_FILE, &p.enabled, p.enabled.as_deref().map(enabled_text)), (ORDER_FILE, &p.order, p.order.as_deref().map(order_text))] {
        let path = root.join(file);
        match (list, text) {
            (Some(_), Some(t)) => write_atomic(&path, &t)?,
            _ => {
                if path.exists() {
                    std::fs::remove_file(&path).map_err(|e| format!("{}: {e}", path.display()))?;
                }
            }
        }
    }
    write_atomic(&root.join(PROFILE_FILE), &format!("active = {}\n", toml_quote(name)))?;
    // the target file exists from now on (a copy of what it holds)
    save_profile(root, name)
}

/// New profile «<prefix> N» (first free N from 2) holding the current lists; it becomes the active one.
pub fn new_profile(root: &Path, prefix: &str) -> Result<String, String> {
    let (names, cur) = profiles(root);
    save_profile(root, &names[cur])?;
    let name = (2..1000).map(|n| format!("{prefix} {n}")).find(|n| !names.contains(n)).ok_or("too many profiles")?;
    if !valid_profile_name(&name) {
        return Err(format!("invalid profile name {name:?}"));
    }
    save_profile(root, &name)?;
    write_atomic(&root.join(PROFILE_FILE), &format!("active = {}\n", toml_quote(&name)))?;
    Ok(name)
}

fn profile_taken(names: &[String], name: &str) -> bool {
    // profile files live on a case-insensitive file system
    names.iter().any(|n| n.eq_ignore_ascii_case(name))
}

/// New profile `name` holding the current lists; it becomes the active one (the current lists are saved into the
/// profile that was active first). Err when the name is invalid or taken.
pub fn create_profile(root: &Path, name: &str) -> Result<(), String> {
    if !valid_profile_name(name) {
        return Err(format!("invalid profile name {name:?}"));
    }
    let (names, cur) = profiles(root);
    if profile_taken(&names, name) {
        return Err(format!("profile {name:?} already exists"));
    }
    save_profile(root, &names[cur])?;
    save_profile(root, name)?;
    write_atomic(&root.join(PROFILE_FILE), &format!("active = {}\n", toml_quote(name)))
}

/// Rename profile `old` to `new` (the active one stays active under its new name). The default profile
/// ([`DEFAULT_PROFILE`]) always exists and cannot be renamed.
pub fn rename_profile(root: &Path, old: &str, new: &str) -> Result<(), String> {
    if old == DEFAULT_PROFILE {
        return Err("the default profile cannot be renamed".into());
    }
    if !valid_profile_name(new) {
        return Err(format!("invalid profile name {new:?}"));
    }
    let (names, cur) = profiles(root);
    if !names.iter().any(|n| n == old) {
        return Err(format!("no profile {old:?}"));
    }
    let others: Vec<String> = names.iter().filter(|n| *n != old).cloned().collect();
    if profile_taken(&others, new) {
        return Err(format!("profile {new:?} already exists"));
    }
    let dir = root.join(PROFILES_DIR);
    let from = dir.join(format!("{old}.toml"));
    let to = dir.join(format!("{new}.toml"));
    let active = names[cur] == old;
    if from.is_file() {
        // via a temporary name, so a change of letter case only also works on Windows
        let tmp = dir.join(format!("{old}.rename.tmp"));
        std::fs::rename(&from, &tmp).map_err(|e| format!("{}: {e}", from.display()))?;
        std::fs::rename(&tmp, &to).map_err(|e| {
            let _ = std::fs::rename(&tmp, &from);
            format!("{}: {e}", to.display())
        })?;
    } else if active {
        save_profile(root, new)?;
    }
    if active {
        write_atomic(&root.join(PROFILE_FILE), &format!("active = {}\n", toml_quote(new)))?;
    }
    Ok(())
}

/// Delete profile `name` (not the default one). Deleting the active profile switches to the default profile first
/// (its lists become `enabled.toml` / `load_order.toml`).
pub fn delete_profile(root: &Path, name: &str) -> Result<(), String> {
    if name == DEFAULT_PROFILE {
        return Err("the default profile cannot be deleted".into());
    }
    if !valid_profile_name(name) {
        return Err(format!("invalid profile name {name:?}"));
    }
    let (names, cur) = profiles(root);
    if !names.iter().any(|n| n == name) {
        return Err(format!("no profile {name:?}"));
    }
    if names[cur] == name {
        switch_profile(root, DEFAULT_PROFILE)?;
    }
    let p = root.join(PROFILES_DIR).join(format!("{name}.toml"));
    if p.is_file() {
        std::fs::remove_file(&p).map_err(|e| format!("{}: {e}", p.display()))?;
    }
    Ok(())
}

// ---------------------------------------------------------------- per-mod warnings (in-game detail window)

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WarnKind {
    /// Requires a mod that is not installed.
    Missing,
    /// Requires a mod that is installed but off (or skipped).
    Disabled,
    /// Requires a version the installed mod does not have (`detail` = "<constraint>|<installed>").
    Version,
    /// Declared incompatible with an enabled mod (either side declares it).
    Incompatible,
    /// Files / cells also changed by another active mod (`count`, `wins` = this mod loads later).
    Shared,
    /// Second folder with an id that is already installed.
    Duplicate,
}

impl WarnKind {
    pub fn code(self) -> &'static str {
        match self {
            WarnKind::Missing => "missing",
            WarnKind::Disabled => "disabled",
            WarnKind::Version => "version",
            WarnKind::Incompatible => "incompatible",
            WarnKind::Shared => "shared",
            WarnKind::Duplicate => "duplicate",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModWarning {
    pub kind: WarnKind,
    pub other: String,
    pub count: usize,
    pub wins: bool,
    pub detail: String,
}

impl ModWarning {
    /// `kind|other|count|wins|detail` (one line of CMND_EVT_MODS_GET_TEXT(i, 7)).
    pub fn line(&self) -> String {
        format!("{}|{}|{}|{}|{}", self.kind.code(), self.other, self.count, u8::from(self.wins), self.detail)
    }
}

/// Warnings of mod `m` (one installed folder): requirements, declared incompatibilities with enabled mods, content
/// shared with other active mods (who wins). `all` = every installed folder, `enabled` = the enabled list.
pub fn mod_warnings(m: &ModInfo, all: &[ModInfo], enabled: &[String], plan: &LoadPlan) -> Vec<ModWarning> {
    let mut out = Vec::new();
    let w = |kind, other: &str, count, wins, detail: String| ModWarning { kind, other: other.to_string(), count, wins, detail };
    if all.iter().position(|x| x.id() == m.id()).is_some_and(|k| all[k].dir != m.dir) {
        out.push(w(WarnKind::Duplicate, m.id(), 0, false, String::new()));
        return out;
    }
    for r in m.requirements() {
        match all.iter().find(|x| x.id() == r.id) {
            None => out.push(w(WarnKind::Missing, &r.id, 0, false, r.constraint())),
            Some(x) if !r.accepts(&x.manifest.version) => {
                out.push(w(WarnKind::Version, &r.id, 0, false, format!("{}|{}", r.constraint(), x.manifest.version)))
            }
            Some(_) if !plan.mods.iter().any(|a| a.id() == r.id) => out.push(w(WarnKind::Disabled, &r.id, 0, false, r.constraint())),
            Some(_) => {}
        }
    }
    for x in all.iter().filter(|x| x.id() != m.id() && enabled.iter().any(|e| e == x.id())) {
        if m.manifest.conflicts.iter().any(|c| c == x.id()) || x.manifest.conflicts.iter().any(|c| c == m.id()) {
            if !out.iter().any(|o| o.kind == WarnKind::Incompatible && o.other == x.id()) {
                out.push(w(WarnKind::Incompatible, x.id(), 0, false, String::new()));
            }
        }
    }
    let mut shared: BTreeMap<String, (usize, bool)> = BTreeMap::new();
    for c in plan.conflicts.iter().filter(|c| c.mods.iter().any(|x| x == m.id())) {
        let pos = c.mods.iter().position(|x| x == m.id()).unwrap_or(0);
        for (k, o) in c.mods.iter().enumerate().filter(|(_, o)| *o != m.id()) {
            let e = shared.entry(o.clone()).or_insert((0, pos > k));
            e.0 += 1;
            e.1 = pos > k;
        }
    }
    for (o, (n, wins)) in shared {
        out.push(w(WarnKind::Shared, &o, n, wins, String::new()));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("evt_modfmt_{name}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }
    fn write(p: &Path, t: &str) {
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, t).unwrap();
    }
    fn mk(root: &Path, id: &str, extra: &str) {
        write(&root.join(id).join(MANIFEST), &format!("id = \"{id}\"\nname = \"{id}\"\nversion = \"1.0\"\n{extra}"));
    }

    #[test]
    fn toggle_enabled_rules() {
        let inst = vec!["a".to_string(), "b".to_string(), "a".to_string()];
        // no file: every installed id (once), minus the one switched off
        assert_eq!(toggle_enabled(None, &inst, "a", false), vec!["b"]);
        // on again: appended at the end; switching on twice does not duplicate
        let l = toggle_enabled(Some(vec!["b".into()]), &inst, "a", true);
        assert_eq!(l, vec!["b", "a"]);
        assert_eq!(toggle_enabled(Some(l.clone()), &inst, "a", true), vec!["b", "a"]);
        // unknown ids of the file survive; off twice is a no-op
        let l = toggle_enabled(Some(vec!["ghost".into(), "a".into()]), &inst, "a", false);
        assert_eq!(l, vec!["ghost"]);
        assert_eq!(toggle_enabled(Some(l), &inst, "a", false), vec!["ghost"]);
        // round trip through the file text
        let txt = enabled_text(&toggle_enabled(None, &inst, "b", false));
        assert_eq!(parse_enabled(&txt).unwrap(), vec!["a"]);
    }

    #[test]
    fn manifest_parse_and_validate() {
        let (m, w) = parse_manifest(&manifest_template("my_mod")).unwrap();
        assert_eq!(m.id, "my_mod");
        assert_eq!(m.priority, 0);
        assert!(w.is_empty(), "{w:?}");
        let (_, w) = parse_manifest("id=\"a\"\nname=\"A\"\nversion=\"1\"\nfoo=1").unwrap();
        assert_eq!(w, vec!["unknown key `foo`".to_string()]);
        assert!(parse_manifest("id=\"A b\"\nversion=\"1\"").is_err());
        assert!(parse_manifest("id=\"a\"").is_err()); // no version
        assert!(parse_manifest("id=\"a\"\nversion=\"1\"\nrequires=[\"a\"]").is_err());
        assert!(parse_manifest("id=\"a\"\nversion=\"1\"\nrequires=[\"b\"]\nconflicts=[\"b\"]").is_err());
        assert!(validate_id("x").is_ok() && validate_id("_x").is_err() && validate_id("").is_err());
    }

    #[test]
    fn enabled_list() {
        assert_eq!(parse_enabled("enabled = [\"a\", \"b\"]").unwrap(), vec!["a", "b"]);
        assert!(parse_enabled("enable = []").is_err());
        let ids = vec!["a".to_string(), "b-c".into()];
        assert_eq!(parse_enabled(&enabled_text(&ids)).unwrap(), ids);
    }

    #[test]
    fn delta_parse() {
        let t = r#"
[[set]]
table = "character/chara_param"
key = "pc_para_c01000010"
column = "skill_1"
value = 1234
[[set]]
key = "pc_para_c01000020"
column = 12
value = "x"
[[add]]
key = "pc_para_c09000010"
from = "pc_para_c01000010"
values = { skill_1 = 5, speed = 1.5 }
"#;
        let (d, w) = parse_delta(t, Some("character/chara_param")).unwrap();
        assert!(w.is_empty(), "{w:?}");
        assert_eq!(d.set.len(), 2);
        assert_eq!(d.set[0].column, Column::Name("skill_1".into()));
        assert_eq!(d.set[0].value, Cell::Int(1234));
        assert_eq!(d.set[1].table, "character/chara_param");
        assert_eq!(d.set[1].column, Column::Index(12));
        assert_eq!(d.add[0].values["speed"], Cell::Float(1.5));
        assert_eq!(d.add[0].from.as_deref(), Some("pc_para_c01000010"));
        assert!(parse_delta("[[set]]\nkey=\"a\"\ncolumn=\"c\"\nvalue=1", None).unwrap_err().contains("table"));
        assert!(parse_delta("[[set]]\ntable=\"../x\"\nkey=\"a\"\ncolumn=\"c\"\nvalue=1", None).is_err());
        assert!(parse_delta("[[set]]\ntable=\"t\"\nkey=\"a\"\ncolumn=\"c\"\nvalue=[1]", None).is_err());
        assert!(parse_delta("[[set]]\ntable=\"t\"\nkey=\"a\"\ncolumn=-1\nvalue=1", None).is_err());
        let (_, w) = parse_delta("x = 1\n[[add]]\ntable=\"t\"\nkey=\"k\"", None).unwrap();
        assert_eq!(w.len(), 2);
        assert_eq!(d.tables(), vec!["character/chara_param"]);
    }

    #[test]
    fn row_keys() {
        assert_eq!(crc32(b"123456789"), 0xCBF4_3926);
        assert_eq!(RowKey::parse("1234"), RowKey::Num(1234));
        // signed and unsigned spellings of the same 32-bit id
        assert_eq!(RowKey::parse("-1046485492"), RowKey::Num(-1046485492i32 as u32));
        assert_eq!(RowKey::parse("0xC1A0B2E4").hash(), 0xC1A0_B2E4);
        assert_eq!(RowKey::parse(&(0xC1A0_B2E4u32).to_string()), RowKey::parse(&(0xC1A0_B2E4u32 as i32).to_string()));
        // names hash with crc32; out-of-range numbers and signs in the middle are names
        let n = RowKey::parse("pc_para_c01000010");
        assert_eq!(n, RowKey::Name("pc_para_c01000010".into()));
        assert_eq!(n.hash(), crc32(b"pc_para_c01000010"));
        assert!(matches!(RowKey::parse("99999999999"), RowKey::Name(_)));
        assert!(matches!(RowKey::parse("1-2"), RowKey::Name(_)));
        assert!(matches!(RowKey::parse("-"), RowKey::Name(_)));
    }

    #[test]
    fn keys() {
        assert_eq!(normalize_key("data/common/X.cfg.bin").as_deref(), Some("data/common/x.cfg.bin"));
        assert_eq!(normalize_key(r"common\gamedata\a.bin").as_deref(), Some("data/common/gamedata/a.bin"));
        assert_eq!(normalize_key("./data/a").as_deref(), Some("data/a"));
        assert_eq!(normalize_key("C:/x/data/a").as_deref(), Some("data/a"));
        assert_eq!(normalize_key(r"C:\\x\\y"), None);
    }

    #[test]
    fn scan_and_plan() {
        let root = tmp("plan");
        mk(&root, "base", "priority = -1\n");
        mk(&root, "a", "requires = [\"base\"]\n");
        mk(&root, "b", "priority = 5\n");
        mk(&root, "c", "conflicts = [\"b\"]\n");
        mk(&root, "d", "requires = [\"missing\"]\n");
        mk(&root, "e", "requires = [\"d\"]\n");
        std::fs::create_dir_all(root.join("_off")).unwrap();
        std::fs::create_dir_all(root.join("notamod")).unwrap();
        write(&root.join("a/files/data/common/x.bin"), "aa");
        write(&root.join("b/files/data/common/X.bin"), "bbb");
        write(&root.join("b/files/readme.txt"), "");
        write(&root.join("b/files/data/Thumbs.db"), "");
        write(&root.join("a/lua/title_menu/10_a.lua"), "");
        write(&root.join("a/lua/empty/notes.txt"), "");
        write(&root.join("b/lua/title_menu/10_b.lua"), "");
        write(&root.join("a/data/character/chara_param.toml"), "[[set]]\nkey=\"k\"\ncolumn=\"c\"\nvalue=1\n");
        write(&root.join("b/data/x.toml"), "[[set]]\ntable=\"character/chara_param\"\nkey=\"k\"\ncolumn=\"c\"\nvalue=2\n");
        write(&root.join("b/data/bad.toml"), "[[set]]\nkey=1");
        let plan = plan_root(&root);
        let order: Vec<&str> = plan.mods.iter().map(|m| m.id()).collect();
        assert_eq!(order, vec!["base", "a", "b"]);
        assert_eq!(plan.summary(), "base@1.0 a@1.0 b@1.0");
        let sk: Vec<&str> = plan.skipped.iter().map(|s| s.id.as_str()).collect();
        assert_eq!(sk, vec!["c", "d", "e"]);
        assert_eq!(plan.conflicts.len(), 2, "{:?}", plan.conflicts);
        assert_eq!(plan.conflicts[0].kind, ConflictKind::File { key: "data/common/x.bin".into() });
        assert_eq!(plan.conflicts[0].winner(), "b");
        assert!(matches!(&plan.conflicts[1].kind, ConflictKind::Cell { table, .. } if table == "character/chara_param"));
        let fo = plan.file_overrides();
        assert_eq!(fo["data/common/x.bin"].0, "b");
        let lr: Vec<String> = plan.lua_roots().into_iter().map(|r| r.0).collect();
        assert_eq!(lr, vec!["a", "b"]);
        assert!(plan.has_errors()); // bad.toml
        let msgs: Vec<String> = plan.issues.iter().map(|i| i.to_string()).collect();
        assert!(msgs.iter().any(|m| m.contains("notamod")), "{msgs:?}");
        assert!(msgs.iter().any(|m| m.contains("readme.txt")));
        assert!(msgs.iter().any(|m| m.contains("script folder without")));
        // enabled.toml
        write(&root.join(ENABLED_FILE), "enabled = [\"base\", \"b\", \"ghost\"]");
        let plan = plan_root(&root);
        assert_eq!(plan.summary(), "base@1.0 b@1.0");
        assert!(plan.conflicts.is_empty());
        assert!(plan.issues.iter().any(|i| i.msg.contains("not installed")));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn voice_manifest() {
        let t = voice_pack_manifest("voces_es", "Voces \"ES\"", "Yo", "es", "Español");
        let (m, w) = parse_manifest(&t).unwrap();
        assert!(w.is_empty(), "{w:?}");
        assert_eq!(m.voice_language, Some(VoiceLanguage { code: "es".into(), name: "Español".into() }));
        assert_eq!(m.name, "Voces \"ES\"");
        assert_eq!(m.loader_modules, vec!["mods".to_string()]);
        for bad in ["ja", "en", "E", "espanol", "es_mx", "es-", "es-MX", ""] {
            assert!(validate_voice_code(bad).is_err(), "{bad}");
        }
        for good in ["es", "fr", "ita", "pt-br", "es-mx"] {
            assert!(validate_voice_code(good).is_ok(), "{good}");
        }
        let base = "id=\"a\"\nversion=\"1\"\n";
        assert!(parse_manifest(&format!("{base}voice_language = {{ code = \"ja\", name = \"x\" }}")).is_err());
        assert!(parse_manifest(&format!("{base}voice_language = {{ code = \"es\", name = \" \" }}")).is_err());
        assert!(parse_manifest(&format!("{base}voice_language = {{ code = \"es\", name = \"x\", extra = 1 }}")).is_err());
        assert!(parse_manifest(&format!("{base}voice_language = \"es\"")).is_err());
        assert!(voice_pack_readme("P", "es", "Español").contains("voice/es/c01000010/gl010.ogg"));
    }

    #[test]
    fn voice_keys() {
        assert_eq!(voice_key("es", "C01000010.acb"), "data/common/sound_asset/es/c01000010.acb");
        assert_eq!(voice_bank_of("data/common/sound_asset/es/c01000010.awb"), Some(("es", "c01000010", "awb")));
        assert_eq!(voice_bank_of("data/common/sound_asset/bgm.acb"), None);
        assert_eq!(voice_bank_of("data/common/sound_asset/es/x/y.acb"), None);
        assert_eq!(voice_bank_of("data/common/sound_asset/es/c.usm"), None);
        assert_eq!(voice_bank_of("data/common/sound/es/c.acb"), None);
    }

    #[test]
    fn root_bank_keys() {
        assert_eq!(root_bank_of("data/common/sound_asset/bgm.acb"), Some(("bgm", "acb")));
        assert_eq!(root_bank_of("data/common/sound_asset/BGM_CHRONICLE.AWB".to_ascii_lowercase().as_str()), Some(("bgm_chronicle", "awb")));
        assert_eq!(root_bank_of("data/common/sound_asset/es/c01000010.acb"), None, "voice bank, not a root bank");
        assert_eq!(root_bank_of("data/common/sound_asset/bgm.usm"), None, "not acb/awb");
        assert_eq!(root_bank_of("data/common/sound/bgm.acb"), None, "wrong folder");
        assert_eq!(root_bank_of("data/common/sound_asset/.acb"), None, "empty stem");
    }

    #[test]
    fn voice_packs_scan_and_languages() {
        let root = tmp("voice");
        let man = |prio: i32, code: &str, name: &str| format!("priority = {prio}\nvoice_language = {{ code = \"{code}\", name = \"{name}\" }}\n");
        mk(&root, "va", &man(0, "es", "Español"));
        mk(&root, "vb", &man(1, "es", "Castellano"));
        mk(&root, "vf", &man(2, "fr", "Français"));
        mk(&root, "plain", "");
        for (m, stem) in [("va", "c01000010"), ("va", "c01000020"), ("vb", "c01000010"), ("vb", "c01000030")] {
            write(&root.join(format!("{m}/files/data/common/sound_asset/es/{stem}.acb")), "a");
            write(&root.join(format!("{m}/files/data/common/sound_asset/es/{stem}.awb")), "w");
        }
        write(&root.join("va/files/data/common/sound_asset/es/lonely.acb"), "a"); // no .awb: not a bank
        write(&root.join("vf/files/data/common/sound_asset/ja/c01000010.acb"), "a"); // retail language: warned
        // sources (authoring, never scanned as game files)
        write(&root.join("va/voice/es/c01000010/gl010.ogg"), "x");
        write(&root.join("va/voice/es/c01000010/c01000010_whd00020.WAV"), "x");
        write(&root.join("va/voice/es/c01000010/gl010.mp3"), "x");
        write(&root.join("va/voice/es/c01000010/notes.txt"), "x");
        write(&root.join("va/voice/es/c01000010/cover.png"), "x");
        write(&root.join("va/voice/es/stray.ogg"), "x");
        write(&root.join("va/voice/es/bad name/gl010.ogg"), "x");
        write(&root.join("va/voice/es/README.md"), "x");
        let plan = plan_root(&root);
        let va = plan.mods.iter().find(|m| m.id() == "va").unwrap();
        assert_eq!(va.voice_banks(), vec!["c01000010", "c01000020"]);
        assert!(va.files.iter().all(|f| !f.key.contains("voice")));
        let langs = plan.voice_languages();
        assert_eq!(langs.len(), 2);
        assert_eq!((langs[0].code.as_str(), langs[0].name.as_str(), langs[0].banks), ("es", "Español", 3));
        assert_eq!(langs[0].mods, vec!["va", "vb"]);
        assert_eq!((langs[1].code.as_str(), langs[1].banks), ("fr", 0));
        assert!(plan.issues.iter().any(|i| i.msg.contains("voice bank of `ja` in a `fr` voice pack")));
        assert!(plan.conflicts.iter().any(|c| c.kind == ConflictKind::File { key: "data/common/sound_asset/es/c01000010.acb".into() }));
        let (src, issues) = voice_sources(&va.dir, "es");
        let cues: Vec<String> = src.iter().map(|s| format!("{}_{}", s.bank, s.suffix)).collect();
        assert_eq!(cues, vec!["c01000010_gl010", "c01000010_whd00020"]);
        let msgs: Vec<String> = issues.iter().map(|i| i.to_string()).collect();
        assert!(msgs.iter().any(|m| m.contains("two files for cue c01000010_gl010")), "{msgs:?}");
        assert!(msgs.iter().any(|m| m.contains("cover.png") && m.contains("not an audio format")), "{msgs:?}");
        assert!(msgs.iter().any(|m| m.contains("stray.ogg")), "{msgs:?}");
        assert!(msgs.iter().any(|m| m.contains("bad name")), "{msgs:?}");
        assert!(!msgs.iter().any(|m| m.contains("notes.txt") || m.contains("README")), "{msgs:?}");
        assert!(voice_sources(&root.join("plain"), "es").0.is_empty());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn requires_order_is_topological() {
        let root = tmp("order");
        mk(&root, "lib", "priority = 10\n");
        mk(&root, "user", "requires = [\"lib\"]\n");
        mk(&root, "free", "priority = 20\n");
        // user would load first by priority; it moves right after lib (free keeps its place)
        let plan = plan_root(&root);
        assert_eq!(plan.summary(), "lib@1.0 user@1.0 free@1.0");
        assert!(plan.issues.is_empty(), "{:?}", plan.issues);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn dependency_cycles_are_skipped() {
        let root = tmp("cycle");
        mk(&root, "a", "requires = [\"b\"]\n");
        mk(&root, "b", "requires = [\"c\"]\n");
        mk(&root, "c", "requires = [\"a\"]\n");
        mk(&root, "d", "requires = [\"a\"]\n");
        mk(&root, "e", "");
        let plan = plan_root(&root);
        assert_eq!(plan.summary(), "e@1.0");
        let cyc: Vec<&str> = plan.skipped.iter().filter(|s| s.reason.starts_with("dependency cycle")).map(|s| s.id.as_str()).collect();
        assert_eq!(cyc, vec!["a", "b", "c"]);
        assert!(plan.skipped.iter().any(|s| s.id == "d" && s.reason.contains("requires `a`")), "{:?}", plan.skipped);
        assert!(plan.issues.iter().any(|i| i.severity == Severity::Error && i.msg.contains("a -> b -> c -> a")), "{:?}", plan.issues);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn provides_and_loader_min() {
        let root = tmp("provides");
        mk(&root, "engine", "priority = 5\nprovides = [\"engine_api=2\", \"extra\"]\n");
        mk(&root, "user", "requires = [\"engine_api>=2\", \"extra>=1.0\"]\n");
        mk(&root, "old", "requires = [\"engine_api<2\"]\n");
        mk(&root, "new_loader", "loader_min = \"2.0\"\n");
        mk(&root, "plug", "plugin = \"plug.dll\"\nloader_min = \"1.0.0\"\n");
        let plan = plan_root_for(&root, Some("1.0.0"));
        assert_eq!(plan.summary(), "plug@1.0 engine@1.0 user@1.0");
        let why = |id: &str| plan.skipped.iter().find(|s| s.id == id).map(|s| s.reason.clone()).unwrap_or_default();
        assert_eq!(why("old"), "requires `engine_api` < 2 (installed: 2)");
        assert_eq!(why("new_loader"), "needs ModLoader >= 2.0 (this one: 1.0.0)");
        // without a loader version (app / menu) loader_min is not checked
        assert!(plan_root(&root).mods.iter().any(|m| m.id() == "new_loader"));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn plugin_manifest_fields() {
        let (m, w) = parse_manifest("id=\"q\"\nname=\"Q\"\nversion=\"1\"\nplugin=\"q_fix.dll\"\nprovides=[\"q\", \"api=1\"]\nloader_min=\"1.0.0\"").unwrap();
        assert_eq!((m.plugin.as_str(), m.provides.len(), m.loader_min.as_str()), ("q_fix.dll", 2, "1.0.0"));
        assert!(w.is_empty(), "{w:?}");
        for bad in ["plugin=\"../x.dll\"", "plugin=\"a\\\\b.dll\"", "plugin=\"x.exe\"", "provides=[\"A\"]", "provides=[\"a=\"]", "loader_min=\">=1\""] {
            assert!(parse_manifest(&format!("id=\"q\"\nversion=\"1\"\n{bad}")).is_err(), "{bad}");
        }
        assert_eq!(parse_provide("api = 1.2").unwrap(), ("api".to_string(), Some("1.2".to_string())));
        assert_eq!(parse_provide("api").unwrap(), ("api".to_string(), None));
    }

    #[test]
    fn requirements_and_versions() {
        use std::cmp::Ordering::*;
        assert_eq!(compare_versions("1.2", "1.2.0"), Equal);
        assert_eq!(compare_versions("1.10", "1.9"), Greater);
        assert_eq!(compare_versions("v2.0", "1.99"), Greater);
        assert_eq!(compare_versions("1.0-beta", "1.0-alpha"), Greater);
        let r = parse_requirement("base >= 1.2").unwrap();
        assert_eq!(r.id, "base");
        assert!(r.accepts("1.2") && r.accepts("1.10") && !r.accepts("1.1.9"));
        assert_eq!(r.constraint(), ">= 1.2");
        assert!(parse_requirement("base").unwrap().accepts("0"));
        assert!(parse_requirement("base=1.0").unwrap().accepts("1.0.0"));
        assert!(!parse_requirement("base<1.0").unwrap().accepts("1.0"));
        for bad in ["Base>=1", "base>=", "base>> 1", "base>=1 2", ">=1"] {
            assert!(parse_requirement(bad).is_err(), "{bad}");
        }
        assert!(parse_manifest("id=\"a\"\nversion=\"1\"\nrequires=[\"a>=1\"]").is_err());
        let (m, w) = parse_manifest("id=\"a\"\nversion=\"1\"\nrequires=[\"b >= 2.0\"]\ntags=[\"Gráficos\", \"\"]\nupdated=\"2026-9-1\"").unwrap();
        assert_eq!(m.tags.len(), 2);
        assert!(w.iter().any(|x| x.contains("tag")) && w.iter().any(|x| x.contains("YYYY-MM-DD")), "{w:?}");
        assert_eq!(iso_date(0), "1970-01-01");
        assert_eq!(iso_date(1_790_553_600), "2026-09-28");
        assert!(manifest_template("x").contains("tags = []"));
        assert!(parse_manifest(&manifest_template("x")).unwrap().1.is_empty());
    }

    #[test]
    fn version_requirement_skips() {
        let root = tmp("verreq");
        mk(&root, "base", "");
        std::fs::write(root.join("base/mod.toml"), "id = \"base\"\nname = \"B\"\nversion = \"1.5\"\n").unwrap();
        mk(&root, "a", "requires = [\"base>=2\"]\n");
        mk(&root, "b", "requires = [\"base >= 1.2\"]\n");
        let plan = plan_root(&root);
        assert_eq!(plan.summary(), "base@1.5 b@1.0"); // b requires base: loads after it
        assert_eq!(plan.skipped[0].reason, "requires `base` >= 2 (installed: 1.5)");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn load_order_file() {
        let root = tmp("loadorder");
        mk(&root, "a", "priority = 5\n");
        mk(&root, "b", "");
        mk(&root, "c", "");
        mk(&root, "d", "");
        write(&root.join("a/files/data/common/x.bin"), "a");
        write(&root.join("c/files/data/common/x.bin"), "c");
        let plan = plan_root(&root);
        assert_eq!(plan.summary(), "b@1.0 c@1.0 d@1.0 a@1.0"); // priority, then id
        assert_eq!(plan.conflicts[0].winner(), "a");
        // top of load_order.toml = highest priority = loads last; unlisted mods load first
        write(&root.join(ORDER_FILE), "order = [\"c\", \"a\"]");
        let plan = plan_root(&root);
        assert_eq!(plan.summary(), "b@1.0 d@1.0 a@1.0 c@1.0");
        assert_eq!(plan.conflicts[0].winner(), "c");
        let (mods, _) = scan_root(&root);
        assert_eq!(display_order(&mods, read_order(&root).unwrap().as_deref()), vec!["c", "a", "d", "b"]);
        write(&root.join(ORDER_FILE), "order = 1");
        assert!(plan_root(&root).issues.iter().any(|i| i.msg.contains("order by priority")));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn toggle_and_move_files() {
        let root = tmp("togmove");
        for id in ["a", "b", "c", "d"] {
            mk(&root, id, "");
        }
        // no files yet: a..d enabled, display order d c b a (same priority: id order, last loads last)
        let st = ModsState::read(&root).unwrap();
        assert_eq!(st.display, vec!["d", "c", "b", "a"]);
        apply_toggle(&root, "b", false).unwrap();
        assert_eq!(read_enabled(&root).unwrap().unwrap(), vec!["a", "c", "d"]);
        assert_eq!(read_order(&root).unwrap().unwrap(), vec!["d", "c", "a", "b"]);
        assert_eq!(plan_root(&root).summary(), "a@1.0 c@1.0 d@1.0");
        // on again: top of the order (highest priority)
        apply_toggle(&root, "b", true).unwrap();
        assert_eq!(read_order(&root).unwrap().unwrap(), vec!["b", "d", "c", "a"]);
        assert_eq!(plan_root(&root).summary(), "a@1.0 c@1.0 d@1.0 b@1.0");
        // move: up = higher priority; the ends refuse
        assert!(apply_move(&root, "c", -1).unwrap());
        assert_eq!(read_order(&root).unwrap().unwrap(), vec!["b", "c", "d", "a"]);
        assert!(!apply_move(&root, "b", -1).unwrap());
        assert!(!apply_move(&root, "a", 1).unwrap());
        assert!(apply_move(&root, "a", -1).unwrap());
        assert_eq!(read_order(&root).unwrap().unwrap(), vec!["b", "c", "a", "d"]);
        // disabled mods do not move and sit below the active ones
        apply_toggle(&root, "c", false).unwrap();
        assert!(!apply_move(&root, "c", 1).unwrap());
        assert_eq!(read_order(&root).unwrap().unwrap(), vec!["b", "a", "d", "c"]);
        assert!(apply_toggle(&root, "zz", true).is_err());
        assert!(!root.join("enabled.toml.tmp").exists() && !root.join("load_order.toml.tmp").exists());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn profiles_switch_and_new() {
        let root = tmp("profiles");
        for id in ["a", "b"] {
            mk(&root, id, "");
        }
        assert_eq!(profiles(&root), (vec![DEFAULT_PROFILE.to_string()], 0));
        apply_toggle(&root, "b", false).unwrap();
        let name = new_profile(&root, "Perfil").unwrap();
        assert_eq!(name, "Perfil 2");
        assert_eq!(profiles(&root), (vec![DEFAULT_PROFILE.to_string(), "Perfil 2".to_string()], 1));
        // in «Perfil 2»: both on
        apply_toggle(&root, "b", true).unwrap();
        assert_eq!(plan_root(&root).summary(), "a@1.0 b@1.0");
        // back to «Principal»: b off again; «Perfil 2» kept its state
        switch_profile(&root, DEFAULT_PROFILE).unwrap();
        assert_eq!(plan_root(&root).summary(), "a@1.0");
        assert_eq!(profiles(&root).1, 0);
        switch_profile(&root, "Perfil 2").unwrap();
        assert_eq!(plan_root(&root).summary(), "a@1.0 b@1.0");
        assert!(switch_profile(&root, "../x").is_err());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn profiles_create_rename_delete() {
        let root = tmp("profiles_crd");
        for id in ["a", "b"] {
            mk(&root, id, "");
        }
        // «Solo A»: b off
        create_profile(&root, "Solo A").unwrap();
        assert!(create_profile(&root, "solo a").is_err(), "taken (case-insensitive)");
        assert!(create_profile(&root, "x.y").is_err() && create_profile(&root, " x").is_err());
        apply_toggle(&root, "b", false).unwrap();
        assert_eq!(plan_root(&root).summary(), "a@1.0");
        // rename the active one: stays active, keeps its lists
        rename_profile(&root, "Solo A", "Only A").unwrap();
        assert_eq!(profiles(&root), (vec![DEFAULT_PROFILE.to_string(), "Only A".to_string()], 1));
        assert!(rename_profile(&root, DEFAULT_PROFILE, "X").is_err());
        assert!(rename_profile(&root, "Only A", DEFAULT_PROFILE).is_err());
        // case-only rename
        rename_profile(&root, "Only A", "only a").unwrap();
        assert_eq!(profiles(&root).0, vec![DEFAULT_PROFILE.to_string(), "only a".to_string()]);
        switch_profile(&root, DEFAULT_PROFILE).unwrap();
        assert_eq!(plan_root(&root).summary(), "a@1.0 b@1.0");
        switch_profile(&root, "only a").unwrap();
        assert_eq!(plan_root(&root).summary(), "a@1.0");
        // delete the active one: back to the default profile and its lists
        delete_profile(&root, "only a").unwrap();
        assert_eq!(profiles(&root), (vec![DEFAULT_PROFILE.to_string()], 0));
        assert_eq!(plan_root(&root).summary(), "a@1.0 b@1.0");
        assert!(delete_profile(&root, DEFAULT_PROFILE).is_err());
        assert!(delete_profile(&root, "nope").is_err());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn warnings_per_mod() {
        let root = tmp("warn");
        mk(&root, "base", "");
        mk(&root, "a", "requires = [\"base\", \"gone\", \"b>=2\"]\n");
        mk(&root, "b", "conflicts = [\"c\"]\n");
        mk(&root, "c", "");
        mk(&root, "d", "");
        write(&root.join("c/files/data/common/x.bin"), "c");
        write(&root.join("d/files/data/common/x.bin"), "d");
        write(&root.join("d/files/data/common/y.bin"), "d");
        write(&root.join("c/files/data/common/y.bin"), "c");
        write(&root.join(ENABLED_FILE), "enabled = [\"a\", \"b\", \"c\", \"d\"]");
        let (all, _) = scan_root(&root);
        let plan = plan_root(&root);
        let enabled = read_enabled(&root).unwrap().unwrap();
        let get = |id: &str| mod_warnings(all.iter().find(|m| m.id() == id).unwrap(), &all, &enabled, &plan);
        let wa: Vec<String> = get("a").iter().map(ModWarning::line).collect();
        assert!(wa.contains(&"disabled|base|0|0|".to_string()), "{wa:?}");
        assert!(wa.contains(&"missing|gone|0|0|".to_string()), "{wa:?}");
        assert!(wa.contains(&"version|b|0|0|>= 2|1.0".to_string()), "{wa:?}");
        assert_eq!(get("c").iter().filter(|w| w.kind == WarnKind::Incompatible).count(), 1);
        let d = get("d");
        let s = d.iter().find(|w| w.kind == WarnKind::Shared).unwrap();
        assert_eq!((s.other.as_str(), s.count, s.wins), ("c", 2, true)); // d loads after c (id order)
        let c: Vec<ModWarning> = get("c").into_iter().filter(|w| w.kind == WarnKind::Shared).collect();
        assert_eq!((c[0].count, c[0].wins), (2, false));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn lua_fingerprints_file_is_valid() {
        let root = tmp("fpjson");
        mk(&root, "menu", "");
        write(&root.join("menu/lua/_fingerprints.json"), "{}");
        write(&root.join("menu/lua/notes.txt"), "x");
        write(&root.join("menu/lua/script/10_a.lua"), "-- a");
        let (m, issues) = scan_mod(&root.join("menu"));
        assert!(m.is_some());
        let lua_warns: Vec<String> = issues.iter().map(|i| i.to_string()).filter(|l| l.contains("lua")).collect();
        assert!(!lua_warns.iter().any(|l| l.contains("_fingerprints")), "{lua_warns:?}");
        assert!(lua_warns.iter().any(|l| l.contains("notes.txt")), "any other loose file under lua/ is still ignored: {lua_warns:?}");
        let _ = std::fs::remove_dir_all(&root);
    }
}
