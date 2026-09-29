//! Module `mods`: self-contained mod folders `<game>/mods/<id>/` layered by the loader
//! (docs/game/engine/mod-format.md; format, validation and load plan: crate `evt-modfmt`). Off by default
//! (`[modules] mods = false`).
//!
//! * `lua/<script>/*.lua`: handed to module `lua_patch`'s runner as extra roots (after `evt_loader/lua_patches`,
//!   mods in load order); the runner's two hooks are installed for `mods` even when `lua_patch` itself is off.
//! * `files/data/...`: whole-file overrides and new files, by **path redirection** ([`hooks`], Windows only):
//!   `fs.ResolveOverlayPath 0x4E8730` is hooked; when the requested path (normalised to the cpk_list key form
//!   `data/...`, lowercase) is overridden by an enabled mod, the detour writes the mod file's absolute path into the
//!   engine's `out` buffer and returns non-NULL, so `CCriFileOperate::Open` skips the cpk_list lookup and reads the
//!   file **loose** (docs/game/engine/hook-map.md §1, "How to redirect reads"). CPK-packed and loose entries are
//!   served the same way; `cpk_list.cfg.bin` is never edited on disk. Other paths go to the original function.
//!   Once the engine has loaded cpk_list, every served file is also registered **in memory** as a loose cpk_list
//!   record under its root-relative path ([`cpklist`]) and the engine's "overlay goes through cpk_list" byte is set,
//!   so `Open` treats it exactly like a file the app installs loose (size known at open time).
//! * `data/<table>.toml`: cell deltas, merged at boot ([`merge`]): every table file touched by an active mod is
//!   rebuilt once with the cells / rows of every mod (load order, later wins) into `evt_loader/cache/merged/data/...`
//!   and served by the same overlay as a whole-file override.
//! * voice packs (`voice_language` in mod.toml, [`voice`]): while the «Idioma de voz» setting names a pack language,
//!   the same detour serves `sound_asset/ja|en/<bank>.acb|awb` from the pack's `sound_asset/<code>/` copy.
//!
//! The plan is read in DllMain (before the first file open, so boot-time files are redirected too); the init thread
//! logs it (`mods: <n> enabled: id@ver ...`, skipped mods / conflicts / problems at WARN).

pub mod cmds;
pub mod cpklist;
pub mod merge;
pub mod sigs;
pub mod tables;
pub mod voice;
#[cfg(all(windows, target_arch = "x86_64"))]
pub mod hooks;

use evt_modfmt::{LoadPlan, Severity};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

pub use evt_modfmt::{normalize_key, plan_root, plan_root_for, MODS_DIR};

/// The plan read at the very start of DllMain (before the module switches are decided: a mod's `loader_modules`
/// turn built-in modules on), reused by the file-redirect setup.
static EARLY_PLAN: std::sync::Mutex<Option<LoadPlan>> = std::sync::Mutex::new(None);

pub fn set_early_plan(p: LoadPlan) {
    *EARLY_PLAN.lock().unwrap_or_else(|e| e.into_inner()) = Some(p);
}

pub fn take_early_plan() -> Option<LoadPlan> {
    EARLY_PLAN.lock().unwrap_or_else(|e| e.into_inner()).take()
}

/// Turn on, in `modules` (the `[modules]` table), every built-in loader module an active mod names in
/// `loader_modules`. Returns the log lines (modules turned on, unknown names).
pub fn enable_requested_modules(plan: &LoadPlan, modules: &mut toml::Table) -> Vec<(Lvl, String)> {
    let mut on: Vec<(String, Vec<String>)> = Vec::new();
    let mut out = Vec::new();
    for m in &plan.mods {
        for name in &m.manifest.loader_modules {
            match modules.get(name) {
                Some(toml::Value::Boolean(true)) if !on.iter().any(|(n, _)| n == name) => {}
                Some(toml::Value::Boolean(_)) => {
                    modules.insert(name.clone(), toml::Value::Boolean(true));
                    match on.iter_mut().find(|(n, _)| n == name) {
                        Some((_, ids)) => ids.push(m.manifest.id.clone()),
                        None => on.push((name.clone(), vec![m.manifest.id.clone()])),
                    }
                }
                _ => out.push((Lvl::Warn, format!("mods: {} asks for loader module `{name}`, which this ModLoader does not have", m.manifest.id))),
            }
        }
    }
    for (n, ids) in on {
        out.insert(0, (Lvl::Info, format!("mods: loader module `{n}` turned on by mod(s) {} (loader_modules)", ids.join(", "))));
    }
    out
}

