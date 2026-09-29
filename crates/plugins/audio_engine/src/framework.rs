//! GENERIC pieces (no audio in here), kept apart so they can move into the shared `vr-framework` crate
//! (docs/app/modloader-roadmap.md «VR-Framework»):
//!
//! * [`ModDir`] / [`mods_with`] — per-mod folder discovery (active mods in load order that have a given file);
//! * [`overlay_winner`] — which mod's `files\` serves a game path (the overlay rule: the one that loads last);
//! * [`merge_by_key`] — cross-mod merge rule: same key from several mods → the one that loads last wins, with a
//!   conflict note;
//! * [`Cache`] — hash-keyed build cache (`<evt_loader>\cache\<plugin>\…` + a `.stamp` per output);
//! * [`GameFiles`] — where the game's own files come from (plugin: `game_file_path`; tools: a dump folder).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// One active mod.
#[derive(Debug, Clone, PartialEq)]
pub struct ModDir {
    pub id: String,
    pub dir: PathBuf,
    pub load_index: u32,
}

/// The mods (load order) that have `rel` in their folder, with its text.
pub fn mods_with(mods: &[ModDir], rel: &str) -> Vec<(ModDir, String)> {
    let mut v: Vec<&ModDir> = mods.iter().collect();
    v.sort_by_key(|m| m.load_index);
    v.into_iter().filter_map(|m| std::fs::read_to_string(m.dir.join(rel)).ok().map(|t| (m.clone(), t))).collect()
}

/// `files\<key>` of the mod that the overlay serves `key` from (the last one in load order that has it).
pub fn overlay_winner(mods: &[ModDir], key: &str) -> Option<(ModDir, PathBuf)> {
    let rel: PathBuf = key.split('/').collect();
    let mut v: Vec<&ModDir> = mods.iter().collect();
    v.sort_by_key(|m| m.load_index);
    v.into_iter().rev().find_map(|m| {
        let p = m.dir.join("files").join(&rel);
        p.is_file().then(|| (m.clone(), p))
    })
}

/// A merge note: `key` was set by `losers` (in load order) and by `winner` (loads last).
#[derive(Debug, Clone, PartialEq)]
pub struct Conflict {
    pub key: String,
    pub winner: String,
    pub losers: Vec<String>,
}

/// Merge `(mod id, key, value)` items given in load order: the last value of each key wins.
pub fn merge_by_key<V: Clone>(items: &[(String, String, V)]) -> (BTreeMap<String, (String, V)>, Vec<Conflict>) {
    let mut out: BTreeMap<String, (String, V)> = BTreeMap::new();
    let mut seen: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for (m, k, v) in items {
        seen.entry(k.clone()).or_default().push(m.clone());
        out.insert(k.clone(), (m.clone(), v.clone()));
    }
    let conflicts = seen
        .into_iter()
        .filter(|(_, ms)| ms.len() > 1)
        .map(|(k, mut ms)| {
            let winner = ms.pop().unwrap();
            Conflict { key: k, winner, losers: ms }
        })
        .collect();
    (out, conflicts)
}

/// Where the game's own files (no mods) come from.
pub trait GameFiles {
    /// A file on disk with the game's bytes of `key` (`data/...`), if the game has it.
    fn path(&self, key: &str) -> Option<PathBuf>;
}

/// A folder laid out like the game's `data` (the v7.1.2 dump, or an all-loose install): `<root>/<key without data/>`.
pub struct DumpFiles(pub PathBuf);

impl GameFiles for DumpFiles {
    fn path(&self, key: &str) -> Option<PathBuf> {
        let rel = key.strip_prefix("data/").unwrap_or(key);
        let p: PathBuf = std::iter::once(self.0.clone()).chain(rel.split('/').map(PathBuf::from)).collect();
        p.is_file().then_some(p)
    }
}

/// Size + mtime of a file, for input hashes.
pub fn file_stamp(p: &Path) -> String {
    match std::fs::metadata(p) {
        Ok(m) => format!("{}|{}|{}", p.display(), m.len(), m.modified().ok().and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok()).map_or(0, |d| d.as_nanos())),
        Err(_) => format!("{}|missing", p.display()),
    }
}

/// Hex MD5 of a list of strings (input hash of a build).
pub fn hash_inputs(parts: &[String]) -> String {
    use md5::Digest;
    let mut h = md5::Md5::new();
    for p in parts {
        h.update(p.as_bytes());
        h.update([0u8]);
    }
    h.finalize().iter().map(|b| format!("{b:02x}")).collect()
}

/// A build cache: outputs under `root`, each with a `.stamp` (the hash of the inputs it was built from) next to it, or
/// under `stamps` when given (so a mod's `files\` tree only holds game files).
pub struct Cache {
    pub root: PathBuf,
    pub stamps: Option<PathBuf>,
}

