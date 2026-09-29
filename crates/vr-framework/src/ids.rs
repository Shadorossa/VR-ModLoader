//! Ids. Three kinds:
//!
//! * **Mod ids** ([`valid_mod_id`]): folder names of mods, `[a-z0-9_][a-z0-9_.-]{0,63}`.
//! * **Own ids** of new entries, namespaced by the mod: `<mod_id>.<name>` ([`own_id`], [`split_own`]); `name` is
//!   `[a-z0-9_]` plus `.` / `-` inside ([`valid_name`]). A modder writes only `name`; the engine adds the mod id.
//! * **Game ids**: the game names everything by the standard CRC-32 of the name ([`crc32`]; `c01020010`,
//!   `whs01980`, table labels, cue names…). [`parse_id`] reads a reference as the game stores it: a number, `0x…`
//!   hex, a negative number (the game's signed cells) or a name (its crc32).
//!
//! **Numbers for new entries.** The game needs a 32-bit id for each new entry. [`probe_id`] derives it from the own id
//! (`crc32("<mod>.<name>")`, then `crc32("<mod>.<name>#1")`, `#2`… while taken), so the same key gives the same id on
//! every PC. [`IdRegistry`] also **remembers** every id it gave (a JSON file outside the build cache), so an id never
//! changes between starts even when a later mod or game file takes the number it was derived from.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// Standard CRC-32 (zlib / IEEE) of a name's bytes: the game's id of every named thing.
pub fn crc32(name: &str) -> u32 {
    crc32fast::hash(name.as_bytes())
}

/// A mod id usable as a file name: `[a-z0-9_][a-z0-9_.-]{0,63}` (the mod.toml rule, plus a leading `_` for
/// `_local`).
pub fn valid_mod_id(id: &str) -> bool {
    let b = id.as_bytes();
    !b.is_empty()
        && b.len() <= 64
        && (b[0].is_ascii_lowercase() || b[0].is_ascii_digit() || b[0] == b'_')
        && b.iter().all(|&c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, b'_' | b'.' | b'-'))
}

/// The name part of an own id: 1..=64 bytes of `[a-z0-9_]`, `.` and `-` allowed inside (English, lower case).
pub fn valid_name(name: &str) -> bool {
    let b = name.as_bytes();
    !b.is_empty()
        && b.len() <= 64
        && (b[0].is_ascii_lowercase() || b[0].is_ascii_digit() || b[0] == b'_')
        && b.iter().all(|&c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, b'_' | b'.' | b'-'))
        && !name.ends_with(['.', '-'])
}

/// `<mod_id>.<name>`, or why not.
pub fn own_id(mod_id: &str, name: &str) -> Result<String, String> {
    if !valid_mod_id(mod_id) {
        return Err(format!("`{mod_id}` is not a mod id (a-z 0-9 _ . -)"));
    }
    if !valid_name(name) {
        return Err(format!("`{name}` is not a valid name for a new entry (lower-case English: a-z 0-9 _, `.` / `-` inside)"));
    }
    Ok(format!("{mod_id}.{name}"))
}

/// `<mod_id>.<name>` → `(mod_id, name)` when `mod_id` is one of `mods` (mod ids may contain dots: the longest match
/// wins).
pub fn split_own<'a>(key: &'a str, mods: &[&str]) -> Option<(&'a str, &'a str)> {
    mods.iter()
        .filter(|m| key.len() > m.len() + 1 && key.starts_with(**m) && key.as_bytes()[m.len()] == b'.')
        .max_by_key(|m| m.len())
        .map(|m| (&key[..m.len()], &key[m.len() + 1..]))
}

/// A game id as written by a modder: a number (`123`, `-123`), `0x7B` hex, or a name → crc32 of the name.
pub fn parse_id(s: &str) -> u32 {
    let t = s.trim();
    if let Some(h) = t.strip_prefix("0x").or_else(|| t.strip_prefix("0X")) {
        if let Ok(v) = u32::from_str_radix(h, 16) {
            return v;
        }
    }
    if let Ok(v) = t.parse::<u32>() {
        return v;
    }
    if let Ok(v) = t.parse::<i32>() {
        return v as u32;
    }
    crc32(t)
}

/// The id of new entry `key` (an own id): `crc32(key)`, else `crc32("<key>#1")`, `#2`… until one is not `taken`
/// (0 and `u32::MAX` are never given). Allocate keys in sorted order so the result does not depend on the load order.
pub fn probe_id(key: &str, taken: impl Fn(u32) -> bool) -> u32 {
    let mut id = crc32(key);
    let mut i = 1u32;
    while taken(id) || id == 0 || id == u32::MAX {
        id = crc32(&format!("{key}#{i}"));
        i += 1;
    }
    id
}

/// Ids given to new entries, remembered between starts: `<file>` = `{"format": 1, "ids": {"<key>": id}}`.
/// Keep it outside the build cache (`evt_loader\ids\<engine>.json`, [`IdRegistry::default_path`]): deleting the
/// cache must never renumber anything.
#[derive(Debug, Clone)]
pub struct IdRegistry {
    pub path: PathBuf,
    ids: BTreeMap<String, u32>,
    dirty: bool,
}

