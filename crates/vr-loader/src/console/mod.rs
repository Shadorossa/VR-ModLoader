//! Module `console`: the **ModLoader console** (docs/app/modloader-consola.md). A separate console window (title
//! "ModLoader") behind the game that mirrors `evt_loader\loader.log` live, coloured by level / category, with an
//! input line for commands. Off by default (`[modules] console = false`).
//!
//! **Categorised game logging.** Besides the loader's own lines (category `loader`), cheap taps on the engine write
//! lines tagged `[<category>]` into `loader.log` while their category is on (all off by default except `loader`):
//!
//! | category | source (docs) | cost |
//! |---|---|---|
//! | `states` | soccer scene state byte `g_soccerState 0x21F65AA` + `g_sceneSoccer`, read by a poller thread (`crate::sigs::soccer_state_name`) | none on the game thread |
//! | `files` | `CCriFileOperate::Open 0x4E70C0` (the single open of every loader; error code `this+0x144`, hook-map.md §1), chained hook | one atomic load when off |
//! | `menus` | `CMenuController::OpenMenu 0x10DEC90` (chained hook) + Lua filters on `CMND_CLOSE/DELETE_MENU_OBJECT`, `CMND_RESERVE_MENU` | one atomic load when off |
//! | `sound` | `PlayCharaVoice 0x16FC7E0` (chained hook; plugins such as audio_engine chain on it too) | one atomic load when off |
//! | `match` | derived from the state changes (goal, focus battle, zone, half time, full time), the skill banner menu `common_skill_telop` and `CMND_RESERVE_SOCCER` | none extra |
//!
//! The hooks only copy a fixed-size [`Event`] into a bounded channel (`try_send`, never blocks; a full queue drops
//! and counts); a writer thread formats, rate-limits ([`RateLimiter`]) and writes the lines. Plugins can add their
//! own categories ([`register_category`]) and commands ([`cmds::register_command`]).
//!
//! Names are English only. The Spanish names of the first versions (`estados`, `ficheros`, `sonido`, `partido`,
//! `todas`, command `cerrar`, `log nivel`, `config claves`) still work as hidden aliases, also in `[console] categories`.
//!
//! Pure parts (unit-tested): configuration, category set, log-line classification and colours, event formatting,
//! rate limiter, match events derived from state changes; commands in [`cmds`]. Run time (Windows): [`rt`].

pub mod cmds;
pub mod names;
#[cfg(all(windows, target_arch = "x86_64"))]
pub mod rt;
pub mod sigs;

use crate::log::Level;
use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::Mutex;

/// `[console]` section.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct ConsoleCfg {
    /// Categories on at start (`loader`, `states`, `files`, `menus`, `sound`, `match`, plugin categories; the old Spanish names work too).
    pub categories: Vec<String>,
    /// ANSI colours per level / category.
    pub colors: bool,
    /// Command line (input) in the console window.
    pub input: bool,
    /// `files`: also files opened fine (false = only missing / failed ones).
    pub file_hits: bool,
    /// Lines per second and category written by the taps (the rest is counted and reported once a second).
    pub rate_limit: u32,
    /// Grey out the console window's close button (X / Alt+F4): closing can then never touch the game; the command
    /// `close` closes the console safely. false = the X works (ctrl handler detaches the console first).
    pub block_close: bool,
}

impl Default for ConsoleCfg {
    fn default() -> Self {
        ConsoleCfg {
            categories: vec!["loader".into()],
            colors: true,
            input: true,
            file_hits: true,
            rate_limit: 200,
            block_close: true,
        }
    }
}

// ---------------------------------------------------------------- categories

