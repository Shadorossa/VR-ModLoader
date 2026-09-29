//! Boot of the text engine: declarations → cache check → merge → served files → manifest. Runs at the plugin's early
//! phase (exe entry point, before any game code) and in the offline tool (`evt-text-engine prepare`, game closed).
//!
//! Two ways to reach the game ([`Serve`]):
//! * [`Serve::Cache`] (a ModLoader with `file_serve`): the merged tables are written at their real size to
//!   `evt_loader\cache\text_engine\data\common\text\<lang>\<table>.cfg.bin` and the plugin serves each one with
//!   `file_serve` (it wins over any mod's whole file): **shown at the first start**. Slots left by the fallback in
//!   `mods\text_engine\files\` ("legacy") are mapped by the overlay at DllMain: each one is covered by a served file
//!   (the merge, or the base content: role `idle`) so the plugin can delete it ([`retire_legacy`]).
//! * [`Serve::Slots`] (fallback, a ModLoader without `file_serve`): fixed-size slots in the engine's own `files\`
//!   ([`crate::fw::slots`]): a new slot shows from the next start.
//!
//! The orchestration is generic (inputs hash, cache hit, slot states); the text parts are [`crate::build`] (merge),
//! [`crate::table::pad_t2b`] (slot padding) and [`GameTables`] (text keys).

use crate::build::{self, Base, Index, ModInput, Tables};
use crate::decl::{self, ModText};
use crate::fw::cache::{self, Dep, InputHash};
use crate::fw::discover;
use crate::fw::game::{self, Source};
use crate::fw::slots::{self, SlotPolicy, SlotState};
use crate::fw::{ModDir, Notes};
use crate::keys::{self, KeyRef};
use crate::lang::{self, LANGS};
use crate::table::pad_t2b;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

/// Bump when the merge / serving rules change (invalidates every cache).
pub const ENGINE_VERSION: u32 = 2;
/// Cache folder under `evt_loader` (manifest + the served files, laid out like the game: `data\common\text\...`).
pub const CACHE_REL: &str = "cache/text_engine";
pub const MANIFEST: &str = "manifest.json";
/// Text tables live under this game path (slots: under `<text_engine mod>\files\`; cache: under [`CACHE_REL`]).
pub const SLOT_PREFIX: &str = "data/common/text";
const CHARA_BASE_PREFIX: &str = "data/common/gamedata/character/chara_base_";
const CHARA_BASE_V712: &str = "data/common/gamedata/character/chara_base_1.03.98.00.cfg.bin";

/// Base tables for the merge: other mods' whole files, else the game ([`game::base_file`]); `chara_base` also from
/// the ModLoader's data-delta merge (`evt_loader\cache\merged\...`, new characters added by deltas).
pub struct GameTables<'a> {
    pub src: &'a mut dyn Source,
    pub mods: &'a [ModDir],
    pub self_id: &'a str,
    pub loader_dir: Option<PathBuf>,
    /// Every file read (cache dependencies).
    pub deps: Vec<PathBuf>,
    /// key → mod whose whole file is served instead of the engine's (slots only: `file_serve` wins over it).
    pub shadowed: BTreeMap<String, String>,
}

impl Tables for GameTables<'_> {
    fn table(&mut self, lang: &str, table: &str) -> Result<Option<Base>, String> {
        let key = lang::key(lang, table);
        let b = game::base_file(self.src, self.mods, self.self_id, &key)?;
        Ok(b.map(|b| {
            self.deps.extend(b.deps);
            if let Some(m) = b.shadowed_by {
                self.shadowed.insert(key, m);
            }
            Base { bytes: b.bytes, label: b.label }
        }))
    }

    fn chara_base(&mut self) -> Result<Vec<u8>, String> {
        let keys = self.src.keys().unwrap_or_default();
        let key = game::latest_versioned(&keys, CHARA_BASE_PREFIX, ".cfg.bin").unwrap_or_else(|| CHARA_BASE_V712.to_string());
        if let Some(ld) = &self.loader_dir {
            let mut merged = ld.join("cache").join("merged");
            for part in key.split('/') {
                merged.push(part);
            }
            if let Ok(b) = std::fs::read(&merged) {
                self.deps.push(merged);
                return Ok(b);
            }
        }
        match game::base_file(self.src, self.mods, self.self_id, &key)? {
            Some(b) => {
                self.deps.extend(b.deps);
                Ok(b.bytes)
            }
            None => Err(format!("{key} not found")),
        }
    }
}

