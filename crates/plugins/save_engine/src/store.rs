//! The per-mod store (pure file logic, unit-tested on temp folders; no game access).
//!
//! ```text
//! <root>\                       storage root (lib.rs storage_root: profile LocalLow folder or <game>\evt_loader\saves)
//!     slot<N>\<mod id>.json     slot scope: follows retail save slot N (committed when the game saves N)
//!     global\<mod id>.json      global scope: not tied to a slot (written at once)
//!     trash\<stamp>_<why>_slot<N>\   data of a deleted / replaced slot (kept, newest `trash_keep` folders)
//! ```
//!
//! File: `{"format": 1, "mod": "<id>", "bound": true, "written_unix": 1759140000, "reason": "game save",
//! "data": {"key": value, ...}}`. `bound` = committed together with a retail save (false = immediate / forced write).
//! Every write is atomic: `<file>.tmp`, flush to disk, rename over the old file. A file that does not parse is kept as
//! `<file>.bad-<stamp>` and the mod starts empty (logged).

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

pub const FORMAT: u32 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scope {
    /// The active save slot.
    Slot,
    Global,
}

#[derive(Debug, Serialize, Deserialize)]
struct FileDoc {
    format: u32,
    #[serde(rename = "mod")]
    mod_id: String,
    #[serde(default)]
    bound: bool,
    #[serde(default)]
    written_unix: u64,
    #[serde(default)]
    reason: String,
    #[serde(default)]
    data: Map<String, Value>,
}

#[derive(Debug, Default, Clone)]
struct ModData {
    data: Map<String, Value>,
    /// Changed since the last commit.
    dirty: bool,
}

pub use vr_framework::fsx::{unix_now, write_atomic};

#[derive(Debug, Clone, PartialEq)]
pub enum SetError {
    Key,
    Value,
    TooManyKeys,
}

/// What a slot operation did (for the log).
#[derive(Debug, Default, Clone, PartialEq)]
pub struct Outcome {
    /// Mods written.
    pub written: Vec<String>,
    /// Mods whose uncommitted data was dropped (slot switch / delete without a save).
    pub dropped: Vec<String>,
    /// Folder the old data went to.
    pub trashed: Option<PathBuf>,
    /// Files copied.
    pub copied: usize,
    pub errors: Vec<String>,
}

pub struct Store {
    root: PathBuf,
    slot: u8,
    /// The active slot's data may be committed (false: locked slot / no slot: the game's writes go nowhere either).
    writable: bool,
    slot_mods: BTreeMap<String, ModData>,
    global_mods: BTreeMap<String, ModData>,
    trash_keep: usize,
    /// Log lines produced by lazy loads (drained by the caller).
    pub notes: Vec<String>,
    trash_seq: u32,
}

impl Store {
    pub fn new(root: PathBuf, trash_keep: usize) -> Store {
        Store {
            root,
            slot: 0,
            writable: false,
            slot_mods: BTreeMap::new(),
            global_mods: BTreeMap::new(),
            trash_keep: trash_keep.max(1),
            notes: Vec::new(),
            trash_seq: 0,
        }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }
    pub fn slot(&self) -> u8 {
        self.slot
    }
    pub fn writable(&self) -> bool {
        self.writable
    }
    pub fn slot_dir(&self, n: u8) -> PathBuf {
        self.root.join(format!("slot{n}"))
    }
    fn file(&self, scope: Scope, m: &str) -> PathBuf {
        match scope {
            Scope::Slot => self.slot_dir(self.slot),
            Scope::Global => self.root.join("global"),
        }
        .join(format!("{m}.json"))
    }

    /// Mods with changes not committed yet, per scope.
    pub fn dirty(&self, scope: Scope) -> Vec<String> {
        let map = match scope {
            Scope::Slot => &self.slot_mods,
            Scope::Global => &self.global_mods,
        };
        map.iter().filter(|(_, d)| d.dirty).map(|(k, _)| k.clone()).collect()
    }

    /// The game now runs on `slot` (`writable` = its saves reach the disk). A real change drops the old slot's
    /// uncommitted data (the game drops its unsaved progress too) and reloads lazily.
    pub fn set_slot(&mut self, slot: u8, writable: bool) -> Outcome {
        let mut o = Outcome::default();
        if slot != self.slot {
            o.dropped = self.dirty(Scope::Slot);
            self.slot_mods.clear();
            self.slot = slot;
        }
        self.writable = writable;
        o
    }