pub const CAT_LOADER: u8 = 0;
pub const CAT_STATES: u8 = 1;
pub const CAT_FILES: u8 = 2;
pub const CAT_MENUS: u8 = 3;
pub const CAT_SOUND: u8 = 4;
pub const CAT_MATCH: u8 = 5;
/// Built-in categories: (name, description). Index = category id.
pub const BUILTIN: &[(&str, &str)] = &[
    ("loader", "the ModLoader's own lines (modules, mods, plugins)"),
    ("states", "match state changes (InPlay, FocusBtl, Zone, OutOfPlay…) and match scene"),
    ("files", "every file the game opens: found / missing / failed"),
    ("menus", "menus opened, closed, deleted and reserved"),
    ("sound", "character voices requested (PlayCharaVoice) and whether they played"),
    ("match", "goals, focus battles, zone, half time, full time, techniques (banner), reserved matches"),
];
/// Spanish names of the first versions -> English (hidden aliases; `todas` = `all` is handled by `log`).
pub const CATEGORY_ALIASES: &[(&str, &str)] = &[("estados", "states"), ("ficheros", "files"), ("sonido", "sound"), ("partido", "match")];
pub const MAX_CATEGORIES: usize = 64;

/// Lowercase name with the old Spanish aliases mapped to the English one.
pub fn canonical_category(name: &str) -> String {
    let n = name.trim().to_ascii_lowercase();
    match CATEGORY_ALIASES.iter().find(|(a, _)| *a == n) {
        Some((_, e)) => e.to_string(),
        None => n,
    }
}

/// The set of categories and which ones are on. The global one is [`CATS`]; tests make their own.
pub struct CategorySet {
    mask: AtomicU64,
    /// Categories registered at run time (plugins): (name, description, owner); id = BUILTIN.len() + index.
    extra: Mutex<Vec<(String, String, String)>>,
}

impl CategorySet {
    pub const fn new() -> Self {
        CategorySet { mask: AtomicU64::new(1 << CAT_LOADER), extra: Mutex::new(Vec::new()) }
    }

    /// Hot path of every tap: one relaxed load.
    #[inline]
    pub fn enabled(&self, id: u8) -> bool {
        (id as usize) < MAX_CATEGORIES && self.mask.load(Ordering::Relaxed) >> id & 1 != 0
    }

    pub fn set(&self, id: u8, on: bool) {
        if (id as usize) >= MAX_CATEGORIES {
            return;
        }
        if on {
            self.mask.fetch_or(1 << id, Ordering::Relaxed);
        } else {
            self.mask.fetch_and(!(1u64 << id), Ordering::Relaxed);
        }
    }

    pub fn mask(&self) -> u64 {
        self.mask.load(Ordering::Relaxed)
    }

    pub fn id(&self, name: &str) -> Option<u8> {
        let n = canonical_category(name);
        if let Some(i) = BUILTIN.iter().position(|(b, _)| *b == n) {
            return Some(i as u8);
        }
        let x = self.extra.lock().unwrap_or_else(|e| e.into_inner());
        x.iter().position(|(e, _, _)| *e == n).map(|i| (BUILTIN.len() + i) as u8)
    }

    pub fn name(&self, id: u8) -> Option<String> {
        let i = id as usize;
        if i < BUILTIN.len() {
            return Some(BUILTIN[i].0.to_string());
        }
        self.extra.lock().unwrap_or_else(|e| e.into_inner()).get(i - BUILTIN.len()).map(|e| e.0.clone())
    }

    /// Every category: (id, name, description, on).
    pub fn list(&self) -> Vec<(u8, String, String, bool)> {
        let mut v: Vec<(u8, String, String, bool)> =
            BUILTIN.iter().enumerate().map(|(i, (n, d))| (i as u8, n.to_string(), d.to_string(), self.enabled(i as u8))).collect();
        let x = self.extra.lock().unwrap_or_else(|e| e.into_inner());
        for (i, (n, d, o)) in x.iter().enumerate() {
            let id = (BUILTIN.len() + i) as u8;
            v.push((id, n.clone(), format!("{d} (plugin {o})"), self.enabled(id)));
        }
        v
    }