#[derive(Serialize, Deserialize)]
struct RegistryFile {
    format: u32,
    ids: BTreeMap<String, u32>,
}

impl IdRegistry {
    /// `<loader_dir>\ids\<engine>.json`.
    pub fn default_path(loader_dir: &Path, engine: &str) -> PathBuf {
        loader_dir.join("ids").join(format!("{engine}.json"))
    }

    /// Load (missing file = empty). A file that does not parse is kept aside as `<file>.bad` and the registry starts
    /// empty: Err carries the message for the log.
    pub fn load(path: &Path) -> (IdRegistry, Option<String>) {
        let mut r = IdRegistry { path: path.to_path_buf(), ids: BTreeMap::new(), dirty: false };
        let Ok(b) = std::fs::read(path) else { return (r, None) };
        match serde_json::from_slice::<RegistryFile>(&b) {
            Ok(f) => {
                r.ids = f.ids;
                (r, None)
            }
            Err(e) => {
                let mut bad = path.as_os_str().to_owned();
                bad.push(".bad");
                let _ = std::fs::rename(path, PathBuf::from(bad));
                (r, Some(format!("{}: not readable ({e}): kept as .bad, ids are given again", path.display())))
            }
        }
    }

    /// The id of `key`: the remembered one, else a new one by [`probe_id`] that is neither `taken` nor remembered for
    /// another key. Call in sorted key order for new keys.
    pub fn assign(&mut self, key: &str, taken: impl Fn(u32) -> bool) -> u32 {
        if let Some(&id) = self.ids.get(key) {
            return id;
        }
        let used: std::collections::HashSet<u32> = self.ids.values().copied().collect();
        let id = probe_id(key, |i| taken(i) || used.contains(&i));
        self.ids.insert(key.to_string(), id);
        self.dirty = true;
        id
    }

    pub fn get(&self, key: &str) -> Option<u32> {
        self.ids.get(key).copied()
    }

    /// Every remembered `(key, id)`.
    pub fn iter(&self) -> impl Iterator<Item = (&str, u32)> {
        self.ids.iter().map(|(k, v)| (k.as_str(), *v))
    }

    /// Write the file when something was added (atomically). Ids of keys no mod uses any more are kept: a mod that
    /// comes back gets its old numbers.
    pub fn save(&mut self) -> std::io::Result<()> {
        if !self.dirty {
            return Ok(());
        }
        let f = RegistryFile { format: 1, ids: self.ids.clone() };
        crate::fsx::write_atomic(&self.path, &serde_json::to_vec_pretty(&f).map_err(std::io::Error::other)?)?;
        self.dirty = false;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mod_ids_names_and_own_ids() {
        for ok in ["my_mod", "a", "_local", "mod-2.1", "0x"] {
            assert!(valid_mod_id(ok), "{ok}");
        }
        for bad in ["", "My", "a/b", "..", ".x", "a b", "a\\b", &"x".repeat(65)] {
            assert!(!valid_mod_id(bad), "{bad}");
        }
        assert_eq!(own_id("my_mod", "kaito").unwrap(), "my_mod.kaito");
        assert!(own_id("my_mod", "Kaito").is_err() && own_id("my_mod", "kaito.").is_err() && own_id("Mi", "x").is_err());
        assert_eq!(split_own("my.mod.kaito", &["my", "my.mod"]), Some(("my.mod", "kaito")));
        assert_eq!(split_own("other.kaito", &["my"]), None);
    }

    #[test]
    fn game_ids() {
        assert_eq!(crc32("c01000010"), 0x99A1_C150);
        assert_eq!(parse_id("0x7B"), 123);
        assert_eq!(parse_id("-1"), u32::MAX);
        assert_eq!(parse_id("123"), 123);
        assert_eq!(parse_id(" sysmes_x "), crc32("sysmes_x"));
        let taken = crc32("a.x");
        assert_eq!(probe_id("a.x", |i| i == taken), crc32("a.x#1"));
        assert_eq!(probe_id("a.x", |_| false), taken);
    }

    #[test]
    fn registry_keeps_ids_between_starts() {
        let d = std::env::temp_dir().join(format!("vr-fw-ids-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        let p = IdRegistry::default_path(&d, "example_engine");
        let (mut r, e) = IdRegistry::load(&p);
        assert!(e.is_none());
        let a = r.assign("m.a", |_| false);
        assert_eq!(a, crc32("m.a"));
        r.save().unwrap();
        // next start: the game (or another mod) now has crc32("m.a"): the remembered id stays
        let (mut r, _) = IdRegistry::load(&p);
        assert_eq!(r.assign("m.a", |i| i == a), a);
        // a new key never takes a remembered number
        let b = r.assign("m.b", |i| i == crc32("m.b"));
        assert_eq!(b, crc32("m.b#1"));
        r.save().unwrap();
        assert_eq!(IdRegistry::load(&p).0.iter().count(), 2);
        std::fs::write(&p, b"{").unwrap();
        let (r, e) = IdRegistry::load(&p);
        assert!(e.is_some() && r.iter().count() == 0);
        let _ = std::fs::remove_dir_all(&d);
    }
}
