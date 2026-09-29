//! Build cache (generic): a hash of the declared inputs + size / mtime of every file the build read. Same inputs and
//! unchanged files = reuse the previous result without reading the game (the ModLoader's data-delta cache works the
//! same way, `crates/vr-loader/src/mods/merge.rs`).

use serde::{de::DeserializeOwned, Deserialize, Serialize};
use sha1::{Digest, Sha1};
use std::path::{Path, PathBuf};

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
    pub fn finish(self) -> String {
        self.0.finalize().iter().map(|b| format!("{b:02x}")).collect()
    }
}

pub fn read_json<T: DeserializeOwned>(p: &Path) -> Option<T> {
    serde_json::from_slice(&std::fs::read(p).ok()?).ok()
}

/// Write via a temporary file + rename (a crash never leaves half a file).
pub fn write_atomic(p: &Path, bytes: &[u8]) -> Result<(), String> {
    if let Some(d) = p.parent() {
        std::fs::create_dir_all(d).map_err(|e| format!("{}: {e}", d.display()))?;
    }
    let tmp = p.with_extension("tmp~");
    std::fs::write(&tmp, bytes).map_err(|e| format!("{}: {e}", tmp.display()))?;
    std::fs::rename(&tmp, p).map_err(|e| format!("{}: {e}", p.display()))
}

pub fn write_json<T: Serialize>(p: &Path, v: &T) -> Result<(), String> {
    write_atomic(p, &serde_json::to_vec_pretty(v).map_err(|e| e.to_string())?)
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
        let d = std::env::temp_dir().join(format!("evt-te-cache-{}", std::process::id()));
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
}
