//! Build cache. Engines build their outputs at the early phase; a build is skipped when nothing it depends on
//! changed. Two shapes, both in use:
//!
//! * **Manifest cache** (text engine): a hash of the declared inputs ([`InputHash`]: engine version, mod files, load
//!   order…) + size / mtime of every file the build read ([`Dep`], [`stats`], [`unchanged`]), kept in a JSON manifest
//!   ([`read_json`], [`write_json`]) next to the outputs in `evt_loader\cache\<engine>\`. Same inputs and unchanged
//!   files = reuse the previous result without reading the game (the ModLoader's data-delta cache works the same way,
//!   `crates/vr-loader/src/mods/merge.rs`).
//! * **Stamp cache** (audio engine, [`StampCache`]): one `.stamp` per output holding the hash of the inputs it was
//!   built from ([`hash_inputs`] over [`file_stamp`]s and settings), so outputs are rebuilt one by one.
//!
//! Both hashes include the engine's version: bump it when the same inputs give different outputs.

use serde::{de::DeserializeOwned, Deserialize, Serialize};
use sha1::{Digest, Sha1};
use std::path::{Path, PathBuf};

// ---------------------------------------------------------------- manifest cache

/// Size + mtime of a file the build read.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Dep {
    pub path: String,
    pub size: u64,
    pub mtime: u128,
}

pub fn stat(p: &Path) -> Option<Dep> {
    let m = std::fs::metadata(p).ok()?;
    let mtime = m.modified().ok()?.duration_since(std::time::UNIX_EPOCH).ok()?.as_nanos();
    Some(Dep { path: p.to_string_lossy().into_owned(), size: m.len(), mtime })
}

/// Stats of the given files (deduplicated, missing ones skipped).
pub fn stats(paths: &[PathBuf]) -> Vec<Dep> {
    let mut seen = std::collections::HashSet::new();
    paths.iter().filter(|p| seen.insert(p.to_path_buf())).filter_map(|p| stat(p)).collect()
}

/// Every dependency still has the recorded size and mtime.
pub fn unchanged(deps: &[Dep]) -> bool {
    deps.iter().all(|d| stat(Path::new(&d.path)).as_ref() == Some(d))
}

/// Incremental input hash (hex SHA-1). Parts are length-prefixed, so `("ab", "c")` != `("a", "bc")`.
#[derive(Default)]
pub struct InputHash(Sha1);

impl InputHash {
    pub fn new(tag: &str) -> InputHash {
        let mut h = InputHash(Sha1::new());
        h.part(tag.as_bytes());
        h
    }
    pub fn part(&mut self, b: &[u8]) -> &mut Self {
        self.0.update((b.len() as u64).to_le_bytes());
        self.0.update(b);
        self
    }
    pub fn str(&mut self, s: &str) -> &mut Self {
        self.part(s.as_bytes())
    }
    /// A file's bytes (a data file of a mod) or its absence.
    pub fn file(&mut self, p: &Path) -> &mut Self {
        match std::fs::read(p) {
            Ok(b) => self.str(&p.to_string_lossy()).part(&b),
            Err(_) => self.str(&format!("{} missing", p.display())),
        }
    }
    pub fn finish(self) -> String {
        self.0.finalize().iter().map(|b| format!("{b:02x}")).collect()
    }
}

pub fn read_json<T: DeserializeOwned>(p: &Path) -> Option<T> {
    serde_json::from_slice(&std::fs::read(p).ok()?).ok()
}

/// Write via a temporary file + rename (a crash never leaves half a file); [`crate::fsx::write_atomic`] with the
/// error as text.
pub fn write_atomic(p: &Path, bytes: &[u8]) -> Result<(), String> {
    crate::fsx::write_atomic(p, bytes).map_err(|e| format!("{}: {e}", p.display()))
}

pub fn write_json<T: Serialize>(p: &Path, v: &T) -> Result<(), String> {
    write_atomic(p, &serde_json::to_vec_pretty(v).map_err(|e| e.to_string())?)
}

// ---------------------------------------------------------------- stamp cache

/// Size + mtime of a file as one string, for input hashes (`<path>|missing` when it is not there).
pub fn file_stamp(p: &Path) -> String {
    match std::fs::metadata(p) {
        Ok(m) => format!("{}|{}|{}", p.display(), m.len(), m.modified().ok().and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok()).map_or(0, |d| d.as_nanos())),
        Err(_) => format!("{}|missing", p.display()),
    }
}

/// Hex MD5 of a list of strings (NUL after each): the input hash of one [`StampCache`] output. (The format of the
/// stamps already on disk, from the audio engine's boot cache and the `audio_build` pack-time tool.)
pub fn hash_inputs(parts: &[String]) -> String {
    use md5::Digest;
    let mut h = md5::Md5::new();
    for p in parts {
        h.update(p.as_bytes());
        h.update([0u8]);
    }
    h.finalize().iter().map(|b| format!("{b:02x}")).collect()
}

/// A build cache of separate outputs: files under `root` (laid out like the game: `data\...`), each with a `.stamp`
/// (the hash of the inputs it was built from) next to it, or under `stamps` when given (so a mod's `files\` tree only
/// holds game files).
pub struct StampCache {
    pub root: PathBuf,
    pub stamps: Option<PathBuf>,
}

impl StampCache {
    pub fn new(root: PathBuf) -> StampCache {
        StampCache { root, stamps: None }
    }

    pub fn with_stamps(root: PathBuf, stamps: PathBuf) -> StampCache {
        StampCache { root, stamps: Some(stamps) }
    }

    /// Path of output `rel` (`/`-separated).
    pub fn path(&self, rel: &str) -> PathBuf {
        crate::fsx::key_path(&self.root, rel)
    }

    fn stamp(&self, rel: &str) -> PathBuf {
        let base = match &self.stamps {
            Some(s) => crate::fsx::key_path(s, rel),
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

    /// Delete outputs that are not in `keep` (rels) — stale builds nobody asks for any more.
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
    fn hash_parts_are_delimited_and_deps_detect_changes() {
        let h = |parts: &[&str]| {
            let mut h = InputHash::new("t");
            for p in parts {
                h.str(p);
            }
            h.finish()
        };
        assert_ne!(h(&["ab", "c"]), h(&["a", "bc"]));
        assert_eq!(h(&["x"]), h(&["x"]));
        let d = std::env::temp_dir().join(format!("vr-fw-cache-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        let p = d.join("f.bin");
        write_atomic(&p, b"one").unwrap();
        let deps = stats(&[p.clone(), p.clone(), d.join("missing")]);
        assert_eq!(deps.len(), 1);
        assert!(unchanged(&deps));
        std::fs::write(&p, b"longer").unwrap();
        assert!(!unchanged(&deps));
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn stamp_cache_fresh_seal_prune() {
        let root = std::env::temp_dir().join(format!("vr-fw-stamps-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let c = StampCache::new(root.join("cache"));
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
        // the stamp hash format (MD5, NUL-separated) is the one already on disk
        assert_eq!(hash_inputs(&["a".into()]), "4144e195f46de78a3623da7364d04f11");
        let _ = std::fs::remove_dir_all(&root);
    }
}
