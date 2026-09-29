//! In-game «Mods» menu (`evt_mods_menu`, docs/game/engine/mods-menu.md): the `CMND_EVT_MODS_*` Lua commands of
//! module `mods`. The menu lists every folder of `<game>/mods/` (active, skipped and disabled mods) in display order
//! (top = highest priority, the one that wins conflicts), switches mods on / off, moves them in the load order and
//! switches profiles. Every change is a file rewrite (`enabled.toml`, `load_order.toml`, `profile.toml`,
//! `profiles/<name>.toml`, through the same `evt_modfmt` functions the app uses) and applies at the next start: the
//! overlay and the Lua roots are read once in DllMain / the init thread (`dirty` / `pending` tell the menu).
//!
//! | Command | Args | Returns |
//! |---|---|---|
//! | `CMND_EVT_MODS_COUNT` | – | `installed, active, skipped, conflicts, dirty, changes, profiles, profile` |
//! | `CMND_EVT_MODS_GET` | `i` (0-based row) | `ok, id, name, version, author, priority, enabled, state, lua, files, deltas, applied, pending, pos, size, preview, warnings, level` |
//! | `CMND_EVT_MODS_GET_TEXT` | `i, what` | `text` (0 description / 1 state detail / 2 requires / 3 conflicts / 4 loader modules / 5 content conflicts / 6 folder / 7 warnings `kind|other|count|wins|detail` per line / 8 tags one per line / 9 updated `YYYY-MM-DD`) |
//! | `CMND_EVT_MODS_SET_ENABLED` / `_SET_ACTIVE` | `i, on` (0 / 1) | `ok, enabled`: on = top of the order |
//! | `CMND_EVT_MODS_MOVE` | `i, delta` (-1 up = higher priority / +1 down) | `ok, moved` |
//! | `CMND_EVT_MODS_PROFILE_NAME` | `k` | `name` ("" out of range) |
//! | `CMND_EVT_MODS_PROFILE_SWITCH` | `k` | `ok` |
//! | `CMND_EVT_MODS_PROFILE_NEW` | `prefix` | `ok, k` (new profile «prefix N», now active) |
//! | `CMND_EVT_MODS_RESCAN` | – | `ok, dirty` |
//! | `CMND_EVT_MODS_ROOT` | – | `path, hasEnabledFile` |
//!
//! `state` 0 active / 1 skipped / 2 off; `pos` = place in the «Activos» column (1 = top), 0 when off; `level` 0 no
//! warning / 1 amber / 2 red; `preview` = the loader serves `preview_key(id)` (the app built preview.g4tx).
//! The pure part ([`View`]) is unit-tested; the command handlers live in `rt` (Windows only, Lua bridge on).

use evt_modfmt::{
    apply_move, apply_toggle, build_plan_full, display_order, mod_warnings, new_profile, profiles, read_enabled, read_order,
    scan_root, switch_profile, Conflict, Issue, ModInfo, ModWarning, WarnKind, ENABLED_FILE, ORDER_FILE,
};
use std::path::{Path, PathBuf};

/// Row state as the menu shows it (`CMND_EVT_MODS_GET` field `state`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    /// In the load plan.
    Active = 0,
    /// Enabled but skipped (missing `requires`, declared conflict, duplicate id).
    Skipped = 1,
    /// Not listed in `enabled.toml`.
    Disabled = 2,
}

#[derive(Debug, Clone)]
pub struct Entry {
    pub info: ModInfo,
    /// Listed in `enabled.toml` (or there is no file: everything enabled).
    pub enabled: bool,
    pub state: State,
    /// Why the mod is skipped ("" otherwise).
    pub reason: String,
    /// Place in the «Activos» column (1 = top = highest priority), 0 when off.
    pub pos: usize,
    pub warnings: Vec<ModWarning>,
}

impl Entry {
    pub fn id(&self) -> &str {
        self.info.id()
    }
    /// 0 none / 1 amber / 2 red (on but not loaded, a required mod missing or too old, duplicate folder).
    pub fn level(&self) -> i64 {
        let red = (self.enabled && self.state == State::Skipped)
            || self.warnings.iter().any(|w| matches!(w.kind, WarnKind::Missing | WarnKind::Version | WarnKind::Duplicate));
        if red {
            2
        } else if self.warnings.is_empty() {
            0
        } else {
            1
        }
    }
}

