//! The mod list of `<game>\mods` as the manager edits it: enabled switches and priority (drag order) in memory,
//! «Save» writes `enabled.toml` + `load_order.toml` with the `evt_modfmt` writers (the files the ModLoader reads at
//! start and its in-game Mods menu edits). Status per mod and the conflict list come from
//! `evt_modfmt::build_plan_full` / `mod_warnings`, the same plan the loader builds.

use std::path::{Path, PathBuf};

use evt_installer::modpack::{satisfies, Avail};
use evt_modfmt::{self as fmt, ConflictKind, Issue, LoadPlan, ModInfo, Requirement, Severity, WarnKind};

use crate::i18n::{tr, trf};

#[derive(Debug, Clone)]
pub struct Row {
    pub id: String,
    /// First folder with this id (a second one is ignored by the loader too).
    pub info: ModInfo,
    pub enabled: bool,
}

#[derive(Debug, Clone)]
pub struct ModList {
    pub root: PathBuf,
    /// Every mod folder scanned (duplicates included: the plan reports them).
    pub all: Vec<ModInfo>,
    /// Display order: top = highest priority (loads last, wins conflicts).
    pub rows: Vec<Row>,
    /// Ids of `enabled.toml` that are not installed (kept on save, as the in-game menu does).
    pub extra_enabled: Vec<String>,
    /// Scan problems and unreadable list files.
    pub issues: Vec<Issue>,
    /// In-memory state differs from the files.
    pub dirty: bool,
}

impl ModList {
    /// Read `root` (= `<game>\mods`; a missing folder is an empty list).
    pub fn load(root: &Path) -> ModList {
        let (all, mut issues) = if root.is_dir() { fmt::scan_root(root) } else { (Vec::new(), Vec::new()) };
        let enabled = match fmt::read_enabled(root) {
            Ok(e) => e,
            Err(e) => {
                issues.push(Issue::err(None, Some(&root.join(fmt::ENABLED_FILE)), format!("{e}: every mod counts as enabled")));
                None
            }
        };
        let order = match fmt::read_order(root) {
            Ok(o) => o,
            Err(e) => {
                issues.push(Issue::err(None, Some(&root.join(fmt::ORDER_FILE)), format!("{e}: order by priority")));
                None
            }
        };
        let rows: Vec<Row> = fmt::display_order(&all, order.as_deref())
            .into_iter()
            .filter_map(|id| {
                let info = all.iter().find(|m| m.id() == id)?.clone();
                let on = enabled.as_ref().is_none_or(|e| e.contains(&id));
                Some(Row { id, info, enabled: on })
            })
            .collect();
        let extra_enabled = enabled.unwrap_or_default().into_iter().filter(|e| !rows.iter().any(|r| &r.id == e)).collect();
        ModList { root: root.to_path_buf(), all, rows, extra_enabled, issues, dirty: false }
    }

    pub fn index_of(&self, id: &str) -> Option<usize> {
        self.rows.iter().position(|r| r.id == id)
    }

    /// `enabled.toml` list: the enabled rows (display order), then the kept unknown ids.
    pub fn enabled_list(&self) -> Vec<String> {
        self.rows.iter().filter(|r| r.enabled).map(|r| r.id.clone()).chain(self.extra_enabled.iter().cloned()).collect()
    }

    /// `load_order.toml` list: every installed id, top first.
    pub fn order_list(&self) -> Vec<String> {
        self.rows.iter().map(|r| r.id.clone()).collect()
    }

    pub fn set_enabled(&mut self, i: usize, on: bool) {
        if let Some(r) = self.rows.get_mut(i) {
            if r.enabled != on {
                r.enabled = on;
                self.dirty = true;
            }
        }
    }

    pub fn set_all(&mut self, on: bool) {
        for i in 0..self.rows.len() {
            self.set_enabled(i, on);
        }
    }