/// Owned [`GameTables`] for the run-time lookups ([`crate::index::FileLazy`]).
pub struct OwnedTables<S: Source> {
    pub src: S,
    pub mods: Vec<ModDir>,
    pub self_id: String,
    pub loader_dir: PathBuf,
}

impl<S: Source> OwnedTables<S> {
    fn with<R>(&mut self, f: impl FnOnce(&mut GameTables) -> R) -> R {
        let mut g = GameTables {
            src: &mut self.src,
            mods: &self.mods,
            self_id: &self.self_id,
            loader_dir: Some(self.loader_dir.clone()),
            deps: Vec::new(),
            shadowed: BTreeMap::new(),
        };
        f(&mut g)
    }
}

impl<S: Source> Tables for OwnedTables<S> {
    fn table(&mut self, lang: &str, table: &str) -> Result<Option<Base>, String> {
        self.with(|g| g.table(lang, table))
    }
    fn chara_base(&mut self) -> Result<Vec<u8>, String> {
        self.with(|g| g.chara_base())
    }
}

/// How the merged tables reach the game.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Serve {
    /// Written to the cache at their real size; the plugin serves them with `file_serve` (shown at this start).
    Cache,
    /// Fixed-size slots in the engine's `files\` (a ModLoader without `file_serve`).
    Slots(SlotPolicy),
}

/// One served file as recorded in the manifest.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FileRec {
    pub key: String,
    pub size: u64,
    pub mtime: u128,
    pub state: SlotState,
    /// `merge` (mods' texts), `idle` (no mod touches it now: base content), `prepared` (slots: for an inactive mod).
    pub role: String,
    pub mods: Vec<String>,
    /// Merged size (stale slots: what did not fit).
    pub content: u64,
    /// [`Serve::Cache`]: the file in the cache the plugin serves with `file_serve`. None = a slot in the engine's
    /// `files\` (served by the overlay).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file: Option<String>,
}