    fn load(&mut self, scope: Scope, m: &str) -> ModData {
        let path = self.file(scope, m);
        let text = match std::fs::read(&path) {
            Ok(t) => t,
            Err(_) => return ModData::default(),
        };
        match serde_json::from_slice::<FileDoc>(&text) {
            Ok(d) if d.format <= FORMAT => ModData { data: d.data, dirty: false },
            Ok(d) => {
                self.notes.push(format!("{}: format {} is newer than this save_engine ({FORMAT}): read as empty, file kept", path.display(), d.format));
                ModData::default()
            }
            Err(e) => {
                let bad = PathBuf::from(format!("{}.bad-{}", path.display(), unix_now()));
                let kept = std::fs::rename(&path, &bad).is_ok();
                self.notes.push(format!(
                    "{}: unreadable ({e}): {}; mod starts empty",
                    path.display(),
                    if kept { format!("kept as {}", bad.display()) } else { "could not rename it".into() }
                ));
                ModData::default()
            }
        }
    }

    fn entry(&mut self, scope: Scope, m: &str) -> &mut ModData {
        let present = match scope {
            Scope::Slot => self.slot_mods.contains_key(m),
            Scope::Global => self.global_mods.contains_key(m),
        };
        if !present {
            let d = self.load(scope, m);
            match scope {
                Scope::Slot => self.slot_mods.insert(m.to_string(), d),
                Scope::Global => self.global_mods.insert(m.to_string(), d),
            };
        }
        match scope {
            Scope::Slot => self.slot_mods.get_mut(m).unwrap(),
            Scope::Global => self.global_mods.get_mut(m).unwrap(),
        }
    }

    pub fn get(&mut self, scope: Scope, m: &str, key: &str) -> Option<Value> {
        self.entry(scope, m).data.get(key).cloned()
    }

    pub fn set(&mut self, scope: Scope, m: &str, key: &str, v: Value) -> Result<(), SetError> {
        if !crate::valid_key(key) {
            return Err(SetError::Key);
        }
        if matches!(&v, Value::String(s) if s.len() > crate::MAX_STR) {
            return Err(SetError::Value);
        }
        let e = self.entry(scope, m);
        if !e.data.contains_key(key) && e.data.len() >= crate::MAX_KEYS {
            return Err(SetError::TooManyKeys);
        }
        if e.data.get(key) != Some(&v) {
            e.data.insert(key.to_string(), v);
            e.dirty = true;
        }
        Ok(())
    }

    /// Returns whether the key existed.
    pub fn del(&mut self, scope: Scope, m: &str, key: &str) -> bool {
        let e = self.entry(scope, m);
        let had = e.data.remove(key).is_some();
        e.dirty |= had;
        had
    }

    fn write_mod(&mut self, scope: Scope, m: &str, bound: bool, reason: &str) -> Result<bool, String> {
        let path = self.file(scope, m);
        let map = match scope {
            Scope::Slot => &mut self.slot_mods,
            Scope::Global => &mut self.global_mods,
        };
        let Some(e) = map.get_mut(m) else { return Ok(false) };
        if !e.dirty {
            return Ok(false);
        }
        let r = if e.data.is_empty() {
            // nothing left to keep: no file (an old one would bring deleted keys back)
            match std::fs::remove_file(&path) {
                Err(err) if err.kind() != std::io::ErrorKind::NotFound => Err(err),
                _ => Ok(()),
            }
        } else {
            let doc = FileDoc { format: FORMAT, mod_id: m.to_string(), bound, written_unix: unix_now(), reason: reason.to_string(), data: e.data.clone() };
            write_atomic(&path, serde_json::to_string_pretty(&doc).unwrap().as_bytes())
        };
        match r {
            Ok(()) => {
                e.dirty = false;
                Ok(true)
            }
            Err(err) => Err(format!("{}: {err}", path.display())),
        }
    }

    /// Commit every changed mod of the active slot (the game saved it). Nothing when the slot is not writable.
    pub fn commit_slot(&mut self, reason: &str) -> Outcome {
        let mut o = Outcome::default();
        if !self.writable {
            return o;
        }
        for m in self.dirty(Scope::Slot) {
            match self.write_mod(Scope::Slot, &m, true, reason) {
                Ok(true) => o.written.push(m),
                Ok(false) => {}
                Err(e) => o.errors.push(e),
            }
        }
        o
    }

