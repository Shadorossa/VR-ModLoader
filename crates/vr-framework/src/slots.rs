//! Serving generated files through the mods overlay **without a loader API** (generic). Only the fallback for a
//! ModLoader without `file_serve` (plugin API v1 +248): with it, the engines write their output to the cache and serve
//! it at its real size at the early phase, shown at the first start (see [`crate::host::EarlyMode`]).
//!
//! The ModLoader builds its overlay in DllMain, before any plugin runs: the files of every mod's `files\data\...` and
//! their SIZES (the size goes into the in-memory cpk_list record, and the game reads exactly that many bytes). A
//! plugin can therefore only change the *content* of a file that already existed at DllMain, keeping its size. So an
//! engine serves each generated file from a **slot**: `<engine mod>\files\<key>`, bigger than the content, rewritten
//! in place at the early phase (exe entry point, before any game code) with the content padded to the slot size by
//! the format's own padding (a zero tail the format ignores).
//!
//! * slot exists and the content fits → rewritten now, **served at this start** ([`SlotState::Served`]);
//! * slot missing → created now with room to grow; the overlay did not map it, so it is **served from the next
//!   start** ([`SlotState::Pending`]);
//! * slot too small (live) → left as it is (still a valid file of the size the overlay registered, with the previous
//!   content): [`SlotState::Stale`]; the fix is to delete the slot with the game closed (or run the offline tool,
//!   which may resize freely) and start again.
//!
//! The loader API that removes the dance exists now: `file_serve(h, key, path)` during the early phase (the loader
//! registers the file with its real size before the game opens anything).

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SlotState {
    /// The slot holds the new content and the overlay serves it at this start.
    Served,
    /// Created at this start: served from the next start.
    Pending,
    /// The content does not fit the slot the overlay mapped: the previous content stays.
    Stale,
}

/// Sizing / mode of the slots.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SlotPolicy {
    /// Minimum free room of a new slot (bytes).
    pub min_headroom: usize,
    /// Slot sizes are multiples of this (>= 16, power of two).
    pub align: usize,
    /// true = in the game (sizes are fixed once mapped); false = offline tool, game closed (slots may be resized).
    pub live: bool,
}

impl Default for SlotPolicy {
    fn default() -> Self {
        SlotPolicy { min_headroom: 64 * 1024, align: 4096, live: true }
    }
}

impl SlotPolicy {
    /// Size of a new slot for content of `len` bytes: `len` + max(min headroom, len / 8), aligned.
    pub fn slot_size(&self, len: usize) -> usize {
        let a = self.align.max(16);
        (len + self.min_headroom.max(len / 8)).div_ceil(a) * a
    }
}

/// Result of [`put`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Put {
    pub state: SlotState,
    /// Size of the slot file now.
    pub size: u64,
    /// The file was (re)written.
    pub wrote: bool,
}

/// Format padding: exactly `size` bytes from `content`, or Err when impossible.
pub type Pad<'a> = &'a dyn Fn(&[u8], usize) -> Result<Vec<u8>, String>;

/// Put `content` into the slot at `path`. `reserve` = a length the slot should hold at least when it is created
/// (e.g. the base file's size, so a later smaller/larger merge still fits).
pub fn put(path: &Path, content: &[u8], reserve: usize, pad: Pad, p: &SlotPolicy) -> Result<Put, String> {
    let existing = std::fs::metadata(path).ok().map(|m| m.len() as usize);
    let (size, state) = match existing {
        Some(s) if content.len() <= s => (s, SlotState::Served),
        Some(_) if p.live => return Ok(Put { state: SlotState::Stale, size: existing.unwrap_or(0) as u64, wrote: false }),
        Some(_) => (p.slot_size(content.len().max(reserve)), SlotState::Served),
        None => (p.slot_size(content.len().max(reserve)), if p.live { SlotState::Pending } else { SlotState::Served }),
    };
    let bytes = pad(content, size)?;
    if bytes.len() != size {
        return Err(format!("padding gave {} bytes instead of {size}", bytes.len()));
    }
    let same = existing == Some(size) && std::fs::read(path).map(|old| old == bytes).unwrap_or(false);
    if !same {
        write_in_place(path, &bytes)?;
    }
    Ok(Put { state, size: size as u64, wrote: !same })
}

/// Overwrite (or create) a file. A mapped slot is rewritten in place (same path, same size), never renamed: the
/// overlay keeps the path it registered.
fn write_in_place(path: &Path, bytes: &[u8]) -> Result<(), String> {
    if let Some(d) = path.parent() {
        std::fs::create_dir_all(d).map_err(|e| format!("{}: {e}", d.display()))?;
    }
    std::fs::write(path, bytes).map_err(|e| format!("{}: {e}", path.display()))
}