    /// A new category (plugins). Names: `[a-z0-9_]`, 1..24 characters. The same name registered again by the same
    /// owner returns its id. Starts off.
    pub fn register(&self, name: &str, desc: &str, owner: &str) -> Result<u8, String> {
        let n = name.trim().to_ascii_lowercase();
        if n.is_empty() || n.len() > 24 || !n.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_') {
            return Err(format!("invalid category name: `{name}` (a-z, 0-9, _; up to 24)"));
        }
        if BUILTIN.iter().any(|(b, _)| *b == n)
            || CATEGORY_ALIASES.iter().any(|(a, _)| *a == n)
            || matches!(n.as_str(), "all" | "todas" | "level" | "nivel")
        {
            return Err(format!("category `{n}` is reserved"));
        }
        let mut x = self.extra.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(i) = x.iter().position(|(e, _, _)| *e == n) {
            return if x[i].2 == owner { Ok((BUILTIN.len() + i) as u8) } else { Err(format!("category `{n}` already belongs to {}", x[i].2)) };
        }
        if BUILTIN.len() + x.len() >= MAX_CATEGORIES {
            return Err(format!("at most {MAX_CATEGORIES} categories"));
        }
        x.push((n, desc.to_string(), owner.to_string()));
        Ok((BUILTIN.len() + x.len() - 1) as u8)
    }

    /// Switch on exactly the named categories; returns the unknown names.
    pub fn apply(&self, names: &[String]) -> Vec<String> {
        let mut mask = 0u64;
        let mut unknown = Vec::new();
        for n in names {
            match self.id(n) {
                Some(id) => mask |= 1 << id,
                None => unknown.push(n.clone()),
            }
        }
        self.mask.store(mask, Ordering::Relaxed);
        unknown
    }

    /// Names of the categories that are on.
    pub fn on_names(&self) -> Vec<String> {
        self.list().into_iter().filter(|c| c.3).map(|c| c.1).collect()
    }
}

impl Default for CategorySet {
    fn default() -> Self {
        Self::new()
    }
}

/// The global category set.
pub static CATS: CategorySet = CategorySet::new();

/// Taps: is category `id` on (one relaxed load).
#[inline]
pub fn enabled(id: u8) -> bool {
    CATS.enabled(id)
}

/// Names of `[console] categories` not known at start (plugin categories registered later): switched on when
/// they are registered.
static WANTED: Mutex<Vec<String>> = Mutex::new(Vec::new());

/// Apply `[console] categories` to the global set; unknown names wait for [`register_category`].
pub fn apply_config_categories(names: &[String]) {
    let unknown = CATS.apply(names);
    *WANTED.lock().unwrap_or_else(|e| e.into_inner()) = unknown;
}

/// **API** for plugins / other modules: register a category (off until `log <name> on`, or on at once when
/// `[console] categories` names it).
pub fn register_category(name: &str, desc: &str, owner: &str) -> Result<u8, String> {
    let id = CATS.register(name, desc, owner)?;
    let n = canonical_category(name);
    if WANTED.lock().unwrap_or_else(|e| e.into_inner()).contains(&n) {
        CATS.set(id, true);
    }
    Ok(id)
}

/// API for plugins / other modules: write `[<category>] text` to loader.log when the category is on. Unknown
/// categories are ignored. Not for per-frame hot paths (it formats on the calling thread).
pub fn emit_text(category: &str, text: &str) {
    if let Some(id) = CATS.id(category) {
        if CATS.enabled(id) {
            crate::log::write(Level::Info, format_args!("[{}] {text}", category.trim().to_ascii_lowercase()));
        }
    }
}

/// Run-time switches changeable from the console (`config set`).
pub static COLORS: AtomicBool = AtomicBool::new(true);
pub static FILE_HITS: AtomicBool = AtomicBool::new(true);
pub static RATE_LIMIT: AtomicU32 = AtomicU32::new(200);

// ---------------------------------------------------------------- loader.log lines (mirror)