/// Every installed mod folder, with the plan's verdict for each one.
#[derive(Debug, Clone, Default)]
pub struct View {
    pub root: PathBuf,
    /// Rows in display order: highest priority first (duplicate folders after their first folder).
    pub entries: Vec<Entry>,
    /// Content conflicts among the active mods (file / cell / row).
    pub conflicts: Vec<Conflict>,
    pub issues: Vec<Issue>,
    /// Active ids in load order (what a fresh boot would apply).
    pub active: Vec<String>,
    /// `enabled.toml` exists.
    pub has_enabled_file: bool,
    /// Profile names and the active one.
    pub profiles: Vec<String>,
    pub profile: usize,
}

impl View {
    /// Scan `<root>` (= `<game>/mods`) and build the plan, keeping the skipped and disabled mods as rows.
    pub fn scan(root: &Path) -> View {
        let (mods, mut issues) = scan_root(root);
        let (enabled, has_file) = match read_enabled(root) {
            Ok(e) => {
                let has = e.is_some();
                (e, has)
            }
            Err(e) => {
                issues.push(Issue::err(None, Some(&root.join(ENABLED_FILE)), format!("{e}: every mod enabled")));
                (None, root.join(ENABLED_FILE).is_file())
            }
        };
        let order = read_order(root).unwrap_or_else(|e| {
            issues.push(Issue::err(None, Some(&root.join(ORDER_FILE)), format!("{e}: order by priority")));
            None
        });
        // same checks as the plan the loader applies (loader_min against this ModLoader)
        let plan = build_plan_full(mods.clone(), enabled.as_deref(), order.as_deref(), Some(crate::MODLOADER_VERSION));
        let display = display_order(&mods, order.as_deref());
        let enabled_list: Vec<String> = enabled.clone().unwrap_or_else(|| mods.iter().map(|m| m.manifest.id.clone()).collect());
        let mut firsts: Vec<(String, PathBuf)> = Vec::new();
        for m in &mods {
            if !firsts.iter().any(|(id, _)| id == m.id()) {
                firsts.push((m.manifest.id.clone(), m.dir.clone()));
            }
        }
        let mut entries: Vec<Entry> = Vec::with_capacity(mods.len());
        for m in &mods {
            let id = m.manifest.id.clone();
            let is_enabled = enabled_list.contains(&id);
            let duplicate = firsts.iter().any(|(fid, dir)| fid == &id && dir != &m.dir);
            let (state, reason) = if duplicate {
                (State::Skipped, format!("duplicate id (folder {})", m.dir.display()))
            } else if plan.mods.iter().any(|a| a.manifest.id == id) {
                (State::Active, String::new())
            } else if !is_enabled {
                (State::Disabled, String::new())
            } else if let Some(s) = plan.skipped.iter().find(|s| s.id == id) {
                (State::Skipped, s.reason.clone())
            } else {
                (State::Skipped, "not in the load plan".to_string())
            };
            let warnings = mod_warnings(m, &mods, &enabled_list, &plan);
            entries.push(Entry { info: m.clone(), enabled: is_enabled, state, reason, pos: 0, warnings });
        }
        let rank = |e: &Entry| display.iter().position(|d| d == e.id()).unwrap_or(usize::MAX);
        entries.sort_by(|a, b| {
            let dup = |e: &Entry| firsts.iter().any(|(fid, dir)| fid == e.id() && dir != &e.info.dir);
            (rank(a), dup(a), &a.info.dir).cmp(&(rank(b), dup(b), &b.info.dir))
        });
        let mut p = 0;
        for e in entries.iter_mut() {
            if e.enabled && !e.reason.starts_with("duplicate") {
                p += 1;
                e.pos = p;
            }
        }
        issues.extend(plan.issues.iter().cloned());
        let (profiles, profile) = profiles(root);
        View {
            root: root.to_path_buf(),
            entries,
            conflicts: plan.conflicts.clone(),
            issues,
            active: plan.mods.iter().map(|m| m.manifest.id.clone()).collect(),
            has_enabled_file: has_file,
            profiles,
            profile,
        }
    }