    /// Commit one mod now (immediate mode / `CMND_EVT_SAVE_COMMIT`). Slot scope needs a writable slot.
    pub fn commit_mod(&mut self, scope: Scope, m: &str, reason: &str) -> Result<bool, String> {
        if scope == Scope::Slot && !self.writable {
            return Err(format!("slot {} is not writable (locked / none): nothing committed", self.slot));
        }
        self.write_mod(scope, m, false, reason)
    }

    /// Worker flush: every changed global mod, and the slot mods `immediate` names.
    pub fn flush(&mut self, immediate: impl Fn(&str) -> bool) -> Outcome {
        let mut o = Outcome::default();
        for m in self.dirty(Scope::Global) {
            match self.write_mod(Scope::Global, &m, false, "global") {
                Ok(true) => o.written.push(format!("global/{m}")),
                Ok(false) => {}
                Err(e) => o.errors.push(e),
            }
        }
        if self.writable {
            for m in self.dirty(Scope::Slot).into_iter().filter(|m| immediate(m)) {
                match self.write_mod(Scope::Slot, &m, false, "immediate") {
                    Ok(true) => o.written.push(m),
                    Ok(false) => {}
                    Err(e) => o.errors.push(e),
                }
            }
        }
        o
    }

    fn move_to_trash(&mut self, n: u8, why: &str) -> Result<Option<PathBuf>, String> {
        let dir = self.slot_dir(n);
        let has_files = std::fs::read_dir(&dir).map(|mut r| r.next().is_some()).unwrap_or(false);
        if !has_files {
            let _ = std::fs::remove_dir(&dir);
            return Ok(None);
        }
        let trash = self.root.join("trash");
        std::fs::create_dir_all(&trash).map_err(|e| format!("{}: {e}", trash.display()))?;
        self.trash_seq += 1;
        let dst = trash.join(format!("{:011}-{:03}_{why}_slot{n}", unix_now(), self.trash_seq % 1000));
        std::fs::rename(&dir, &dst).map_err(|e| format!("{} -> {}: {e}", dir.display(), dst.display()))?;
        self.prune_trash();
        Ok(Some(dst))
    }

    fn prune_trash(&self) {
        let Ok(rd) = std::fs::read_dir(self.root.join("trash")) else { return };
        let mut v: Vec<PathBuf> = rd.flatten().map(|e| e.path()).filter(|p| p.is_dir()).collect();
        if v.len() <= self.trash_keep {
            return;
        }
        v.sort();
        for p in &v[..v.len() - self.trash_keep] {
            let _ = std::fs::remove_dir_all(p);
        }
    }

    /// Slot `n`'s save was removed (or a new game starts in it): its mod data goes to the trash. When `n` is the
    /// active slot its in-memory data (and uncommitted changes) is dropped.
    pub fn on_slot_gone(&mut self, n: u8, why: &str) -> Outcome {
        let mut o = Outcome::default();
        if n == self.slot {
            o.dropped = self.dirty(Scope::Slot);
            self.slot_mods.clear();
        }
        match self.move_to_trash(n, why) {
            Ok(t) => o.trashed = t,
            Err(e) => o.errors.push(e),
        }
        o
    }