/// [`enable_requested_modules`] on the loader's `[modules]` switches.
pub fn apply_loader_modules(plan: &LoadPlan, modules: &mut crate::config::ModulesCfg) -> Vec<(Lvl, String)> {
    let Ok(toml::Value::Table(mut t)) = toml::Value::try_from(&*modules) else { return Vec::new() };
    let lines = enable_requested_modules(plan, &mut t);
    if let Ok(m) = toml::Value::Table(t).try_into() {
        *modules = m;
    }
    lines
}

/// One redirected file: winning mod and the NUL-terminated path written into the engine's buffer.
#[derive(Debug, Clone, PartialEq)]
pub struct Redirect {
    pub module: String,
    pub cpath: Vec<u8>,
    /// Size of the mod file when the plan was read (the in-memory cpk_list record gets it).
    pub size: u64,
}

/// Key (`data/...`, lowercase) -> redirect.
#[derive(Debug, Clone, Default)]
pub struct Overlay {
    pub map: HashMap<String, Redirect>,
}

/// `path` as the engine's ANSI C string (`/` separators, NUL-terminated), None when it is not ASCII or does not fit
/// the engine's 256-byte buffer.
pub fn c_path(path: &Path) -> Option<Vec<u8>> {
    let s = path.to_str()?.replace('\\', "/");
    if !s.is_ascii() || s.len() >= sigs::OUT_BUF || s.contains('\0') {
        return None;
    }
    let mut v = s.into_bytes();
    v.push(0);
    Some(v)
}

/// Log lines of the last data-delta merge (set by [`Overlay::from_plan`] in DllMain, logged by [`report`] from the
/// init thread). None = no mod has deltas.
static MERGE_NOTES: std::sync::Mutex<Option<Vec<(Lvl, String)>>> = std::sync::Mutex::new(None);

/// Mod ids whose preview texture the overlay serves (set once in DllMain; the in-game menu only loads those).
static SERVED_PREVIEWS: std::sync::OnceLock<Vec<String>> = std::sync::OnceLock::new();

pub fn set_served_previews(o: &Overlay) {
    let ids = o.map.keys().filter_map(|k| k.strip_prefix("data/dx11/menu/evt_mods/preview/")).filter_map(|f| f.strip_suffix(".g4tx")).map(str::to_string).collect();
    let _ = SERVED_PREVIEWS.set(ids);
}

/// The preview of mod `id` is served this session (`evt_modfmt::preview_key`).
pub fn preview_served(id: &str) -> bool {
    SERVED_PREVIEWS.get().is_some_and(|v| v.iter().any(|x| x == id))
}

impl Overlay {
    /// The winning file of every override, plus the preview texture of every installed mod that has one
    /// (`evt_modfmt::preview_key`, active or not: the in-game Mods menu shows them all). Unusable paths are
    /// returned as problems (key, path).
    ///
    /// Data deltas: when an active mod has `data/*.toml`, the merged table files ([`merge::run`], game folder = the
    /// parent of the mods folder, cache in `<game>/evt_loader/cache/merged`) replace the whole-file override of their
    /// key (or are added); the merge's log lines are kept for [`report`].
    pub fn from_plan(plan: &LoadPlan) -> (Overlay, Vec<(String, PathBuf)>) {
        let mut o = Overlay::default();
        let mut bad = Vec::new();
        let mut entries = plan.file_overrides();
        for (id, path) in &plan.previews {
            entries.entry(evt_modfmt::preview_key(id)).or_insert_with(|| (id.clone(), path.clone()));
        }
        let game_dir = plan.mods.iter().find(|m| !m.deltas.is_empty()).and_then(|m| m.dir.parent()?.parent()).map(Path::to_path_buf);
        if let Some(gd) = game_dir {
            let res = merge::run(plan, &mut merge::GameSource::new(&gd), &gd.join("evt_loader").join(merge::CACHE_REL));
            for (key, path, label) in res.served {
                entries.insert(key, (label, path));
            }
            *MERGE_NOTES.lock().unwrap_or_else(|e| e.into_inner()) = Some(res.notes);
        }
        for (key, (module, path)) in entries {
            match c_path(&path) {
                Some(cpath) => {
                    let size = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
                    o.map.insert(key, Redirect { module, cpath, size });
                }
                None => bad.push((key, path)),
            }
        }
        (o, bad)
    }