    pub fn count(&self, state: State) -> usize {
        self.entries.iter().filter(|e| e.state == state).count()
    }

    /// The plan a fresh boot would apply differs from `applied` (the boot plan's active ids, in load order).
    pub fn dirty(&self, applied: &[String]) -> bool {
        self.active != applied
    }

    /// Changes waiting for the next start: mods switched in or out, or 1 when only the order changed.
    pub fn changes(&self, applied: &[String]) -> usize {
        let n = (0..self.entries.len()).filter(|&i| self.pending(i, applied)).count();
        if n == 0 && self.dirty(applied) { 1 } else { n }
    }

    /// Row `i` is in the plan the loader applied at boot (a second folder of an id never counts as applied).
    pub fn applied(&self, i: usize, applied: &[String]) -> bool {
        self.entries.get(i).is_some_and(|e| !e.reason.starts_with("duplicate") && applied.iter().any(|a| a == e.id()))
    }

    /// Row `i` will change at the next start (it is active now xor it was applied at boot).
    pub fn pending(&self, i: usize, applied: &[String]) -> bool {
        self.entries.get(i).is_some_and(|e| (e.state == State::Active) != self.applied(i, applied))
    }

    fn id_of(&self, i: usize) -> Result<String, String> {
        self.entries.get(i).map(|e| e.id().to_string()).ok_or_else(|| format!("row {i} out of range"))
    }

    /// Switch row `i` on / off (enabled.toml + load_order.toml, `evt_modfmt::apply_toggle`) and rescan.
    pub fn set_enabled(&self, i: usize, on: bool) -> Result<View, String> {
        apply_toggle(&self.root, &self.id_of(i)?, on)?;
        Ok(View::scan(&self.root))
    }

    /// Move row `i` one step in the load order (`delta` -1 = up = higher priority). Ok((view, moved)).
    pub fn move_by(&self, i: usize, delta: i32) -> Result<(View, bool), String> {
        let moved = apply_move(&self.root, &self.id_of(i)?, delta)?;
        Ok((View::scan(&self.root), moved))
    }

    /// Switch to profile `k`.
    pub fn switch_profile(&self, k: usize) -> Result<View, String> {
        let name = self.profiles.get(k).ok_or_else(|| format!("profile {k} out of range"))?;
        switch_profile(&self.root, name)?;
        Ok(View::scan(&self.root))
    }

    /// New profile «prefix N» with the current lists (becomes the active one).
    pub fn new_profile(&self, prefix: &str) -> Result<View, String> {
        new_profile(&self.root, prefix)?;
        Ok(View::scan(&self.root))
    }

    /// Content conflicts involving `id`, one line each (`file data/x: a, b (winner: b)`).
    pub fn conflicts_of(&self, id: &str) -> Vec<String> {
        self.conflicts.iter().filter(|c| c.mods.iter().any(|m| m == id)).map(|c| c.to_string()).collect()
    }

    /// The text of `CMND_EVT_MODS_GET_TEXT(i, what)`.
    pub fn text(&self, i: usize, what: i64) -> String {
        let Some(e) = self.entries.get(i) else { return String::new() };
        let m = &e.info.manifest;
        match what {
            0 => m.description.clone(),
            1 => state_detail(e),
            2 => m.requires.join(", "),
            3 => m.conflicts.join(", "),
            4 => m.loader_modules.join(", "),
            5 => self.conflicts_of(&m.id).join("\n"),
            6 => e.info.dir.display().to_string(),
            7 => e.warnings.iter().map(ModWarning::line).collect::<Vec<_>>().join("\n"),
            8 => m.tags.iter().map(|t| t.trim()).filter(|t| !t.is_empty()).collect::<Vec<_>>().join("\n"),
            9 => e.info.updated(),
            _ => String::new(),
        }
    }
}

/// `state` line of the detail window: reason of a skip.
pub fn state_detail(e: &Entry) -> String {
    match e.state {
        State::Skipped => e.reason.clone(),
        State::Disabled => String::from("not in enabled.toml"),
        State::Active => String::new(),
    }
}