/// Level and `[category]` of a `loader.log` line (`<date> <time> LEVEL [  tid] msg`); `category` is the tag at the
/// start of the message (`[states] …`), lowercase letters / digits / `_` only.
#[derive(Debug, Clone, PartialEq)]
pub struct LineInfo<'a> {
    pub level: Option<Level>,
    pub category: Option<&'a str>,
    pub msg: &'a str,
}

pub fn level_of_tag(t: &str) -> Option<Level> {
    match t {
        "ERROR" => Some(Level::Error),
        "WARN" => Some(Level::Warn),
        "INFO" => Some(Level::Info),
        "DEBUG" => Some(Level::Debug),
        "TRACE" => Some(Level::Trace),
        _ => None,
    }
}

/// Strict level name (`log level` / `config set loader.log_level`).
pub fn parse_level(s: &str) -> Option<Level> {
    match s.trim().to_ascii_lowercase().as_str() {
        "error" => Some(Level::Error),
        "warn" | "warning" => Some(Level::Warn),
        "info" => Some(Level::Info),
        "debug" => Some(Level::Debug),
        "trace" => Some(Level::Trace),
        _ => None,
    }
}

pub fn level_name(l: Level) -> &'static str {
    match l {
        Level::Error => "error",
        Level::Warn => "warn",
        Level::Info => "info",
        Level::Debug => "debug",
        Level::Trace => "trace",
    }
}

pub fn classify(line: &str) -> LineInfo<'_> {
    let (level, msg) = match line.find(" [") {
        Some(i) => {
            let level = line[..i].trim_end().rsplit(' ').next().and_then(level_of_tag);
            match line[i + 2..].find("] ") {
                Some(j) if level.is_some() => (level, &line[i + 2 + j + 2..]),
                _ => (None, line),
            }
        }
        None => (None, line),
    };
    let category = msg.strip_prefix('[').and_then(|r| {
        let end = r.find(']')?;
        let c = &r[..end];
        (!c.is_empty() && c.len() <= 24 && c.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')).then_some(c)
    });
    LineInfo { level, category, msg }
}

/// The mirror shows a line when its category is on (lines without a known category belong to `loader`); errors are
/// always shown.
pub fn show_line(info: &LineInfo, cats: &CategorySet) -> bool {
    if info.level == Some(Level::Error) {
        return true;
    }
    match info.category.and_then(|c| cats.id(c)) {
        Some(id) => cats.enabled(id),
        None => cats.enabled(CAT_LOADER),
    }
}

/// ANSI SGR colour of a line: ERROR / WARN by level, then the category, then DEBUG / TRACE grey; None = default.
pub fn color_of(info: &LineInfo, cats: &CategorySet) -> Option<&'static str> {
    match info.level {
        Some(Level::Error) => return Some("91"),
        Some(Level::Warn) => return Some("93"),
        _ => {}
    }
    if let Some(id) = info.category.and_then(|c| cats.id(c)) {
        return Some(match id {
            CAT_STATES => "96",
            CAT_FILES => "94",
            CAT_MENUS => "95",
            CAT_SOUND => "92",
            CAT_MATCH => "1;97",
            CAT_LOADER => "37",
            n => ["36", "35", "32", "33", "34"][n as usize % 5],
        });
    }
    match info.level {
        Some(Level::Debug) => Some("90"),
        Some(Level::Trace) => Some("2;90"),
        _ => None,
    }
}

// ---------------------------------------------------------------- events (hooks -> writer thread)

/// Fixed-size text of an event (copied on the game thread without allocating).
#[derive(Clone, Copy)]
pub struct Small {
    len: u8,
    buf: [u8; 120],
}