    /// Redirect of an engine path (any form `normalize_key` accepts).
    pub fn lookup(&self, engine_path: &str) -> Option<(&str, &Redirect)> {
        if self.map.is_empty() {
            return None;
        }
        let k = normalize_key(engine_path)?;
        self.map.get_key_value(&k).map(|(k, v)| (k.as_str(), v))
    }
}

/// Loader modules a mod needs that are off: (mod id, module). `modules` = the `[modules]` table of config.toml.
pub fn missing_loader_modules(plan: &LoadPlan, modules: &toml::Table) -> Vec<(String, String)> {
    let mut v = Vec::new();
    for m in &plan.mods {
        for name in &m.manifest.loader_modules {
            // unknown names are reported by enable_requested_modules
            if matches!(modules.get(name), Some(toml::Value::Boolean(false))) {
                v.push((m.manifest.id.clone(), name.clone()));
            }
        }
    }
    v
}

/// Log level of a report line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Lvl {
    Info,
    Warn,
    Error,
}

/// The lines the init thread logs for `plan`.
pub fn report(plan: &LoadPlan, overlay: &Overlay, bad: &[(String, PathBuf)], missing: &[(String, String)]) -> Vec<(Lvl, String)> {
    let mut out = vec![(Lvl::Info, format!("mods: {} enabled: {}", plan.mods.len(), plan.summary()).trim_end().to_string())];
    for s in &plan.skipped {
        out.push((Lvl::Warn, format!("mods: {} skipped: {}", s.id, s.reason)));
    }
    for i in &plan.issues {
        out.push((if i.severity == Severity::Error { Lvl::Error } else { Lvl::Warn }, format!("mods: {i}")));
    }
    let merge_notes = MERGE_NOTES.lock().unwrap_or_else(|e| e.into_inner()).clone();
    for c in &plan.conflicts {
        // cell / row conflicts: the merge reports them with the values (and the resolved row / column)
        if merge_notes.is_some() && matches!(c.kind, evt_modfmt::ConflictKind::Cell { .. } | evt_modfmt::ConflictKind::Row { .. }) {
            continue;
        }
        out.push((Lvl::Warn, format!("mods: conflict: {c}")));
    }
    for (k, p) in bad {
        out.push((Lvl::Error, format!("mods: {k}: path not usable by the engine (non-ASCII or >= 256 bytes): {}", p.display())));
    }
    for (id, m) in missing {
        out.push((Lvl::Warn, format!("mods: {id} needs loader module `{m}`, which is off in config.toml")));
    }
    if !plan.previews.is_empty() {
        out.push((Lvl::Info, format!("mods: {} preview image(s) for the Mods menu", plan.previews.len())));
    }
    let lua = plan.lua_roots().len();
    let deltas: usize = plan.mods.iter().map(|m| m.deltas.iter().map(|d| d.delta.set.len() + d.delta.add.len()).sum::<usize>()).sum();
    out.extend(merge_notes.unwrap_or_default());
    out.push((Lvl::Info, format!("mods: {} file override(s), {lua} mod(s) with Lua patches, {deltas} data delta(s)", overlay.map.len())));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn overlay_from_plan() {
        let root = std::env::temp_dir().join(format!("evt_loader_mods_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        for (id, prio) in [("a", 0), ("b", 1)] {
            let d = root.join(id);
            std::fs::create_dir_all(d.join("files/data/common/gamedata")).unwrap();
            std::fs::create_dir_all(d.join("lua/title_menu")).unwrap();
            std::fs::write(d.join("mod.toml"), format!("id=\"{id}\"\nname=\"{id}\"\nversion=\"1\"\npriority={prio}\nloader_modules=[\"lua_bridge\",\"board\"]")).unwrap();
            std::fs::write(d.join("files/data/common/gamedata/X.cfg.bin"), id).unwrap();
            std::fs::write(d.join("lua/title_menu/a.lua"), "").unwrap();
        }
        let plan = plan_root(&root);
        let (o, bad) = Overlay::from_plan(&plan);
        assert!(bad.is_empty());
        let (k, r) = o.lookup(r"common\gamedata\x.cfg.bin").unwrap();
        assert_eq!(k, "data/common/gamedata/x.cfg.bin");
        assert_eq!(r.module, "b");
        assert!(r.cpath.ends_with(b"/files/data/common/gamedata/X.cfg.bin\0"));
        assert!(o.lookup("data/common/gamedata/y.cfg.bin").is_none());
        // previews: every installed mod with preview.g4tx, active or not
        std::fs::write(root.join("a/preview.g4tx"), "G4TX").unwrap();
        let plan2 = plan_root(&root);
        let (o2, _) = Overlay::from_plan(&plan2);
        let (_, r2) = o2.lookup("data/dx11/menu/evt_mods/preview/a.g4tx").unwrap();
        assert_eq!(r2.module, "a");
        assert!(r2.cpath.ends_with(b"/a/preview.g4tx\0"));
        let mut modules = toml::Table::new();
        modules.insert("lua_bridge".into(), toml::Value::Boolean(true));
        modules.insert("board".into(), toml::Value::Boolean(false));
        let miss = missing_loader_modules(&plan, &modules);
        assert_eq!(miss, vec![("a".to_string(), "board".to_string()), ("b".to_string(), "board".to_string())]);
        let rep = report(&plan, &o, &bad, &miss);
        assert_eq!(rep[0], (Lvl::Info, "mods: 2 enabled: a@1 b@1".to_string()));
        assert!(rep.iter().any(|(l, s)| *l == Lvl::Warn && s.contains("conflict: file data/common/gamedata/x.cfg.bin: a, b (winner: b)")));
        assert!(rep.last().unwrap().1.starts_with("mods: 1 file override(s), 2 mod(s)"));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn loader_modules_are_turned_on() {
        let root = std::env::temp_dir().join(format!("evt_loader_mods_req_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        for (id, lm) in [("a", "[\"quit_fix\", \"lua_bridge\"]"), ("b", "[\"quit_fix\", \"no_such_module\"]")] {
            std::fs::create_dir_all(root.join(id)).unwrap();
            std::fs::write(root.join(id).join("mod.toml"), format!("id=\"{id}\"
version=\"1\"
loader_modules={lm}
")).unwrap();
        }
        let plan = plan_root(&root);
        let mut m = crate::config::ModulesCfg::default();
        m.quit_fix = false;
        m.lua_bridge = true;
        let lines = apply_loader_modules(&plan, &mut m);
        assert!(m.quit_fix && m.lua_bridge);
        assert_eq!(lines[0], (Lvl::Info, "mods: loader module `quit_fix` turned on by mod(s) a, b (loader_modules)".to_string()));
        assert!(lines.iter().any(|(l, s)| *l == Lvl::Warn && s.contains("`no_such_module`")), "{lines:?}");
        assert_eq!(lines.len(), 2);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn c_paths() {
        assert_eq!(c_path(Path::new(r"D:\g\mods\a\files\data\x")).unwrap(), b"D:/g/mods/a/files/data/x\0".to_vec());
        assert!(c_path(Path::new("D:/é")).is_none());
        assert!(c_path(&PathBuf::from("x".repeat(300))).is_none());
    }
}