    /// Slot `src` was duplicated into `dst`: `dst`'s old data goes to the trash, `src`'s **committed** files are
    /// copied (as the retail copy takes the file on disk, not the game in memory).
    pub fn on_slot_copied(&mut self, src: u8, dst: u8) -> Outcome {
        let mut o = self.on_slot_gone(dst, "replaced");
        let (from, to) = (self.slot_dir(src), self.slot_dir(dst));
        let Ok(rd) = std::fs::read_dir(&from) else { return o };
        for e in rd.flatten() {
            let name = e.file_name().to_string_lossy().into_owned();
            let is_data = name.ends_with(".json") && crate::valid_mod_id(name.trim_end_matches(".json"));
            if !is_data || !e.path().is_file() {
                continue;
            }
            match std::fs::read(e.path()).and_then(|b| write_atomic(&to.join(&name), &b)) {
                Ok(()) => o.copied += 1,
                Err(err) => o.errors.push(format!("copy {name} slot{src} -> slot{dst}: {err}")),
            }
        }
        o
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn tmp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("save-engine-store-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        d
    }

    fn doc(p: &Path) -> serde_json::Value {
        serde_json::from_slice(&std::fs::read(p).unwrap()).unwrap()
    }

    #[test]
    fn slot_data_is_committed_when_the_game_saves() {
        let d = tmp("commit");
        let mut s = Store::new(d.clone(), 5);
        s.set_slot(2, true);
        s.set(Scope::Slot, "m", "visits", json!(3)).unwrap();
        s.set(Scope::Slot, "m", "name", json!("x")).unwrap();
        assert_eq!(s.get(Scope::Slot, "m", "visits"), Some(json!(3)), "own writes visible before the commit");
        assert!(!d.join("slot2").join("m.json").exists(), "nothing on disk before the save");
        let o = s.commit_slot("game save");
        assert_eq!(o.written, vec!["m"]);
        let f = doc(&d.join("slot2").join("m.json"));
        assert_eq!((f["format"].clone(), f["mod"].clone(), f["bound"].clone()), (json!(1), json!("m"), json!(true)));
        assert_eq!(f["data"], json!({"visits": 3, "name": "x"}));
        assert!(s.commit_slot("game save").written.is_empty(), "nothing changed");
        // a fresh store (next boot) reads it back
        let mut s2 = Store::new(d.clone(), 5);
        s2.set_slot(2, true);
        assert_eq!(s2.get(Scope::Slot, "m", "visits"), Some(json!(3)));
        assert_eq!(s2.get(Scope::Slot, "other", "visits"), None);
        // deleting every key removes the file
        s2.del(Scope::Slot, "m", "visits");
        assert!(s2.del(Scope::Slot, "m", "name"));
        assert!(!s2.del(Scope::Slot, "m", "name"));
        s2.commit_slot("game save");
        assert!(!d.join("slot2").join("m.json").exists());
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn switching_slot_without_a_save_drops_the_changes() {
        let d = tmp("switch");
        let mut s = Store::new(d.clone(), 5);
        s.set_slot(2, true);
        s.set(Scope::Slot, "m", "k", json!(1)).unwrap();
        s.commit_slot("game save");
        s.set(Scope::Slot, "m", "k", json!(2)).unwrap();
        let o = s.set_slot(3, true);
        assert_eq!(o.dropped, vec!["m"]);
        assert_eq!(s.get(Scope::Slot, "m", "k"), None, "slot 3 has its own data");
        s.set_slot(2, true);
        assert_eq!(s.get(Scope::Slot, "m", "k"), Some(json!(1)), "slot 2 keeps its last committed value");
        assert!(s.set_slot(2, true).dropped.is_empty(), "same slot: nothing dropped");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn locked_slot_never_commits() {
        let d = tmp("locked");
        let mut s = Store::new(d.clone(), 5);
        s.set_slot(1, false);
        s.set(Scope::Slot, "m", "k", json!(true)).unwrap();
        assert!(s.commit_slot("game save").written.is_empty());
        assert!(s.commit_mod(Scope::Slot, "m", "forced").is_err());
        assert!(s.flush(|_| true).written.is_empty());
        assert!(!d.join("slot1").exists());
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn global_and_immediate_data_are_flushed_by_the_worker() {
        let d = tmp("flush");
        let mut s = Store::new(d.clone(), 5);
        s.set_slot(3, true);
        s.set(Scope::Global, "m", "boots", json!(7)).unwrap();
        s.set(Scope::Slot, "fast", "k", json!("v")).unwrap();
        s.set(Scope::Slot, "slow", "k", json!("v")).unwrap();
        let o = s.flush(|m| m == "fast");
        assert_eq!(o.written, vec!["global/m", "fast"]);
        assert_eq!(doc(&d.join("global").join("m.json"))["data"], json!({"boots": 7}));
        assert_eq!(doc(&d.join("slot3").join("fast.json"))["bound"], json!(false));
        assert!(!d.join("slot3").join("slow.json").exists());
        assert_eq!(s.dirty(Scope::Slot), vec!["slow"]);
        // forced commit of one mod
        assert_eq!(s.commit_mod(Scope::Slot, "slow", "forced"), Ok(true));
        assert!(d.join("slot3").join("slow.json").exists());
        // global data does not follow the slot
        s.set_slot(2, true);
        assert_eq!(s.get(Scope::Global, "m", "boots"), Some(json!(7)));
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn delete_and_copy_follow_the_retail_slot() {
        let d = tmp("copydel");
        let mut s = Store::new(d.clone(), 2);
        for (slot, v) in [(2u8, 20), (3, 30)] {
            s.set_slot(slot, true);
            s.set(Scope::Slot, "a", "v", json!(v)).unwrap();
            s.set(Scope::Slot, "b", "v", json!(v + 1)).unwrap();
            s.commit_slot("game save");
        }
        // active slot 3, uncommitted change in slot 3
        s.set(Scope::Slot, "a", "v", json!(99)).unwrap();
        // copy 2 -> 3 (the slot screen copies into an empty slot; here 3 had orphan data: it goes to the trash)
        let o = s.on_slot_copied(2, 3);
        assert_eq!(o.copied, 2);
        assert_eq!(o.dropped, vec!["a"]);
        let t = o.trashed.clone().unwrap();
        assert!(t.file_name().unwrap().to_string_lossy().ends_with("_replaced_slot3"));
        assert_eq!(doc(&t.join("a.json"))["data"], json!({"v": 30}));
        assert_eq!(s.get(Scope::Slot, "a", "v"), Some(json!(20)), "slot 3 now holds slot 2's data");
        assert_eq!(doc(&d.join("slot2").join("a.json"))["data"], json!({"v": 20}), "source untouched");
        // delete slot 2 (not active)
        let o = s.on_slot_gone(2, "deleted");
        assert!(o.dropped.is_empty());
        assert!(!d.join("slot2").exists());
        assert!(o.trashed.unwrap().join("b.json").exists());
        // nothing to trash: no folder
        assert_eq!(s.on_slot_gone(7, "deleted").trashed, None);
        // new game in the active slot: its data is trashed too, trash pruned to 2 folders
        let o = s.on_slot_gone(3, "new_game");
        assert!(o.trashed.is_some());
        assert_eq!(s.get(Scope::Slot, "a", "v"), None);
        assert_eq!(std::fs::read_dir(d.join("trash")).unwrap().count(), 2);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn copy_skips_foreign_files() {
        let d = tmp("foreign");
        std::fs::create_dir_all(d.join("slot2")).unwrap();
        std::fs::write(d.join("slot2").join("m.json"), br#"{"format":1,"mod":"m","data":{"k":1}}"#).unwrap();
        std::fs::write(d.join("slot2").join("m.json.tmp"), b"half").unwrap();
        std::fs::write(d.join("slot2").join("Bad Name.json"), b"{}").unwrap();
        let mut s = Store::new(d.clone(), 5);
        assert_eq!(s.on_slot_copied(2, 3).copied, 1);
        assert!(d.join("slot3").join("m.json").exists() && !d.join("slot3").join("m.json.tmp").exists());
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn corrupt_or_newer_files_never_crash() {
        let d = tmp("corrupt");
        std::fs::create_dir_all(d.join("slot2")).unwrap();
        std::fs::write(d.join("slot2").join("m.json"), b"{ not json").unwrap();
        std::fs::write(d.join("slot2").join("n.json"), br#"{"format":9,"mod":"n","data":{"k":1}}"#).unwrap();
        let mut s = Store::new(d.clone(), 5);
        s.set_slot(2, true);
        assert_eq!(s.get(Scope::Slot, "m", "k"), None);
        assert_eq!(s.get(Scope::Slot, "n", "k"), None);
        assert_eq!(s.notes.len(), 2);
        assert!(std::fs::read_dir(d.join("slot2")).unwrap().flatten().any(|e| e.file_name().to_string_lossy().starts_with("m.json.bad-")));
        assert!(d.join("slot2").join("n.json").exists(), "a newer format is kept untouched");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn limits() {
        let d = tmp("limits");
        let mut s = Store::new(d.clone(), 5);
        s.set_slot(2, true);
        assert_eq!(s.set(Scope::Slot, "m", "", json!(1)), Err(SetError::Key));
        assert_eq!(s.set(Scope::Slot, "m", "k", json!("x".repeat(crate::MAX_STR + 1))), Err(SetError::Value));
        for i in 0..crate::MAX_KEYS {
            s.set(Scope::Slot, "m", &format!("k{i}"), json!(i)).unwrap();
        }
        assert_eq!(s.set(Scope::Slot, "m", "one_more", json!(1)), Err(SetError::TooManyKeys));
        assert!(s.set(Scope::Slot, "m", "k0", json!(5)).is_ok(), "replacing a key is fine");
        let _ = std::fs::remove_dir_all(&d);
    }
}