impl Small {
    pub const EMPTY: Small = Small { len: 0, buf: [0; 120] };
    /// Append bytes (truncated at the capacity).
    pub fn push(&mut self, b: &[u8]) {
        for &c in b {
            if self.len as usize >= self.buf.len() {
                return;
            }
            self.buf[self.len as usize] = c;
            self.len += 1;
        }
    }
    pub fn from_bytes(b: &[u8]) -> Small {
        let mut s = Small::EMPTY;
        s.push(b);
        s
    }
    pub fn as_str(&self) -> std::borrow::Cow<'_, str> {
        String::from_utf8_lossy(&self.buf[..self.len as usize])
    }
    pub fn is_full(&self) -> bool {
        self.len as usize == self.buf.len()
    }
}

impl std::fmt::Debug for Small {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:?}", self.as_str())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// `a` = previous state, `b` = new state, `f` = clock of the half (s).
    State,
    /// `a` = 1 created / 0 gone.
    Scene,
    /// `s` = path, `a` = 1 ok / 0 failed, `b` = error code (`this+0x144`) when failed.
    File,
    /// Native `OpenMenu`: `a` = name hash, `b` = OpenMenuParam.param, `f` = 1.0 ok / 0.0 refused.
    MenuOpen,
    /// Lua `CMND_CLOSE_MENU_OBJECT`: `a` = name hash.
    MenuClose,
    /// Lua `CMND_DELETE_MENU_OBJECT`: `a` = name hash.
    MenuDelete,
    /// Lua `CMND_RESERVE_MENU`: `a` = name hash, `b` = param (5th argument).
    MenuReserve,
    /// Lua `CMND_RESERVE_SOCCER`: `a` = game id, `b` = difficulty.
    SoccerReserve,
    /// `PlayCharaVoice`: `s` = `<bank>_<suffix>`, `a` = handle (0 = nothing played).
    Voice,
    /// Match event derived from a state change: `a` = previous state, `b` = new state, `f` = clock.
    Match,
    /// The skill banner (`common_skill_telop`) opened: a technique was used.
    Technique,
}

#[derive(Debug, Clone, Copy)]
pub struct Event {
    pub cat: u8,
    pub kind: Kind,
    pub a: u32,
    pub b: u32,
    pub f: f32,
    pub s: Small,
}

impl Event {
    pub fn new(cat: u8, kind: Kind) -> Event {
        Event { cat, kind, a: 0, b: 0, f: 0.0, s: Small::EMPTY }
    }
}

/// Soccer scene state name (the loader's table, `crate::sigs`).
pub fn state_name(s: u32) -> &'static str {
    if s > 0xFF {
        return "?";
    }
    crate::sigs::soccer_state_name(s as u8)
}

/// `mm:ss` of the clock of the half (game seconds).
pub fn clock_text(sec: f32) -> String {
    if !sec.is_finite() || sec < 0.0 {
        return "--:--".into();
    }
    let s = sec as u32;
    format!("{:02}:{:02}", s / 60, s % 60)
}

/// File error code (`CCriFileOperate+0x144`, hook-map.md §1).
pub fn file_error(code: u32) -> String {
    match code {
        0x8000_0001 => "not found".into(),
        0 => "failed (no code)".into(),
        c => format!("failed 0x{c:08X}"),
    }
}

fn menu_label(h: u32, names: &dyn Fn(u32) -> Option<&'static str>) -> String {
    match names(h) {
        Some(n) => n.to_string(),
        None => format!("0x{h:08X}"),
    }
}

/// Match event text for a state change (`match`), None when the change is not one.
pub fn match_event(from: u8, to: u8, clock: f32) -> Option<String> {
    let t = clock_text(clock);
    Some(match (from, to) {
        (_, 13) => format!("GOAL! (half clock {t})"),
        (_, 14) => format!("focus battle: starts ({t})"),
        (14, n) => format!("focus battle: ends -> {} ({t})", crate::sigs::soccer_state_name(n)),
        (_, 16) => format!("shoot zone ({t})"),
        (_, 15) => format!("scramble ({t})"),
        (_, 11) => "half time".to_string(),
        (_, 1) => "match: setup (Init)".to_string(),
        (_, 6) => format!("full time ({t})"),
        (_, 22) => "match abandoned (Retire)".to_string(),
        (_, 26) => "replay".to_string(),
        _ => return None,
    })
}