/// Write `<root>/enabled.toml` atomically (kept for callers of the v2 API).
pub fn write_enabled(root: &Path, ids: &[String]) -> Result<(), String> {
    evt_modfmt::write_atomic(&root.join(ENABLED_FILE), &evt_modfmt::enabled_text(ids))
}

#[cfg(all(windows, target_arch = "x86_64"))]
pub use rt::init;

#[cfg(all(windows, target_arch = "x86_64"))]
mod rt {
    use super::{State, View};
    use crate::lua::{self, Call};
    use crate::{info, warn};
    use std::path::Path;
    use std::sync::Mutex;

    struct Rt {
        view: View,
        /// Active ids of the plan applied at boot (load order).
        applied: Vec<String>,
    }
    static RT: Mutex<Option<Rt>> = Mutex::new(None);

    fn with<R>(f: impl FnOnce(&mut Rt) -> R) -> Option<R> {
        let mut g = RT.lock().unwrap_or_else(|e| e.into_inner());
        g.as_mut().map(f)
    }

    /// Init thread (module `mods` on, Lua bridge on): scan the folder and register the commands (before
    /// `lua::activate`). `applied` = ids of the plan the loader applied in DllMain, in load order.
    pub fn init(root: &Path, applied: Vec<String>) {
        let view = View::scan(root);
        info!(
            "mods: menu commands: {} installed ({} active, {} skipped, {} disabled; profile {}) in {}",
            view.entries.len(),
            view.count(State::Active),
            view.count(State::Skipped),
            view.count(State::Disabled),
            view.profiles.get(view.profile).map(String::as_str).unwrap_or("?"),
            root.display()
        );
        *RT.lock().unwrap_or_else(|e| e.into_inner()) = Some(Rt { view, applied });
        lua::register("CMND_EVT_MODS_COUNT", cmd_count);
        lua::register("CMND_EVT_MODS_GET", cmd_get);
        lua::register("CMND_EVT_MODS_GET_TEXT", cmd_get_text);
        lua::register("CMND_EVT_MODS_SET_ENABLED", cmd_set_enabled);
        lua::register("CMND_EVT_MODS_SET_ACTIVE", cmd_set_enabled);
        lua::register("CMND_EVT_MODS_MOVE", cmd_move);
        lua::register("CMND_EVT_MODS_PROFILE_NAME", cmd_profile_name);
        lua::register("CMND_EVT_MODS_PROFILE_SWITCH", cmd_profile_switch);
        lua::register("CMND_EVT_MODS_PROFILE_NEW", cmd_profile_new);
        lua::register("CMND_EVT_MODS_RESCAN", cmd_rescan);
        lua::register("CMND_EVT_MODS_ROOT", cmd_root);
    }

    /// `CMND_EVT_MODS_COUNT()` → `installed, active, skipped, conflicts, dirty, changes, profiles, profile`.
    fn cmd_count(c: &mut Call) {
        let r = with(|rt| {
            let v = &rt.view;
            (
                v.entries.len(),
                v.count(State::Active),
                v.count(State::Skipped),
                v.conflicts.len(),
                v.dirty(&rt.applied),
                v.changes(&rt.applied),
                v.profiles.len(),
                v.profile,
            )
        });
        let (installed, active, skipped, conflicts, dirty, changes, np, p) = r.unwrap_or((0, 0, 0, 0, false, 0, 1, 0));
        c.push_int(installed as i64);
        c.push_int(active as i64);
        c.push_int(skipped as i64);
        c.push_int(conflicts as i64);
        c.push_bool(dirty);
        c.push_int(changes as i64);
        c.push_int(np as i64);
        c.push_int(p as i64);
    }