    /// Move row `from` so it lands before the row that is at index `to` now (`to == len` = to the bottom).
    pub fn move_row(&mut self, from: usize, to: usize) {
        if from >= self.rows.len() || to > self.rows.len() || to == from || to == from + 1 {
            return;
        }
        let r = self.rows.remove(from);
        let at = if to > from { to - 1 } else { to };
        self.rows.insert(at, r);
        self.dirty = true;
    }

    /// Forget the row of `id` (after an uninstall), its enabled entry too.
    pub fn remove(&mut self, id: &str) {
        self.rows.retain(|r| r.id != id);
        self.all.retain(|m| m.id() != id);
        self.extra_enabled.retain(|e| e != id);
    }

    /// The load plan the ModLoader would build from the in-memory lists. `loader` = installed ModLoader version
    /// (`loader_min` checked), None = unknown / not installed (not checked).
    pub fn plan(&self, loader: Option<&str>) -> LoadPlan {
        let (e, o) = (self.enabled_list(), self.order_list());
        let mut plan = fmt::build_plan_full(self.all.clone(), Some(&e), Some(&o), loader);
        let mut issues = self.issues.clone();
        issues.append(&mut plan.issues);
        plan.issues = issues;
        plan
    }

    /// Write `enabled.toml` and `load_order.toml` (atomic; the mods folder is created when missing).
    pub fn save(&mut self) -> Result<(), String> {
        std::fs::create_dir_all(&self.root).map_err(|e| format!("{}: {e}", self.root.display()))?;
        fmt::write_atomic(&self.root.join(fmt::ENABLED_FILE), &fmt::enabled_text(&self.enabled_list()))?;
        fmt::write_atomic(&self.root.join(fmt::ORDER_FILE), &fmt::order_text(&self.order_list()))?;
        self.dirty = false;
        Ok(())
    }

    /// What can satisfy a `requires`: every installed folder (enabled or not).
    pub fn avail(&self) -> Vec<Avail> {
        self.all.iter().map(|m| Avail { id: m.manifest.id.clone(), version: m.manifest.version.clone(), provides: m.manifest.provides.clone() }).collect()
    }

    /// Requirements of the enabled mods that no installed mod satisfies: `(id, constraint, needed by)`.
    pub fn missing_dependencies(&self) -> Vec<(String, String, Vec<String>)> {
        let avail = self.avail();
        let mut out: Vec<(String, String, Vec<String>)> = Vec::new();
        for r in self.rows.iter().filter(|r| r.enabled) {
            for req in r.info.requirements() {
                if avail.iter().any(|a| satisfies(a, &req)) {
                    continue;
                }
                match out.iter_mut().find(|(id, _, _)| *id == req.id) {
                    Some(e) => e.2.push(r.id.clone()),
                    None => out.push((req.id.clone(), req.constraint(), vec![r.id.clone()])),
                }
            }
        }
        out
    }

    /// Enabled mods that would stop loading if `id` were gone (they require it and nothing else provides it).
    pub fn dependents(&self, id: &str, loader: Option<&str>) -> Vec<String> {
        let before = self.plan(loader);
        let mut without = self.clone();
        without.remove(id);
        let after = without.plan(loader);
        before.mods.iter().map(|m| m.manifest.id.clone()).filter(|m| m != id && after.skipped.iter().any(|s| &s.id == m)).collect()
    }
}

// ---------------------------------------------------------------- status per mod

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Level {
    Ok,
    Off,
    Info,
    Warn,
    Error,
}

#[derive(Debug, Clone)]
pub struct Status {
    pub level: Level,
    pub lines: Vec<(Level, String)>,
    /// Requirements not installed at all: `(id, constraint)` (offered by «Install missing…»).
    pub missing: Vec<(String, String)>,
}