/// Text of an event (without the `[category]` tag).
pub fn format_event(e: &Event, names: &dyn Fn(u32) -> Option<&'static str>) -> String {
    match e.kind {
        Kind::State => format!(
            "{} -> {} ({}, clock {})",
            state_name(e.a),
            state_name(e.b),
            e.b,
            clock_text(e.f)
        ),
        Kind::Scene => if e.a != 0 { "match scene created".into() } else { "match scene closed".into() },
        Kind::File => {
            let p = e.s.as_str();
            let more = if e.s.is_full() { "…" } else { "" };
            if e.a != 0 {
                format!("ok {p}{more}")
            } else {
                format!("{}: {p}{more}", file_error(e.b))
            }
        }
        Kind::MenuOpen => {
            let r = if e.f != 0.0 { "opened" } else { "REFUSED" };
            if e.b != 0 {
                format!("{r}: {} (param {})", menu_label(e.a, names), e.b)
            } else {
                format!("{r}: {}", menu_label(e.a, names))
            }
        }
        Kind::MenuClose => format!("close: {}", menu_label(e.a, names)),
        Kind::MenuDelete => format!("delete: {}", menu_label(e.a, names)),
        Kind::MenuReserve => format!("reserved: {} (param {})", menu_label(e.a, names), e.b as i32),
        Kind::SoccerReserve => format!("match reserved: game 0x{:08X}, difficulty {}", e.a, e.b as i32),
        Kind::Voice => {
            if e.a != 0 {
                format!("voice {} (handle {})", e.s.as_str(), e.a)
            } else {
                format!("voice {}: not played (no cue in the loaded banks, or voices off)", e.s.as_str())
            }
        }
        Kind::Match => match_event(e.a as u8, e.b as u8, e.f).unwrap_or_else(|| format!("{} -> {}", state_name(e.a), state_name(e.b))),
        Kind::Technique => "technique used (banner common_skill_telop)".into(),
    }
}

// ---------------------------------------------------------------- rate limit

/// Per-category lines per second; the overflow of a second is reported when the next one starts.
pub struct RateLimiter {
    window_ms: u64,
    counts: [u32; MAX_CATEGORIES],
    dropped: [u32; MAX_CATEGORIES],
}

impl Default for RateLimiter {
    fn default() -> Self {
        Self::new()
    }
}

impl RateLimiter {
    pub fn new() -> Self {
        RateLimiter { window_ms: 0, counts: [0; MAX_CATEGORIES], dropped: [0; MAX_CATEGORIES] }
    }

    /// Start a new window when `now_ms` is past the current one; returns the (category, dropped) of the old one.
    pub fn tick(&mut self, now_ms: u64) -> Vec<(u8, u32)> {
        if now_ms < self.window_ms + 1000 {
            return Vec::new();
        }
        self.window_ms = now_ms;
        let out = self.dropped.iter().enumerate().filter(|(_, &d)| d > 0).map(|(i, &d)| (i as u8, d)).collect();
        self.counts = [0; MAX_CATEGORIES];
        self.dropped = [0; MAX_CATEGORIES];
        out
    }