impl FileRec {
    /// Where the file is on disk.
    pub fn path(&self, self_dir: &Path) -> PathBuf {
        match &self.file {
            Some(f) => PathBuf::from(f),
            None => game::mod_file(self_dir, &self.key),
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct Manifest {
    version: u32,
    inputs: String,
    deps: Vec<Dep>,
    files: Vec<FileRec>,
    notes: Notes,
    index: Index,
}

/// Input of [`run`].
pub struct BootIn<'a> {
    pub self_id: &'a str,
    pub self_dir: &'a Path,
    /// `evt_loader` (cache + the ModLoader's delta-merge output).
    pub loader_dir: &'a Path,
    /// Active mods in load order (the engine included).
    pub mods: &'a [ModDir],
    /// Installed mods that are not active (slots only: their slots are prepared so enabling them needs one restart).
    pub inactive: &'a [ModDir],
    pub serve: Serve,
}

/// Result of [`run`].
#[derive(Debug, Clone, Default)]
pub struct BootOut {
    pub notes: Notes,
    pub index: Index,
    pub from_cache: bool,
    pub files: Vec<FileRec>,
    /// Active mods that have text declarations.
    pub text_mods: Vec<String>,
}

impl BootOut {
    /// `(lang, table)` → the file the game reads the merge from (served slot or served cache file).
    pub fn served(&self, self_dir: &Path) -> BTreeMap<(String, String), PathBuf> {
        self.files
            .iter()
            .filter(|s| s.state == SlotState::Served)
            .filter_map(|s| lang::split_key(&s.key).map(|(l, t)| ((l.to_string(), t), s.path(self_dir))))
            .collect()
    }

    /// `(game path, cache file)` the plugin serves with `file_serve` ([`Serve::Cache`]).
    pub fn to_serve(&self) -> Vec<(&str, PathBuf)> {
        self.files.iter().filter_map(|f| f.file.as_ref().map(|p| (f.key.as_str(), PathBuf::from(p)))).collect()
    }
}

/// The index and files of the last build, without checking or writing anything (the plugin's late path: the game
/// may already be reading the served files).
pub fn cached(loader_dir: &Path) -> Option<BootOut> {
    let man: Manifest = cache::read_json(&loader_dir.join(CACHE_REL).join(MANIFEST))?;
    (man.version == ENGINE_VERSION).then(|| BootOut { notes: Notes::default(), index: man.index, from_cache: true, files: man.files, text_mods: Vec::new() })
}

/// Declarations of every mod in `mods` that has any (warnings of unreadable files included).
pub fn read_decls(mods: &[ModDir]) -> Vec<(ModInput, Vec<discover::DeclFile>)> {
    let mut out = Vec::new();
    for m in mods {
        if !discover::has(&m.dir, decl::NAME) {
            continue;
        }
        let mut files = Vec::new();
        let mut errs = Vec::new();
        for r in discover::read(&m.dir, decl::NAME) {
            match r {
                Ok(f) => files.push(f),
                Err(e) => errs.push(format!("{e}: ignored")),
            }
        }
        let mut text = ModText::parse(&m.id, &files);
        text.warnings.splice(0..0, errs);
        out.push((ModInput { id: m.id.clone(), load_index: m.load_index, text }, files));
    }
    out
}

/// `(lang, table)` pairs a mod's declarations touch, without reading the game (bare keys skipped).
pub fn pairs_of(t: &ModText) -> BTreeSet<(String, String)> {
    let mut out = BTreeSet::new();
    for name in t.new_names() {
        let (table, _, _) = t.new_meta(name);
        for l in LANGS {
            out.insert((l.to_string(), table.clone()));
        }
    }
    for k in t.replace_keys() {
        let table = match keys::parse_key(k, &|_| false) {
            Ok(KeyRef::Alias { field, .. }) => field.table().to_string(),
            Ok(KeyRef::Table { table, .. }) => table,
            _ => continue,
        };
        for l in LANGS.iter().filter(|l| t.resolve_replace(k, l).is_some()) {
            out.insert((l.to_string(), table.clone()));
        }
    }
    out
}

fn whole_text_files(m: &ModDir) -> Vec<String> {
    slots::list(&m.dir, SLOT_PREFIX).into_iter().map(|(k, _)| k).collect()
}

/// Slot files in the engine's own `files\` (the fallback's output).
pub fn legacy_slots(self_dir: &Path) -> BTreeSet<String> {
    slots::list(self_dir, SLOT_PREFIX).into_iter().map(|(k, _)| k).collect()
}

/// Delete the engine's slots of `keys` (legacy of the fallback) once something else serves them, or with the game
/// closed. `(deleted, errors)`.
pub fn retire_legacy(self_dir: &Path, keys: &[String]) -> (usize, Vec<String>) {
    slots::retire(&self_dir.join("files"), keys)
}

/// The file of `key` in the cache.
pub fn cache_file(loader_dir: &Path, key: &str) -> PathBuf {
    let mut p = loader_dir.join(CACHE_REL);
    for part in key.split('/') {
        p.push(part);
    }
    p
}

fn inputs_hash(inp: &BootIn, active: &[(ModInput, Vec<discover::DeclFile>)], inactive: &[(ModInput, Vec<discover::DeclFile>)], legacy: &BTreeSet<String>) -> String {
    let mut h = InputHash::new(&format!("text_engine v{ENGINE_VERSION}"));
    h.str(inp.self_id);
    match inp.serve {
        Serve::Cache => {
            h.str("serve file_serve");
            for k in legacy {
                h.str(&format!("legacy {k}"));
            }
        }
        Serve::Slots(p) => {
            h.str(&format!("serve slots headroom {} align {}", p.min_headroom, p.align));
        }
    }
    for (m, files) in active {
        h.str(&format!("mod {} {}", m.id, m.load_index));
        for f in files {
            h.str(&f.rel).str(&f.text);
        }
    }
    for m in inp.mods.iter().filter(|m| m.id != inp.self_id) {
        let w = whole_text_files(m);
        if !w.is_empty() {
            h.str(&format!("whole {} {}", m.id, m.load_index));
            for k in w {
                h.str(&k);
            }
        }
    }
    for (m, files) in inactive {
        h.str(&format!("inactive {}", m.id));
        for f in files {
            h.str(&f.rel).str(&f.text);
        }
    }
    h.finish()
}

fn short(key: &str) -> &str {
    key.trim_start_matches("data/common/text/").trim_end_matches(".cfg.bin")
}

/// Notes of the served files for the log (also on a cache hit).
pub fn slot_notes(recs: &[FileRec], fresh: bool) -> Notes {
    let mut n = Notes::default();
    let names = |st: SlotState, role: Option<&str>| -> Vec<String> {
        recs.iter().filter(|s| s.state == st && role.is_none_or(|r| s.role == r)).map(|s| short(&s.key).to_string()).collect()
    };
    let served = names(SlotState::Served, Some("merge"));
    if !served.is_empty() {
        n.info(format!("{} merged text file(s): {}", served.len(), served.join(", ")));
    }
    let pending: Vec<&FileRec> = recs.iter().filter(|s| s.state == SlotState::Pending).collect();
    if !pending.is_empty() {
        let merge: Vec<&str> = pending.iter().filter(|s| s.role == "merge").map(|s| short(&s.key)).collect();
        if fresh && !merge.is_empty() {
            n.warn(format!(
                "{} new text file(s) created at this start ({}): this ModLoader has no file_serve and maps served files before plugins run, so these texts show from the NEXT start: restart the game once, or update the ModLoader (or run `evt-text-engine prepare --slots <game folder>` with the game closed after installing text mods)",
                merge.len(),
                merge.join(", ")
            ));
        }
        let prep = pending.iter().filter(|s| s.role == "prepared").count();
        if fresh && prep > 0 {
            n.info(format!("{prep} text file slot(s) prepared for installed mods that are not active (enabling them needs one restart only)"));
        }
    }
    for s in recs.iter().filter(|s| s.state == SlotState::Stale) {
        n.error(format!(
            "{}: the {} ({} bytes) does not fit the slot mapped at this start ({} bytes): the previous content stays. Fix: update the ModLoader (file_serve), or close the game and run `evt-text-engine prepare --slots <game folder>` (or delete mods\\<text_engine>\\files\\{} and start the game twice)",
            s.key,
            if s.role == "merge" { "merged table" } else { "game table" },
            s.content,
            s.size,
            s.key.replace('/', "\\")
        ));
    }
    n
}

fn stat_rec(p: &Path) -> (u64, u128) {
    cache::stat(p).map(|d| (d.size, d.mtime)).unwrap_or((0, 0))
}

/// The whole boot (see the module doc). `src` = the game (cpk_list is read only when something must be built).
pub fn run(inp: &BootIn, src: &mut dyn Source) -> BootOut {
    let active = read_decls(inp.mods);
    let inactive = match inp.serve {
        Serve::Slots(_) => read_decls(inp.inactive),
        // served at its real size at the start that enables it: nothing to prepare
        Serve::Cache => Vec::new(),
    };
    let text_mods: Vec<String> = active.iter().map(|(m, _)| m.id.clone()).collect();
    let mpath = inp.loader_dir.join(CACHE_REL).join(MANIFEST);
    let legacy = legacy_slots(inp.self_dir);
    let inputs = inputs_hash(inp, &active, &inactive, &legacy);

    // ---- cache hit
    if let Some(mut man) = cache::read_json::<Manifest>(&mpath) {
        let (files_ok, stale_offline) = match inp.serve {
            Serve::Cache => (man.files.iter().all(|s| s.file.is_some() && stat_rec(&s.path(inp.self_dir)) == (s.size, s.mtime)), false),
            Serve::Slots(p) => (
                man.files.iter().all(|s| s.file.is_none() && stat_rec(&s.path(inp.self_dir)) == (s.size, s.mtime))
                    && man.files.iter().map(|s| s.key.clone()).collect::<BTreeSet<_>>() == legacy,
                // offline (game closed) a stale slot can be resized now: rebuild
                !p.live && man.files.iter().any(|s| s.state == SlotState::Stale),
            ),
        };
        if man.version == ENGINE_VERSION && man.inputs == inputs && files_ok && !stale_offline && cache::unchanged(&man.deps) {
            let mut notes = man.notes.clone();
            // slots created at an earlier start are mapped now
            if matches!(inp.serve, Serve::Slots(p) if p.live) && man.files.iter().any(|s| s.state == SlotState::Pending) {
                for s in man.files.iter_mut().filter(|s| s.state == SlotState::Pending) {
                    s.state = SlotState::Served;
                }
                if let Err(e) = cache::write_json(&mpath, &man) {
                    notes.warn(format!("cache manifest: {e}"));
                }
            }
            notes.extend(slot_notes(&man.files, false));
            return BootOut { notes, index: man.index, from_cache: true, files: man.files, text_mods };
        }
    }

    // ---- build
    let mut notes = Notes::default();
    let mut tables = GameTables { src, mods: inp.mods, self_id: inp.self_id, loader_dir: Some(inp.loader_dir.to_path_buf()), deps: Vec::new(), shadowed: BTreeMap::new() };
    let inputs_m: Vec<ModInput> = active.iter().map(|(m, _)| m.clone()).collect();
    let built = if inputs_m.is_empty() { build::Built::default() } else { build::build(&inputs_m, &mut tables) };
    let mut recs: Vec<FileRec> = Vec::new();
    let pad = |c: &[u8], size: usize| pad_t2b(c, size);
    // one output: a cache file (real size) or a slot
    let put = |key: &str, bytes: &[u8], reserve: usize, role: &str, mods: Vec<String>, notes: &mut Notes| -> Option<FileRec> {
        let content = bytes.len() as u64;
        match inp.serve {
            Serve::Cache => {
                let path = cache_file(inp.loader_dir, key);
                match cache::write_atomic(&path, bytes) {
                    Ok(()) => {
                        let (size, mtime) = stat_rec(&path);
                        let file = Some(path.to_string_lossy().into_owned());
                        Some(FileRec { key: key.to_string(), size, mtime, state: SlotState::Served, role: role.into(), mods, content, file })
                    }
                    Err(e) => {
                        notes.error(format!("{key}: not written to the cache: {e}"));
                        None
                    }
                }
            }
            Serve::Slots(policy) => {
                let path = game::mod_file(inp.self_dir, key);
                match slots::put(&path, bytes, reserve, &pad, &policy) {
                    Ok(p) => {
                        let (size, mtime) = stat_rec(&path);
                        Some(FileRec { key: key.to_string(), size, mtime, state: p.state, role: role.into(), mods, content, file: None })
                    }
                    Err(e) => {
                        notes.error(format!("{key}: slot not written: {e}"));
                        None
                    }
                }
            }
        }
    };
    let mut done: BTreeSet<String> = BTreeSet::new();
    for o in &built.outputs {
        let key = lang::key(o.lang, &o.table);
        recs.extend(put(&key, &o.bytes, o.base_len, "merge", o.mods.clone(), &mut notes));
        done.insert(key);
    }
    if matches!(inp.serve, Serve::Slots(_)) {
        for (key, m) in &tables.shadowed {
            if done.contains(key) {
                notes.warn(format!(
                    "mod {m} ships the whole {key} and loads after {}: ITS file is served, not the text engine's merge (update the ModLoader: with file_serve the merge wins; or move its texts to text\\*.toml, or let it load before {})",
                    inp.self_id, inp.self_id
                ));
            }
        }
    }
    // idle (legacy) slots no mod touches now get the base content; slots for inactive mods are prepared
    let mut want: Vec<(String, &str, Vec<String>)> = legacy.iter().filter(|k| !done.contains(*k)).map(|k| (k.clone(), "idle", Vec::new())).collect();
    for (m, _) in &inactive {
        for (l, t) in pairs_of(&m.text) {
            let key = lang::key(&l, &t);
            if done.contains(&key) || legacy.contains(&key) {
                continue;
            }
            match want.iter_mut().find(|w| w.0 == key) {
                Some(w) => w.2.push(m.id.clone()),
                None => want.push((key, "prepared", vec![m.id.clone()])),
            }
        }
    }
    for (key, role, ms) in want {
        let Some((l, t)) = lang::split_key(&key) else {
            notes.warn(format!("{key}: not a text table path: left as it is"));
            continue;
        };
        match tables.table(l, &t) {
            Ok(Some(b)) => recs.extend(put(&key, &b.bytes, 0, role, ms, &mut notes)),
            Ok(None) => {
                if role == "idle" {
                    notes.warn(format!("{key}: slot of a text file the game does not have: left as it is (delete it with the game closed)"));
                    if matches!(inp.serve, Serve::Slots(_)) {
                        let (size, mtime) = stat_rec(&game::mod_file(inp.self_dir, &key));
                        recs.push(FileRec { key, size, mtime, state: SlotState::Served, role: role.into(), mods: ms, content: 0, file: None });
                    }
                }
            }
            Err(e) => notes.error(format!("{key}: {e}: slot left as it is")),
        }
    }
    // cache files of an earlier build no longer served
    if inp.serve == Serve::Cache {
        let keep: BTreeSet<&str> = recs.iter().map(|r| r.key.as_str()).collect();
        let old: Vec<String> = slots::list_under(&inp.loader_dir.join(CACHE_REL), SLOT_PREFIX).into_iter().map(|(k, _)| k).filter(|k| !keep.contains(k.as_str())).collect();
        let (_, errs) = slots::retire(&inp.loader_dir.join(CACHE_REL), &old);
        for e in errs {
            notes.debug(format!("old cache file not deleted: {e}"));
        }
    }
    let mut all_notes = built.notes.clone();
    all_notes.extend(notes);
    recs.sort_by(|a, b| a.key.cmp(&b.key));
    let man = Manifest { version: ENGINE_VERSION, inputs, deps: cache::stats(&tables.deps), files: recs.clone(), notes: all_notes.clone(), index: built.index.clone() };
    let mut out_notes = all_notes;
    out_notes.extend(slot_notes(&recs, true));
    if let Err(e) = cache::write_json(&mpath, &man) {
        out_notes.warn(format!("cache manifest not written: {e}"));
    }
    BootOut { notes: out_notes, index: built.index, from_cache: false, files: recs, text_mods }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fw::game::DirSource;
    use crate::table::{Kind, TextTable};

    fn write(p: &Path, b: &[u8]) {
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, b).unwrap();
    }

    struct Env {
        root: PathBuf,
        game: PathBuf,
        te: PathBuf,
        loader: PathBuf,
    }

    fn env(tag: &str) -> Env {
        let root = std::env::temp_dir().join(format!("evt-te-boot-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let game = root.join("game");
        for l in LANGS {
            write(&game.join(lang::key(l, "menu_text")), &crate::table::tests::sample());
            write(&game.join(lang::key(l, "chara_text")), &crate::build::tests::chara_text());
        }
        write(&game.join(CHARA_BASE_V712), &crate::build::tests::chara_base());
        let te = root.join("mods").join("text_engine");
        std::fs::create_dir_all(&te).unwrap();
        let loader = root.join("evt_loader");
        Env { game: game.clone(), te, loader, root }
    }

    fn slots_live(live: bool) -> Serve {
        Serve::Slots(SlotPolicy { min_headroom: 4096, align: 4096, live })
    }

    fn text_of(p: &Path, kind: Option<Kind>, id: u32, v: i32) -> Option<String> {
        let b = std::fs::read(p).ok()?;
        let t = TextTable::parse(&b).unwrap();
        t.find(kind, id, v).and_then(|(_, i)| t.text(i).map(str::to_string))
    }

    fn served_text(e: &Env, lang: &str, table: &str, kind: Option<Kind>, id: u32, v: i32) -> Option<String> {
        text_of(&game::mod_file(&e.te, &lang::key(lang, table)), kind, id, v)
    }

    #[test]
    fn cache_serving_first_start_and_legacy_slots() {
        let e = env("cache");
        let m = e.root.join("mods").join("mymod");
        write(&m.join("text").join("en.toml"), b"[new]\ngreeting = \"Hello\"\n[replace]\n\"chara.c01000010.name\" = \"Mark MOD\"\n");
        // a legacy slot of the fallback (system_text is not touched by the mod) + one the merge covers
        let legacy_sys = game::mod_file(&e.te, &lang::key("en", "system_text"));
        write(&legacy_sys, &crate::table::tests::sample());
        write(&e.game.join(lang::key("en", "system_text")), &crate::table::tests::sample());
        write(&game::mod_file(&e.te, &lang::key("en", "menu_text")), &[0u8; 8192]);
        let mods = vec![ModDir { id: "text_engine".into(), dir: e.te.clone(), load_index: 0 }, ModDir { id: "mymod".into(), dir: m.clone(), load_index: 1 }];
        let inp = BootIn { self_id: "text_engine", self_dir: &e.te, loader_dir: &e.loader, mods: &mods, inactive: &[], serve: Serve::Cache };
        // first start: built into the cache at the real size, every file to serve now (no restart warning)
        let r = run(&inp, &mut DirSource(e.game.clone()));
        assert!(!r.from_cache);
        assert!(r.notes.at(crate::fw::Lvl::Warn).next().is_none(), "{:?}", r.notes);
        assert_eq!(r.files.iter().filter(|f| f.role == "merge").count(), 18);
        assert_eq!(r.files.iter().filter(|f| f.role == "idle").map(|f| f.key.as_str()).collect::<Vec<_>>(), ["data/common/text/en/system_text.cfg.bin"]);
        assert!(r.files.iter().all(|f| f.state == SlotState::Served && f.size == f.content && f.file.is_some()));
        let serve = r.to_serve();
        assert_eq!(serve.len(), 19);
        let menu_en = cache_file(&e.loader, &lang::key("en", "menu_text"));
        assert!(serve.iter().any(|(k, p)| *k == "data/common/text/en/menu_text.cfg.bin" && *p == menu_en));
        let gid = r.index.keys["mymod.greeting"].id;
        assert_eq!(text_of(&cache_file(&e.loader, &lang::key("ja", "menu_text")), None, gid, 0).as_deref(), Some("Hello"));
        assert_eq!(text_of(&cache_file(&e.loader, &lang::key("de", "chara_text")), Some(Kind::Noun), 0xE5530F12, 0).as_deref(), Some("Mark MOD"));
        assert_eq!(r.served(&e.te)[&("en".to_string(), "menu_text".to_string())], menu_en);
        // the plugin served them: the legacy slots go
        let keys: Vec<String> = legacy_slots(&e.te).into_iter().collect();
        assert_eq!(retire_legacy(&e.te, &keys), (2, vec![]));
        assert!(!e.te.join("files").exists());
        // next start: the legacy set changed → rebuilt once, without the idle copy (pruned from the cache)
        let r = run(&inp, &mut DirSource(e.game.clone()));
        assert!(!r.from_cache && r.files.len() == 18 && r.files.iter().all(|f| f.role == "merge"));
        assert!(!cache_file(&e.loader, &lang::key("en", "system_text")).exists());
        // then: cache hit, nothing read from the game
        let r = run(&inp, &mut DirSource(e.root.join("nowhere")));
        assert!(r.from_cache && r.files.len() == 18);
        assert_eq!(r.index.keys["mymod.greeting"].id, gid);
        // the mod drops its rename: chara_text files leave the cache
        write(&m.join("text").join("en.toml"), b"[new]\ngreeting = \"Hello again\"\n");
        let r = run(&inp, &mut DirSource(e.game.clone()));
        assert!(!r.from_cache && r.files.len() == 9);
        assert!(!cache_file(&e.loader, &lang::key("de", "chara_text")).exists());
        assert_eq!(text_of(&menu_en, None, gid, 0).as_deref(), Some("Hello again"));
        // a cache file changed on disk: rebuilt
        std::fs::write(&menu_en, b"x").unwrap();
        let r = run(&inp, &mut DirSource(e.game.clone()));
        assert!(!r.from_cache);
        assert_eq!(text_of(&menu_en, None, gid, 0).as_deref(), Some("Hello again"));
        let _ = std::fs::remove_dir_all(&e.root);
    }

    #[test]
    fn slots_cache_and_restart_flow() {
        let e = env("flow");
        let m = e.root.join("mods").join("mymod");
        write(&m.join("text").join("en.toml"), b"[new]\ngreeting = \"Hello\"\n[replace]\n\"chara.c01000010.name\" = \"Mark MOD\"\n");
        let mods = vec![ModDir { id: "text_engine".into(), dir: e.te.clone(), load_index: 0 }, ModDir { id: "mymod".into(), dir: m.clone(), load_index: 1 }];
        let inp = BootIn { self_id: "text_engine", self_dir: &e.te, loader_dir: &e.loader, mods: &mods, inactive: &[], serve: slots_live(true) };
        // first start: slots created → pending (restart needed)
        let r = run(&inp, &mut DirSource(e.game.clone()));
        assert!(!r.from_cache);
        assert_eq!(r.files.len(), 18);
        assert!(r.files.iter().all(|s| s.state == SlotState::Pending && s.size % 4096 == 0 && s.file.is_none()), "{:?}", r.files);
        assert!(r.to_serve().is_empty());
        assert!(r.notes.at(crate::fw::Lvl::Warn).any(|w| w.contains("from the NEXT start")));
        let gid = r.index.keys["mymod.greeting"].id;
        assert_eq!(served_text(&e, "ja", "menu_text", None, gid, 0).as_deref(), Some("Hello"));
        assert_eq!(served_text(&e, "de", "chara_text", Some(Kind::Noun), 0xE5530F12, 0).as_deref(), Some("Mark MOD"));
        // second start: cache hit, pending → served, nothing read from the game
        let r = run(&inp, &mut DirSource(e.root.join("nowhere")));
        assert!(r.from_cache);
        assert!(r.files.iter().all(|s| s.state == SlotState::Served));
        assert_eq!(r.index.keys["mymod.greeting"].id, gid);
        assert!(r.notes.at(crate::fw::Lvl::Info).any(|w| w.contains("18 merged text file(s)")));
        // the mod changes a text: rebuilt in place, same slot size, served now
        write(&m.join("text").join("en.toml"), b"[new]\ngreeting = \"Hello again\"\n[replace]\n\"chara.c01000010.name\" = \"Mark MOD\"\n");
        let before = std::fs::metadata(game::mod_file(&e.te, &lang::key("en", "menu_text"))).unwrap().len();
        let r = run(&inp, &mut DirSource(e.game.clone()));
        assert!(!r.from_cache && r.files.iter().all(|s| s.state == SlotState::Served));
        assert_eq!(std::fs::metadata(game::mod_file(&e.te, &lang::key("en", "menu_text"))).unwrap().len(), before);
        assert_eq!(served_text(&e, "en", "menu_text", None, gid, 0).as_deref(), Some("Hello again"));
        // a huge text does not fit the mapped slot: stale, previous content kept, ERROR
        let big = "x".repeat(20_000);
        write(&m.join("text").join("en.toml"), format!("[new]\ngreeting = \"{big}\"\n").as_bytes());
        let r = run(&inp, &mut DirSource(e.game.clone()));
        let stale: Vec<&FileRec> = r.files.iter().filter(|s| s.state == SlotState::Stale).collect();
        assert_eq!(stale.len(), 9);
        assert_eq!(served_text(&e, "en", "menu_text", None, gid, 0).as_deref(), Some("Hello again"));
        assert!(r.notes.at(crate::fw::Lvl::Error).any(|w| w.contains("does not fit")));
        // the chara_text slots are idle now (base content)
        assert_eq!(served_text(&e, "de", "chara_text", Some(Kind::Noun), 0xE5530F12, 0).as_deref(), Some("Mark Evans"));
        assert!(r.files.iter().filter(|s| s.key.contains("chara_text")).all(|s| s.role == "idle"));
        // offline prepare resizes the slots
        let off = BootIn { serve: slots_live(false), ..inp };
        let r = run(&off, &mut DirSource(e.game.clone()));
        assert!(r.files.iter().all(|s| s.state == SlotState::Served));
        assert_eq!(served_text(&e, "en", "menu_text", None, gid, 0).map(|s| s.len()), Some(20_000));
        let _ = std::fs::remove_dir_all(&e.root);
    }

    #[test]
    fn inactive_mods_get_prepared_slots_and_whole_files_shadow() {
        let e = env("prep");
        let m = e.root.join("mods").join("sleeping");
        write(&m.join("text.toml"), b"[es.replace]\n\"menu_text:10\" = \"Hola\"\n");
        let w = e.root.join("mods").join("whole");
        write(&game::mod_file(&w, &lang::key("en", "menu_text")), &crate::table::tests::sample());
        let x = e.root.join("mods").join("x");
        write(&x.join("text").join("en.toml"), b"[replace]\n\"menu_text:20\" = \"W\"\n");
        let mods = vec![
            ModDir { id: "text_engine".into(), dir: e.te.clone(), load_index: 0 },
            ModDir { id: "x".into(), dir: x.clone(), load_index: 1 },
            ModDir { id: "whole".into(), dir: w.clone(), load_index: 2 },
        ];
        let inactive = vec![ModDir { id: "sleeping".into(), dir: m.clone(), load_index: 0 }];
        // with file_serve: the merge wins over the whole file (no warning), nothing prepared for inactive mods
        let inp = BootIn { self_id: "text_engine", self_dir: &e.te, loader_dir: &e.loader, mods: &mods, inactive: &inactive, serve: Serve::Cache };
        let r = run(&inp, &mut DirSource(e.game.clone()));
        assert_eq!(r.files.iter().filter(|s| s.role == "merge").count(), 9);
        assert!(r.files.iter().all(|s| s.role == "merge"));
        assert!(r.notes.at(crate::fw::Lvl::Warn).next().is_none(), "{:?}", r.notes);
        let inp = BootIn { serve: slots_live(true), ..inp };
        let r = run(&inp, &mut DirSource(e.game.clone()));
        // x's replace (default en → every language): 9 merge slots; es/menu_text already one of them
        assert_eq!(r.files.iter().filter(|s| s.role == "merge").count(), 9);
        assert_eq!(r.files.iter().filter(|s| s.role == "prepared").count(), 0);
        assert!(r.notes.at(crate::fw::Lvl::Warn).any(|w| w.contains("mod whole ships the whole data/common/text/en/menu_text.cfg.bin")), "{:?}", r.notes);
        // only the sleeping mod (es only, so es is its default language: every language): its slots get prepared
        // with the base content
        let mods2 = vec![mods[0].clone()];
        let inp2 = BootIn { mods: &mods2, ..inp };
        let _ = std::fs::remove_dir_all(e.te.join("files"));
        let r = run(&inp2, &mut DirSource(e.game.clone()));
        let prep: Vec<&FileRec> = r.files.iter().filter(|s| s.role == "prepared").collect();
        assert_eq!(prep.len(), 9);
        assert!(prep.iter().all(|s| s.key.ends_with("/menu_text.cfg.bin") && s.state == SlotState::Pending && s.mods == ["sleeping"]));
        // with default_lang = "none" only Spanish
        write(&m.join("text.toml"), b"default_lang = \"none\"
[es.replace]
\"menu_text:10\" = \"Hola\"
");
        let _ = std::fs::remove_dir_all(e.te.join("files"));
        let r = run(&inp2, &mut DirSource(e.game.clone()));
        let prep: Vec<&str> = r.files.iter().filter(|s| s.role == "prepared").map(|s| s.key.as_str()).collect();
        assert_eq!(prep, ["data/common/text/es/menu_text.cfg.bin"]);
        assert_eq!(served_text(&e, "es", "menu_text", None, 10, 0).as_deref(), Some("Hello"));
        let _ = std::fs::remove_dir_all(&e.root);
    }
}