/// Status of every row (same index), in the current UI language.
pub fn statuses(list: &ModList, plan: &LoadPlan, loader: Option<&str>) -> Vec<Status> {
    let enabled = list.enabled_list();
    list.rows
        .iter()
        .map(|r| {
            let mut lines: Vec<(Level, String)> = Vec::new();
            let mut missing = Vec::new();
            let hard = if r.enabled { Level::Error } else { Level::Warn };
            for w in fmt::mod_warnings(&r.info, &list.all, &enabled, plan) {
                let line = match w.kind {
                    WarnKind::Missing => {
                        missing.push((w.other.clone(), w.detail.clone()));
                        let what = if w.detail.is_empty() { w.other.clone() } else { format!("{} {}", w.other, w.detail) };
                        (hard, trf("requires «{}», which is not installed", &[&what]))
                    }
                    WarnKind::Disabled => (hard, trf("requires «{}», which is disabled", &[&w.other])),
                    WarnKind::Version => {
                        let (want, have) = w.detail.split_once('|').unwrap_or((&w.detail, ""));
                        (hard, trf("requires «{}» {} (installed: {})", &[&w.other, &want, &have]))
                    }
                    WarnKind::Incompatible => (hard, trf("incompatible with «{}» (enabled)", &[&w.other])),
                    WarnKind::Shared if w.wins => (Level::Info, trf("shares {} file(s)/cell(s) with «{}» — this mod wins", &[&w.count, &w.other])),
                    WarnKind::Shared => (Level::Info, trf("shares {} file(s)/cell(s) with «{}» — «{}» wins", &[&w.count, &w.other, &w.other])),
                    WarnKind::Duplicate => (Level::Warn, trf("second folder with the id «{}»: ignored", &[&w.other])),
                };
                lines.push(line);
            }
            // the loader's own reason, unless a line above already explains it
            if let Some(s) = plan.skipped.iter().find(|s| s.id == r.id && r.enabled && s.reason != "not in enabled.toml") {
                if !lines.iter().any(|(l, _)| *l == Level::Error) {
                    lines.insert(0, (Level::Error, trf("Not loaded: {}", &[&s.reason])));
                }
            }
            let min = r.info.manifest.loader_min.trim();
            if let Some(lv) = loader {
                if !min.is_empty() && fmt::compare_versions(lv, min).is_lt() && !lines.iter().any(|(_, l)| l.contains(min)) {
                    lines.push((hard, trf("needs ModLoader {} (installed: {})", &[&min, &lv])));
                }
            }
            let worst = lines.iter().map(|(l, _)| *l).max().unwrap_or(Level::Ok);
            let level = if !r.enabled && worst <= Level::Info { Level::Off } else { worst };
            Status { level, lines, missing }
        })
        .collect()
}

pub fn level_label(l: Level) -> &'static str {
    match l {
        Level::Ok | Level::Info => tr("Active"),
        Level::Off => tr("Off"),
        Level::Warn => tr("Warning"),
        Level::Error => tr("Won't load"),
    }
}

// ---------------------------------------------------------------- conflicts

#[derive(Debug, Clone, PartialEq)]
pub struct ConflictRow {
    /// `file` / `cell` / `new row` / an audio kind.
    pub kind: String,
    pub what: String,
    /// Load order; the last one wins.
    pub mods: Vec<String>,
}

impl ConflictRow {
    pub fn winner(&self) -> &str {
        self.mods.last().map(String::as_str).unwrap_or("")
    }
}

/// File / cell / row conflicts of the plan (what the loader logs as `conflict`).
pub fn conflict_rows(plan: &LoadPlan) -> Vec<ConflictRow> {
    plan.conflicts
        .iter()
        .map(|c| {
            let (kind, what) = match &c.kind {
                ConflictKind::File { key } => (tr("file"), key.clone()),
                ConflictKind::Cell { table, key, column } => (tr("cell"), format!("{table}[{key}].{column}")),
                ConflictKind::Row { table, key } => (tr("new row"), format!("{table}[{key}]")),
            };
            ConflictRow { kind: kind.to_string(), what, mods: c.mods.clone() }
        })
        .collect()
}