    /// `CMND_EVT_MODS_GET(i)` → `ok, id, name, version, author, priority, enabled, state, lua, files, deltas, applied,
    /// pending, pos, size, preview, warnings, level`.
    fn cmd_get(c: &mut Call) {
        let i = c.int(0).unwrap_or(-1);
        let row = if i < 0 {
            None
        } else {
            with(|rt| {
                let k = i as usize;
                rt.view.entries.get(k).cloned().map(|e| (e, rt.view.applied(k, &rt.applied), rt.view.pending(k, &rt.applied)))
            })
            .flatten()
        };
        let Some((e, applied, pending)) = row else {
            c.push_bool(false);
            return;
        };
        let m = &e.info.manifest;
        c.push_bool(true);
        c.push_str(&m.id);
        c.push_str(if m.name.trim().is_empty() { &m.id } else { &m.name });
        c.push_str(&m.version);
        c.push_str(&m.author);
        c.push_int(m.priority as i64);
        c.push_bool(e.enabled);
        c.push_int(e.state as i64);
        c.push_int(e.info.lua_scripts.len() as i64);
        c.push_int(e.info.files.len() as i64);
        c.push_int(e.info.deltas.iter().map(|d| d.delta.set.len() + d.delta.add.len()).sum::<usize>() as i64);
        c.push_bool(applied);
        c.push_bool(pending);
        c.push_int(e.pos as i64);
        c.push_num(e.info.size as f64);
        // served only when the loader mapped it at boot (crate::mods::Overlay); a preview built later shows next start
        c.push_bool(e.info.preview.is_some() && crate::mods::preview_served(&m.id));
        c.push_int(e.warnings.len() as i64);
        c.push_int(e.level());
    }

    /// `CMND_EVT_MODS_GET_TEXT(i, what)` → `text`.
    fn cmd_get_text(c: &mut Call) {
        let i = c.int(0).unwrap_or(-1);
        let what = c.int(1).unwrap_or(0);
        let t = if i < 0 { None } else { with(|rt| rt.view.text(i as usize, what)) };
        c.push_str(&t.unwrap_or_default());
    }

    /// `CMND_EVT_MODS_SET_ENABLED(i, on)` → `ok, enabled`.
    fn cmd_set_enabled(c: &mut Call) {
        let i = c.int(0).unwrap_or(-1);
        let on = c.int(1).unwrap_or(0) != 0;
        if i < 0 {
            c.push_bool(false);
            c.push_bool(false);
            return;
        }
        let r = with(|rt| {
            let id = rt.view.entries.get(i as usize).map(|e| e.id().to_string()).unwrap_or_default();
            match rt.view.set_enabled(i as usize, on) {
                Ok(v) => {
                    let en = v.entries.iter().find(|e| e.id() == id).is_some_and(|e| e.enabled);
                    info!("mods: menu: {id} {} (enabled.toml + load_order.toml rewritten; dirty={})", if on { "ON" } else { "OFF" }, v.dirty(&rt.applied));
                    rt.view = v;
                    (true, en)
                }
                Err(e) => {
                    warn!("mods: menu: cannot switch row {i}: {e}");
                    let en = rt.view.entries.get(i as usize).is_some_and(|e| e.enabled);
                    (false, en)
                }
            }
        });
        let (ok, en) = r.unwrap_or((false, false));
        c.push_bool(ok);
        c.push_bool(en);
    }

    /// `CMND_EVT_MODS_MOVE(i, delta)` → `ok, moved`.
    fn cmd_move(c: &mut Call) {
        let i = c.int(0).unwrap_or(-1);
        let d = c.int(1).unwrap_or(0) as i32;
        let r = if i < 0 {
            None
        } else {
            with(|rt| match rt.view.move_by(i as usize, d) {
                Ok((v, moved)) => {
                    if moved {
                        info!("mods: menu: row {i} moved {d:+} (load_order.toml rewritten; dirty={})", v.dirty(&rt.applied));
                    }
                    rt.view = v;
                    (true, moved)
                }
                Err(e) => {
                    warn!("mods: menu: cannot move row {i}: {e}");
                    (false, false)
                }
            })
        };
        let (ok, moved) = r.unwrap_or((false, false));
        c.push_bool(ok);
        c.push_bool(moved);
    }

    /// `CMND_EVT_MODS_PROFILE_NAME(k)` → `name`.
    fn cmd_profile_name(c: &mut Call) {
        let k = c.int(0).unwrap_or(-1);
        let n = if k < 0 { None } else { with(|rt| rt.view.profiles.get(k as usize).cloned()).flatten() };
        c.push_str(&n.unwrap_or_default());
    }