/// Existing slot files under `<engine dir>\files\<prefix>` as `(key, path)`, key = `data/...` lower case.
pub fn list(engine_dir: &Path, prefix: &str) -> Vec<(String, PathBuf)> {
    list_under(&engine_dir.join("files"), prefix)
}

/// Files under `<root>\<prefix>` as `(key, path)`, key = `<prefix>/...` lower case (`root` = a folder whose layout is
/// the game's: a mod's `files\`, an engine's cache).
pub fn list_under(base: &Path, prefix: &str) -> Vec<(String, PathBuf)> {
    let mut out = Vec::new();
    let mut root = base.to_path_buf();
    for p in prefix.split('/').filter(|p| !p.is_empty()) {
        root.push(p);
    }
    fn walk(dir: &Path, rel: &str, out: &mut Vec<(String, PathBuf)>) {
        if let Ok(rd) = std::fs::read_dir(dir) {
            for e in rd.flatten() {
                let n = e.file_name().to_string_lossy().to_ascii_lowercase();
                let r = format!("{rel}/{n}");
                if e.path().is_dir() {
                    walk(&e.path(), &r, out);
                } else {
                    out.push((r, e.path()));
                }
            }
        }
    }
    walk(&root, prefix.trim_end_matches('/'), &mut out);
    out.sort();
    out
}

/// Delete the files of `keys` under `<root>` (layout of the game) and the folders left empty (`root` included).
/// `(deleted, errors)`. A slot mapped by the overlay may be deleted only once something else serves its key.
pub fn retire(root: &Path, keys: &[String]) -> (usize, Vec<String>) {
    let (mut n, mut errs) = (0, Vec::new());
    for k in keys {
        let mut p = root.to_path_buf();
        for part in k.split('/') {
            p.push(part);
        }
        match std::fs::remove_file(&p) {
            Ok(()) => n += 1,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => errs.push(format!("{}: {e}", p.display())),
        }
        // empty folders up to the root (remove_dir fails on a non-empty one)
        let mut d = p.parent().map(Path::to_path_buf);
        while let Some(dir) = d {
            if !dir.starts_with(root) || std::fs::remove_dir(&dir).is_err() {
                break;
            }
            d = dir.parent().map(Path::to_path_buf);
        }
    }
    (n, errs)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pad(c: &[u8], size: usize) -> Result<Vec<u8>, String> {
        if c.len() > size {
            return Err("too big".into());
        }
        let mut v = c.to_vec();
        v.resize(size, 0);
        Ok(v)
    }

    #[test]
    fn slot_lifecycle() {
        let d = std::env::temp_dir().join(format!("vr-fw-slots-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        let p = d.join("files").join("data").join("x").join("a.cfg.bin");
        let live = SlotPolicy { min_headroom: 100, align: 64, live: true };
        assert_eq!(live.slot_size(10), 128);
        assert_eq!(live.slot_size(1000), 1152);
        // missing: created, pending
        let r = put(&p, b"hello", 0, &pad, &live).unwrap();
        assert_eq!((r.state, r.size, r.wrote), (SlotState::Pending, 128, true));
        // fits: served; same content again: not rewritten
        let r = put(&p, b"hello2", 0, &pad, &live).unwrap();
        assert_eq!((r.state, r.size, r.wrote), (SlotState::Served, 128, true));
        assert!(!put(&p, b"hello2", 0, &pad, &live).unwrap().wrote);
        // too big while live: stale, untouched
        let big = vec![7u8; 200];
        let r = put(&p, &big, 0, &pad, &live).unwrap();
        assert_eq!((r.state, r.size, r.wrote), (SlotState::Stale, 128, false));
        assert_eq!(&std::fs::read(&p).unwrap()[..6], b"hello2");
        // offline: resized
        let off = SlotPolicy { live: false, ..live };
        let r = put(&p, &big, 0, &pad, &off).unwrap();
        assert_eq!((r.state, r.size), (SlotState::Served, 320));
        assert_eq!(list(&d, "data/x").iter().map(|(k, _)| k.as_str()).collect::<Vec<_>>(), ["data/x/a.cfg.bin"]);
        // retired: file and the empty folders gone, the engine folder stays
        std::fs::write(d.join("keep.txt"), b"k").unwrap();
        assert_eq!(retire(&d.join("files"), &["data/x/a.cfg.bin".into(), "data/x/missing.cfg.bin".into()]), (1, vec![]));
        assert!(!d.join("files").exists() && d.join("keep.txt").exists());
        let _ = std::fs::remove_dir_all(&d);
    }
}