/// Errors and warnings of the plan (scan problems, duplicates, cycles, unknown enabled ids), as text lines.
pub fn problem_lines(plan: &LoadPlan) -> Vec<(Level, String)> {
    let mut out: Vec<(Level, String)> = plan
        .issues
        .iter()
        .map(|i| (if i.severity == Severity::Error { Level::Error } else { Level::Warn }, i.to_string()))
        .collect();
    for s in plan.skipped.iter().filter(|s| s.reason.starts_with("duplicate id")) {
        out.push((Level::Warn, format!("{}: {}", s.id, s.reason)));
    }
    out
}

/// A requirement text (`id` / `id>=1.2`) parsed, for callers outside the plan.
pub fn requirement(s: &str) -> Option<Requirement> {
    fmt::parse_requirement(s).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mk(root: &Path, id: &str, extra: &str) {
        let d = root.join(id);
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(d.join("mod.toml"), format!("id = \"{id}\"\nname = \"{id}\"\nversion = \"1.0\"\n{extra}")).unwrap();
    }
    fn file(root: &Path, rel: &str) {
        let p = root.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, rel).unwrap();
    }
    fn fresh(name: &str) -> PathBuf {
        let d = crate::temp_dir(name).unwrap();
        let m = d.join("mods");
        std::fs::create_dir_all(&m).unwrap();
        m
    }

    #[test]
    fn load_edit_save_round_trip() {
        let root = fresh("model_rt");
        for id in ["a", "b", "c"] {
            mk(&root, id, "");
        }
        std::fs::write(root.join("enabled.toml"), "enabled = [\"a\", \"c\", \"ghost\"]\n").unwrap();
        std::fs::write(root.join("load_order.toml"), "order = [\"c\", \"a\", \"b\"]\n").unwrap();
        let mut l = ModList::load(&root);
        assert_eq!(l.order_list(), vec!["c", "a", "b"]);
        assert_eq!(l.rows.iter().map(|r| r.enabled).collect::<Vec<_>>(), vec![true, true, false]);
        assert_eq!(l.extra_enabled, vec!["ghost"]);
        // plan = what the loader would do: lowest priority first
        assert_eq!(l.plan(None).summary(), "a@1.0 c@1.0");
        // drag b to the top, enable it, save
        l.move_row(2, 0);
        l.set_enabled(0, true);
        assert!(l.dirty);
        l.save().unwrap();
        assert!(!l.dirty);
        assert_eq!(fmt::read_order(&root).unwrap().unwrap(), vec!["b", "c", "a"]);
        assert_eq!(fmt::read_enabled(&root).unwrap().unwrap(), vec!["b", "c", "a", "ghost"]);
        // the loader's own reader agrees with the manager
        assert_eq!(fmt::plan_root(&root).summary(), "a@1.0 c@1.0 b@1.0");
        let again = ModList::load(&root);
        assert_eq!(again.order_list(), vec!["b", "c", "a"]);
        assert!(again.rows.iter().all(|r| r.enabled));
        let _ = std::fs::remove_dir_all(root.parent().unwrap());
    }

    #[test]
    fn no_files_means_all_enabled_by_priority() {
        let root = fresh("model_nofiles");
        mk(&root, "x", "priority = 5\n");
        mk(&root, "y", "");
        let l = ModList::load(&root);
        // y (prio 0) loads first, so x is on top
        assert_eq!(l.order_list(), vec!["x", "y"]);
        assert!(l.rows.iter().all(|r| r.enabled));
        let empty = ModList::load(&root.join("nope"));
        assert!(empty.rows.is_empty());
        let _ = std::fs::remove_dir_all(root.parent().unwrap());
    }

    #[test]
    fn move_row_semantics() {
        let root = fresh("model_move");
        for id in ["a", "b", "c", "d"] {
            mk(&root, id, "");
        }
        std::fs::write(root.join("load_order.toml"), "order = [\"a\", \"b\", \"c\", \"d\"]\n").unwrap();
        let mut l = ModList::load(&root);
        l.move_row(0, 4); // a to the bottom
        assert_eq!(l.order_list(), vec!["b", "c", "d", "a"]);
        l.move_row(3, 1); // a before c
        assert_eq!(l.order_list(), vec!["b", "a", "c", "d"]);
        l.dirty = false;
        l.move_row(1, 2); // no-op (same slot)
        assert!(!l.dirty);
        let _ = std::fs::remove_dir_all(root.parent().unwrap());
    }

    #[test]
    fn statuses_conflicts_and_dependencies() {
        let root = fresh("model_status");
        mk(&root, "lib", "");
        mk(&root, "needs_lib", "requires = [\"lib>=1.0\"]\n");
        mk(&root, "needs_new", "requires = [\"lib>=2.0\"]\n");
        mk(&root, "needs_ghost", "requires = [\"ghost\"]\n");
        mk(&root, "modern", "loader_min = \"9.0.0\"\n");
        mk(&root, "p", "");
        mk(&root, "q", "");
        file(&root, "p/files/data/common/x.bin");
        file(&root, "q/files/data/common/x.bin");
        std::fs::write(root.join("load_order.toml"), "order = [\"q\", \"p\"]\n").unwrap();
        let l = ModList::load(&root);
        let plan = l.plan(Some("1.0.0"));
        let st = statuses(&l, &plan, Some("1.0.0"));
        let get = |id: &str| &st[l.index_of(id).unwrap()];
        assert_eq!(get("lib").level, Level::Ok);
        assert_eq!(get("needs_lib").level, Level::Ok);
        assert_eq!(get("needs_new").level, Level::Error);
        assert_eq!(get("needs_ghost").level, Level::Error);
        assert_eq!(get("needs_ghost").missing, vec![("ghost".to_string(), String::new())]);
        assert_eq!(get("modern").level, Level::Error, "{:?}", get("modern").lines);
        assert_eq!(get("p").level, Level::Info);
        // conflicts: q is on top, so it loads last and wins
        let c = conflict_rows(&plan);
        assert_eq!(c.len(), 1);
        assert_eq!(c[0].what, "data/common/x.bin");
        assert_eq!(c[0].winner(), "q");
        // dependencies
        // not installed at all, or only in a version that is too old (an update is offered)
        let miss = l.missing_dependencies();
        let ids: Vec<&str> = miss.iter().map(|m| m.0.as_str()).collect();
        assert_eq!(ids, vec!["lib", "ghost"]);
        assert_eq!(miss[0].1, ">= 2.0");
        assert_eq!(l.dependents("lib", Some("1.0.0")), vec!["needs_lib"]);
        assert!(l.dependents("p", Some("1.0.0")).is_empty());
        // a disabled mod with a missing requirement is only a warning
        let mut l2 = l.clone();
        let i = l2.index_of("needs_ghost").unwrap();
        l2.set_enabled(i, false);
        let st2 = statuses(&l2, &l2.plan(None), None);
        assert_eq!(st2[i].level, Level::Warn);
        let _ = std::fs::remove_dir_all(root.parent().unwrap());
    }

    #[test]
    fn profiles_follow_the_manager_save() {
        // the manager saves, then switches profile with evt_modfmt: both see the same files
        let root = fresh("model_profiles");
        mk(&root, "a", "");
        mk(&root, "b", "");
        let mut l = ModList::load(&root);
        fmt::create_profile(&root, "Solo").unwrap();
        let i = l.index_of("b").unwrap();
        l.set_enabled(i, false);
        l.save().unwrap();
        fmt::switch_profile(&root, fmt::DEFAULT_PROFILE).unwrap();
        assert!(ModList::load(&root).rows.iter().all(|r| r.enabled));
        fmt::switch_profile(&root, "Solo").unwrap();
        let l = ModList::load(&root);
        assert_eq!(l.enabled_list(), vec!["a"]);
        let _ = std::fs::remove_dir_all(root.parent().unwrap());
    }
}