    /// `CMND_EVT_MODS_PROFILE_SWITCH(k)` → `ok`.
    fn cmd_profile_switch(c: &mut Call) {
        let k = c.int(0).unwrap_or(-1);
        let r = if k < 0 {
            None
        } else {
            with(|rt| match rt.view.switch_profile(k as usize) {
                Ok(v) => {
                    info!("mods: menu: profile {:?} (dirty={})", v.profiles.get(v.profile), v.dirty(&rt.applied));
                    rt.view = v;
                    true
                }
                Err(e) => {
                    warn!("mods: menu: cannot switch to profile {k}: {e}");
                    false
                }
            })
        };
        c.push_bool(r.unwrap_or(false));
    }

    /// `CMND_EVT_MODS_PROFILE_NEW(prefix)` → `ok, k`.
    fn cmd_profile_new(c: &mut Call) {
        let prefix = c.string(0).filter(|s| !s.trim().is_empty()).unwrap_or_else(|| "Perfil".to_string());
        let r = with(|rt| match rt.view.new_profile(&prefix) {
            Ok(v) => {
                info!("mods: menu: new profile {:?}", v.profiles.get(v.profile));
                let k = v.profile;
                rt.view = v;
                (true, k as i64)
            }
            Err(e) => {
                warn!("mods: menu: cannot create a profile: {e}");
                (false, -1)
            }
        });
        let (ok, k) = r.unwrap_or((false, -1));
        c.push_bool(ok);
        c.push_int(k);
    }

    /// `CMND_EVT_MODS_RESCAN()` → `ok, dirty`.
    fn cmd_rescan(c: &mut Call) {
        let r = with(|rt| {
            rt.view = View::scan(&rt.view.root);
            rt.view.dirty(&rt.applied)
        });
        c.push_bool(r.is_some());
        c.push_bool(r.unwrap_or(false));
    }