    /// `limit` = lines per second (0 = unlimited).
    pub fn admit(&mut self, cat: u8, limit: u32) -> bool {
        let i = (cat as usize).min(MAX_CATEGORIES - 1);
        if limit != 0 && self.counts[i] >= limit {
            self.dropped[i] += 1;
            return false;
        }
        self.counts[i] += 1;
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_config() {
        let c = ConsoleCfg::default();
        assert_eq!(c.categories, vec!["loader".to_string()]);
        assert!(c.colors && c.input && c.file_hits && c.block_close);
        let t: ConsoleCfg = toml::from_str("categories = [\"states\", \"menus\"]\ncolors = false\n").unwrap();
        assert_eq!(t.categories.len(), 2);
        assert!(!t.colors && t.input);
    }

    #[test]
    fn category_filter() {
        let s = CategorySet::new();
        assert!(s.enabled(CAT_LOADER));
        for id in 1..BUILTIN.len() as u8 {
            assert!(!s.enabled(id), "{id} must start off");
        }
        s.set(CAT_STATES, true);
        assert!(s.enabled(CAT_STATES));
        s.set(CAT_LOADER, false);
        assert!(!s.enabled(CAT_LOADER));
        assert_eq!(s.id("FILES"), Some(CAT_FILES));
        // the old Spanish names are hidden aliases
        assert_eq!(s.id("ficheros"), Some(CAT_FILES));
        assert_eq!(s.id("Estados"), Some(CAT_STATES));
        assert_eq!(s.id("sonido"), Some(CAT_SOUND));
        assert_eq!(s.id("partido"), Some(CAT_MATCH));
        assert_eq!(s.id("nada"), None);
        // plugin categories
        let id = s.register("my_mod", "tests", "mod_a").unwrap();
        assert_eq!(id as usize, BUILTIN.len());
        assert_eq!(s.register("my_mod", "again", "mod_a"), Ok(id));
        assert!(s.register("my_mod", "stolen", "mod_b").is_err());
        assert!(s.register("states", "", "mod_b").is_err());
        assert!(s.register("estados", "", "mod_b").is_err(), "aliases are reserved too");
        assert!(s.register("todas", "", "mod_b").is_err() && s.register("all", "", "mod_b").is_err());
        assert!(s.register("Mal Nombre", "", "mod_b").is_err());
        assert!(!s.enabled(id));
        assert_eq!(s.name(id).as_deref(), Some("my_mod"));
        // apply = exactly these
        let unknown = s.apply(&["menus".into(), "my_mod".into(), "zzz".into(), "estados".into()]);
        assert_eq!(unknown, vec!["zzz".to_string()]);
        assert!(s.enabled(CAT_MENUS) && s.enabled(id) && s.enabled(CAT_STATES) && !s.enabled(CAT_LOADER));
        assert_eq!(s.on_names(), vec!["states".to_string(), "menus".to_string(), "my_mod".to_string()]);
        assert!(!s.enabled(200));
    }

    #[test]
    fn classify_loader_lines() {
        let l = "2026-09-29 10:43:01.123 INFO  [ 4242] [states] InPlay -> FocusBtl (14, clock 12:03)";
        let i = classify(l);
        assert_eq!(i.level, Some(Level::Info));
        assert_eq!(i.category, Some("states"));
        assert!(i.msg.starts_with("[states]"));
        // lines of an older loader.log keep their colour / filter through the alias
        assert_eq!(classify("2026-09-29 10:43:01.123 INFO  [ 4242] [estados] x").category, Some("estados"));
        assert_eq!(CategorySet::new().id("estados"), Some(CAT_STATES));
        let w = classify("2026-09-29 10:43:01.123 WARN  [   12] quit_fix: something [x]");
        assert_eq!((w.level, w.category), (Some(Level::Warn), None));
        assert_eq!(w.msg, "quit_fix: something [x]");
        let e = classify("2026-09-29 10:43:01.123 ERROR [   12] [Mal] x");
        assert_eq!((e.level, e.category), (Some(Level::Error), None));
        let raw = classify("loose line without format");
        assert_eq!((raw.level, raw.category), (None, None));
        // filter: loader lines follow `loader`, category lines their category, errors always
        let s = CategorySet::new();
        assert!(!show_line(&i, &s));
        assert!(show_line(&w, &s));
        s.set(CAT_LOADER, false);
        assert!(!show_line(&w, &s));
        assert!(show_line(&e, &s));
        s.set(CAT_STATES, true);
        assert!(show_line(&i, &s));
        // colours
        assert_eq!(color_of(&e, &s), Some("91"));
        assert_eq!(color_of(&w, &s), Some("93"));
        assert_eq!(color_of(&i, &s), Some("96"));
        let d = classify("2026-09-29 10:43:01.123 DEBUG [   12] lua: x");
        assert_eq!(color_of(&d, &s), Some("90"));
    }

    #[test]
    fn event_texts() {
        let names = |h: u32| if h == 0x4A33CE3A { Some("common_skill_telop") } else { None };
        let mut e = Event::new(CAT_STATES, Kind::State);
        e.a = 9;
        e.b = 14;
        e.f = 125.5;
        assert_eq!(format_event(&e, &names), "InPlay -> FocusBtl (14, clock 02:05)");
        let mut f = Event::new(CAT_FILES, Kind::File);
        f.s = Small::from_bytes(b"data/common/x.cfg.bin");
        f.a = 0;
        f.b = 0x8000_0001;
        assert_eq!(format_event(&f, &names), "not found: data/common/x.cfg.bin");
        f.a = 1;
        assert_eq!(format_event(&f, &names), "ok data/common/x.cfg.bin");
        f.a = 0;
        f.b = 0x8000_0004;
        assert!(format_event(&f, &names).starts_with("failed 0x80000004"));
        let long = [b'a'; 300];
        let s = Small::from_bytes(&long);
        assert!(s.is_full());
        f.s = s;
        assert!(format_event(&f, &names).ends_with('…'));
        let mut m = Event::new(CAT_MENUS, Kind::MenuOpen);
        m.a = 0x4A33CE3A;
        m.f = 1.0;
        assert_eq!(format_event(&m, &names), "opened: common_skill_telop");
        m.a = 0x1234;
        m.f = 0.0;
        assert_eq!(format_event(&m, &names), "REFUSED: 0x00001234");
        let mut r = Event::new(CAT_MENUS, Kind::MenuReserve);
        r.a = 0x4A33CE3A;
        r.b = 4;
        assert_eq!(format_event(&r, &names), "reserved: common_skill_telop (param 4)");
        let mut v = Event::new(CAT_SOUND, Kind::Voice);
        v.s = Small::from_bytes(b"c05020700_armed");
        assert!(format_event(&v, &names).contains("not played"));
        v.a = 7;
        assert_eq!(format_event(&v, &names), "voice c05020700_armed (handle 7)");
    }

    #[test]
    fn match_events_from_states() {
        assert_eq!(match_event(9, 13, 61.0).as_deref(), Some("GOAL! (half clock 01:01)"));
        assert!(match_event(9, 14, 0.0).unwrap().starts_with("focus battle: starts"));
        assert!(match_event(14, 9, 0.0).unwrap().contains("ends -> InPlay"));
        assert!(match_event(9, 17, 0.0).is_none()); // OutOfPlay: only `states`
        assert!(match_event(9, 10, 0.0).is_none());
        assert_eq!(clock_text(-1.0), "--:--");
        assert_eq!(clock_text(f32::NAN), "--:--");
    }

    #[test]
    fn rate_limiter_counts_and_reports() {
        let mut r = RateLimiter::new();
        assert!(r.tick(5000).is_empty());
        for _ in 0..3 {
            assert!(r.admit(2, 3));
        }
        assert!(!r.admit(2, 3));
        assert!(!r.admit(2, 3));
        assert!(r.admit(1, 3));
        assert!(r.tick(5500).is_empty(), "same window");
        assert_eq!(r.tick(6000), vec![(2, 2)]);
        assert!(r.admit(2, 3));
        assert!(r.admit(9, 0), "0 = unlimited");
    }

    #[test]
    fn levels() {
        assert_eq!(parse_level("DEBUG"), Some(Level::Debug));
        assert_eq!(parse_level("warning"), Some(Level::Warn));
        assert_eq!(parse_level("verbose"), None);
        assert_eq!(level_name(Level::Trace), "trace");
    }
}