impl Cache {
    pub fn new(root: PathBuf) -> Cache {
        Cache { root, stamps: None }
    }

    pub fn with_stamps(root: PathBuf, stamps: PathBuf) -> Cache {
        Cache { root, stamps: Some(stamps) }
    }

    /// Path of output `rel` (`/`-separated).
    pub fn path(&self, rel: &str) -> PathBuf {
        let mut p = self.root.clone();
        for s in rel.split('/') {
            p.push(s);
        }
        p
    }

    fn stamp(&self, rel: &str) -> PathBuf {
        let base = match &self.stamps {
            Some(s) => {
                let mut p = s.clone();
                for x in rel.split('/') {
                    p.push(x);
                }
                p
            }
            None => self.path(rel),
        };
        let mut s = base.into_os_string();
        s.push(".stamp");
        PathBuf::from(s)
    }

    /// Are all `rels` present and built from `hash`?
    pub fn fresh(&self, rels: &[&str], hash: &str) -> bool {
        rels.iter().all(|r| self.path(r).is_file() && std::fs::read_to_string(self.stamp(r)).is_ok_and(|s| s == hash))
    }

    /// Record that `rels` were built from `hash` (after writing them).
    pub fn seal(&self, rels: &[&str], hash: &str) -> std::io::Result<()> {
        for r in rels {
            let s = self.stamp(r);
            if let Some(d) = s.parent() {
                std::fs::create_dir_all(d)?;
            }
            std::fs::write(s, hash)?;
        }
        Ok(())
    }

    /// Delete outputs that are not in `keep` (rels) — stale builds of banks nobody changes any more.
    pub fn prune(&self, keep: &[String]) -> Vec<PathBuf> {
        let mut gone = Vec::new();
        let keep: Vec<PathBuf> = keep.iter().map(|r| self.path(r)).collect();
        fn walk(d: &Path, out: &mut Vec<PathBuf>) {
            if let Ok(rd) = std::fs::read_dir(d) {
                for e in rd.flatten() {
                    let p = e.path();
                    if p.is_dir() {
                        walk(&p, out);
                    } else {
                        out.push(p);
                    }
                }
            }
        }
        let mut all = Vec::new();
        walk(&self.root, &mut all);
        for p in all {
            let base = p.to_string_lossy().trim_end_matches(".stamp").to_string();
            if !keep.iter().any(|k| k.to_string_lossy() == base) && std::fs::remove_file(&p).is_ok() {
                gone.push(p);
            }
        }
        gone
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn merge_rule_last_wins_with_conflicts() {
        let items = vec![
            ("a".to_string(), "waza_stream/ev60_1".to_string(), 1),
            ("b".to_string(), "waza_stream/ev60_2".to_string(), 2),
            ("c".to_string(), "waza_stream/ev60_1".to_string(), 3),
        ];
        let (m, c) = merge_by_key(&items);
        assert_eq!(m["waza_stream/ev60_1"], ("c".to_string(), 3));
        assert_eq!(m["waza_stream/ev60_2"], ("b".to_string(), 2));
        assert_eq!(c, vec![Conflict { key: "waza_stream/ev60_1".into(), winner: "c".into(), losers: vec!["a".into()] }]);
    }

    #[test]
    fn cache_and_winner() {
        let root = std::env::temp_dir().join(format!("evt-fw-cache-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let c = Cache::new(root.join("cache"));
        let p = c.path("data/x/a.bin");
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        assert!(!c.fresh(&["data/x/a.bin"], "h1"));
        std::fs::write(&p, b"1").unwrap();
        c.seal(&["data/x/a.bin"], "h1").unwrap();
        assert!(c.fresh(&["data/x/a.bin"], "h1"));
        assert!(!c.fresh(&["data/x/a.bin"], "h2"));
        std::fs::write(c.path("data/x/old.bin"), b"2").unwrap();
        let gone = c.prune(&["data/x/a.bin".to_string()]);
        assert_eq!(gone.len(), 1);
        assert!(p.is_file());
        // overlay winner = last in load order
        for (id, i) in [("m1", 0u32), ("m2", 1)] {
            let f = root.join(id).join("files/data/common/a.acb");
            std::fs::create_dir_all(f.parent().unwrap()).unwrap();
            std::fs::write(&f, id).unwrap();
            let _ = i;
        }
        let mods = vec![ModDir { id: "m2".into(), dir: root.join("m2"), load_index: 1 }, ModDir { id: "m1".into(), dir: root.join("m1"), load_index: 0 }];
        assert_eq!(overlay_winner(&mods, "data/common/a.acb").unwrap().0.id, "m2");
        assert!(overlay_winner(&mods, "data/common/b.acb").is_none());
        let _ = std::fs::remove_dir_all(&root);
    }
}