    /// `CMND_EVT_MODS_ROOT()` → `path, hasEnabledFile`.
    fn cmd_root(c: &mut Call) {
        let r = with(|rt| (rt.view.root.display().to_string(), rt.view.has_enabled_file));
        let (p, has) = r.unwrap_or_default();
        c.push_str(&p);
        c.push_bool(has);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use evt_modfmt::{read_enabled, read_order};

    fn tmp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("evt_mods_cmds_{name}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }
    fn mk(root: &Path, id: &str, extra: &str) {
        let d = root.join(id);
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(d.join("mod.toml"), format!("id = \"{id}\"\nname = \"Mod {id}\"\nversion = \"1.0\"\n{extra}")).unwrap();
    }
    fn ids(v: &View) -> Vec<&str> {
        v.entries.iter().map(Entry::id).collect()
    }

    #[test]
    fn scan_orders_and_states() {
        let root = tmp("scan");
        mk(&root, "base", "priority = -1\ndescription = \"the base\"\ntags = [\"Datos\", \"Jugabilidad\"]\nupdated = \"2026-09-27\"\n");
        mk(&root, "a", "requires = [\"base\"]\n");
        mk(&root, "b", "priority = 5\n");
        mk(&root, "c", "conflicts = [\"b\"]\n");
        mk(&root, "d", "requires = [\"missing\"]\n");
        let v = View::scan(&root);
        // display order = highest priority first (reverse of the load order: priority, then id)
        assert_eq!(ids(&v), vec!["b", "d", "c", "a", "base"]);
        assert!(!v.has_enabled_file);
        assert_eq!(v.active, vec!["base", "a", "b"]);
        assert_eq!(v.count(State::Active), 3);
        assert_eq!(v.count(State::Skipped), 2);
        assert_eq!(v.entries[2].state, State::Skipped);
        assert!(v.entries[2].reason.contains("conflicts with"));
        assert!(v.entries[1].reason.contains("requires `missing`"));
        assert_eq!(v.entries.iter().map(|e| e.pos).collect::<Vec<_>>(), vec![1, 2, 3, 4, 5]);
        assert_eq!(v.entries[1].level(), 2); // on, not loaded
        assert_eq!(v.entries[0].level(), 1); // b: c declares it incompatible
        assert_eq!(v.text(4, 0), "the base");
        assert_eq!(v.text(4, 8), "Datos\nJugabilidad");
        assert_eq!(v.text(4, 9), "2026-09-27");
        assert_eq!(v.text(3, 2), "base");
        assert!(v.text(1, 7).starts_with("missing|missing|"));
        assert!(v.text(4, 6).ends_with("base"));
        assert_eq!(v.text(9, 0), "");
        assert_eq!((v.profiles.clone(), v.profile), (vec!["Principal".to_string()], 0));
        assert!(!v.dirty(&["base".into(), "a".into(), "b".into()]));
        assert!(v.dirty(&["base".into(), "b".into()]));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn toggle_move_and_pending() {
        let root = tmp("toggle");
        mk(&root, "base", "");
        mk(&root, "a", "requires = [\"base\"]\n");
        mk(&root, "b", "");
        let v = View::scan(&root);
        assert_eq!(ids(&v), vec!["base", "b", "a"]);
        let applied = v.active.clone(); // a b base
        // off: enabled.toml without it, order file written, the row goes below the active ones
        let k = v.entries.iter().position(|e| e.id() == "b").unwrap();
        let v = v.set_enabled(k, false).unwrap();
        assert_eq!(read_enabled(&root).unwrap().unwrap(), vec!["a", "base"]);
        assert_eq!(ids(&v), vec!["base", "a", "b"]);
        assert_eq!(v.entries[2].state, State::Disabled);
        assert_eq!(v.entries[2].pos, 0);
        assert!(v.pending(2, &applied) && v.applied(2, &applied));
        assert_eq!(v.changes(&applied), 1);
        // on again: top of the order
        let v = v.set_enabled(2, true).unwrap();
        assert_eq!(ids(&v), vec!["b", "base", "a"]);
        assert_eq!(read_order(&root).unwrap().unwrap(), vec!["b", "base", "a"]);
        assert!(!v.pending(0, &applied));
        // only the order changed: one change, no pending row
        assert!(v.dirty(&applied) && v.changes(&applied) == 1);
        // move: a is required by nobody, base up/down
        let (v, moved) = v.move_by(2, -1).unwrap();
        assert!(moved);
        assert_eq!(ids(&v), vec!["b", "a", "base"]);
        let (v, moved) = v.move_by(0, -1).unwrap();
        assert!(!moved);
        // disabling base skips a (requires): a stays enabled, state Skipped, red
        let k = v.entries.iter().position(|e| e.id() == "base").unwrap();
        let v = v.set_enabled(k, false).unwrap();
        let a = v.entries.iter().find(|e| e.id() == "a").unwrap();
        assert_eq!((a.state, a.enabled, a.level()), (State::Skipped, true, 2));
        assert!(v.set_enabled(99, true).is_err());
        assert!(!root.join("enabled.toml.tmp").exists());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn profiles_from_the_menu() {
        let root = tmp("prof");
        mk(&root, "a", "");
        mk(&root, "b", "");
        let v = View::scan(&root);
        let v = v.new_profile("Perfil").unwrap();
        assert_eq!((v.profiles.clone(), v.profile), (vec!["Principal".to_string(), "Perfil 2".to_string()], 1));
        let k = v.entries.iter().position(|e| e.id() == "a").unwrap();
        let v = v.set_enabled(k, false).unwrap();
        assert_eq!(v.active, vec!["b"]);
        let v = v.switch_profile(0).unwrap();
        assert_eq!(v.active, vec!["a", "b"]);
        let v = v.switch_profile(1).unwrap();
        assert_eq!(v.active, vec!["b"]);
        assert!(v.switch_profile(7).is_err());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn duplicate_folders() {
        let root = tmp("dup");
        mk(&root, "x", "");
        mk(&root, "x_copy", "");
        std::fs::write(root.join("x_copy/mod.toml"), "id = \"x\"\nname = \"X\"\nversion = \"2\"\n").unwrap();
        let v = View::scan(&root);
        assert_eq!(v.entries.len(), 2);
        assert_eq!(v.entries[0].state, State::Active);
        assert_eq!(v.entries[1].state, State::Skipped);
        assert!(v.entries[1].reason.contains("duplicate"));
        assert_eq!(v.entries[1].level(), 2);
        assert_eq!(v.active, vec!["x"]);
        let applied = vec!["x".to_string()];
        assert!(v.applied(0, &applied) && !v.applied(1, &applied) && !v.pending(1, &applied));
        let _ = std::fs::remove_dir_all(&root);
    }
}
