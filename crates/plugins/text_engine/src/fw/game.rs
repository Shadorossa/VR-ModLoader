//! Where a base file comes from (generic). Order, like the ModLoader's data-delta merge
//! (`crates/vr-loader/src/mods/merge.rs`):
//!
//! 1. the whole-file override of another active mod (`<mod>\files\data\...`), the one that loads last; the engine's own
//!    folder never counts (its files are the engine's output);
//! 2. the file the game would read: `data\cpk_list.cfg.bin` record → loose file under the game folder (what the app
//!    installs), else extracted from its CPK (decrypted + decompressed by `vr_gamefiles::cpk`); a key missing from
//!    the list is read loose when it exists (an all-loose dump).
//!
//! Every file read is returned as a cache dependency.

use super::ModDir;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// A file and the paths it was read from.
pub type Read = Option<(Vec<u8>, Vec<PathBuf>)>;

/// A game file source keyed by overlay keys (`data/...`, lower case, `/`).
pub trait Source {
    /// The file and the paths it was read from (cache dependencies). Ok(None) = no such file.
    fn read(&mut self, key: &str) -> Result<Read, String>;
    /// Every key of the source (cpk_list keys; used to find versioned file names).
    fn keys(&mut self) -> Result<Vec<String>, String>;
}

/// The installed game (or an extracted dump with its `data\cpk_list.cfg.bin`).
pub struct GameSource {
    pub game_dir: PathBuf,
    list: Option<(vr_gamefiles::cpk_list::CpkList, HashMap<String, usize>)>,
}

impl GameSource {
    pub fn new(game_dir: &Path) -> GameSource {
        GameSource { game_dir: game_dir.to_path_buf(), list: None }
    }
    pub fn list_path(&self) -> PathBuf {
        self.game_dir.join("data").join("cpk_list.cfg.bin")
    }
    fn list(&mut self) -> Result<&(vr_gamefiles::cpk_list::CpkList, HashMap<String, usize>), String> {
        if self.list.is_none() {
            let l = vr_gamefiles::cpk::read_list(&self.list_path()).map_err(|e| format!("cpk_list: {e}"))?;
            let idx = l.path_index();
            self.list = Some((l, idx));
        }
        Ok(self.list.as_ref().expect("set above"))
    }
}

impl Source for GameSource {
    fn keys(&mut self) -> Result<Vec<String>, String> {
        Ok(self.list()?.1.keys().cloned().collect())
    }

    fn read(&mut self, key: &str) -> Result<Read, String> {
        let lp = self.list_path();
        let gd = self.game_dir.clone();
        let loose = gd.join(key);
        let (l, idx) = self.list()?;
        let Some(&i) = idx.get(&key.to_ascii_lowercase()) else {
            return match std::fs::read(&loose) {
                Ok(b) => Ok(Some((b, vec![lp, loose]))),
                Err(_) => Ok(None),
            };
        };
        let it = &l.items[i];
        if it.is_loose() {
            let p = gd.join(it.path());
            return std::fs::read(&p).map(|b| Some((b, vec![lp, p.clone()]))).map_err(|e| format!("{}: {e}", p.display()));
        }
        let cpk = gd.join(it.cpk_path().unwrap_or_default());
        match vr_gamefiles::cpk::extract_from_cpk(&cpk, &it.dir, &it.name) {
            Ok(b) => Ok(Some((b, vec![lp, cpk]))),
            // an all-loose dump (no data/packs): the extracted file itself
            Err(e) => std::fs::read(&loose).map(|b| Some((b, vec![lp, loose.clone()]))).map_err(|_| e.to_string()),
        }
    }
}

/// A base file and where it came from.
#[derive(Debug, Clone)]
pub struct BaseFile {
    pub bytes: Vec<u8>,
    /// For the log: `game`, `mod x (whole file)`.
    pub label: String,
    pub deps: Vec<PathBuf>,
    /// Another mod ships the whole file AND loads after the engine: the overlay serves ITS file, not the engine's.
    pub shadowed_by: Option<String>,
}

/// The whole-file override of `key` in a mod folder.
pub fn mod_file(dir: &Path, key: &str) -> PathBuf {
    let mut p = dir.join("files");
    for part in key.split('/') {
        p.push(part);
    }
    p
}

/// Base of `key` (see the module doc). `self_id` = the engine's mod id, `mods` = active mods in load order.
pub fn base_file(src: &mut dyn Source, mods: &[ModDir], self_id: &str, key: &str) -> Result<Option<BaseFile>, String> {
    let self_index = mods.iter().find(|m| m.id == self_id).map(|m| m.load_index);
    let mut order: Vec<&ModDir> = mods.iter().filter(|m| m.id != self_id).collect();
    order.sort_by_key(|m| m.load_index);
    for m in order.iter().rev() {
        let p = mod_file(&m.dir, key);
        if p.is_file() {
            let bytes = std::fs::read(&p).map_err(|e| format!("{}: {e}", p.display()))?;
            let shadowed_by = self_index.filter(|&s| m.load_index > s).map(|_| m.id.clone());
            return Ok(Some(BaseFile { bytes, label: format!("mod {} (whole file)", m.id), deps: vec![p], shadowed_by }));
        }
    }
    Ok(src.read(key)?.map(|(bytes, deps)| BaseFile { bytes, label: "game".into(), deps, shadowed_by: None }))
}

/// Highest version of a versioned game file: keys `<prefix><digits and dots>.cfg.bin` (e.g. prefix
/// `data/common/gamedata/character/chara_base_` → `…chara_base_1.03.98.00.cfg.bin`, not `chara_base_battle…`).
pub fn latest_versioned(keys: &[String], prefix: &str, suffix: &str) -> Option<String> {
    let mut best: Option<(Vec<u32>, &String)> = None;
    for k in keys {
        let Some(mid) = k.strip_prefix(prefix).and_then(|r| r.strip_suffix(suffix)) else { continue };
        if mid.is_empty() || !mid.chars().all(|c| c.is_ascii_digit() || c == '.') {
            continue;
        }
        let v: Vec<u32> = mid.split('.').map(|p| p.parse().unwrap_or(0)).collect();
        if best.as_ref().is_none_or(|(bv, _)| v > *bv) {
            best = Some((v, k));
        }
    }
    best.map(|(_, k)| k.clone())
}

/// A source over a plain folder (tests, tools): `<root>\<key>`.
pub struct DirSource(pub PathBuf);

impl Source for DirSource {
    fn read(&mut self, key: &str) -> Result<Read, String> {
        let p = self.0.join(key);
        match std::fs::read(&p) {
            Ok(b) => Ok(Some((b, vec![p]))),
            Err(_) => Ok(None),
        }
    }
    fn keys(&mut self) -> Result<Vec<String>, String> {
        let mut out = Vec::new();
        fn walk(dir: &Path, rel: &str, out: &mut Vec<String>) {
            if let Ok(rd) = std::fs::read_dir(dir) {
                for e in rd.flatten() {
                    let n = e.file_name().to_string_lossy().to_ascii_lowercase();
                    let r = if rel.is_empty() { n } else { format!("{rel}/{n}") };
                    if e.path().is_dir() {
                        walk(&e.path(), &r, out);
                    } else {
                        out.push(r);
                    }
                }
            }
        }
        walk(&self.0, "", &mut out);
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versioned_keys() {
        let keys: Vec<String> = [
            "data/common/gamedata/character/chara_base_1.03.98.00.cfg.bin",
            "data/common/gamedata/character/chara_base_1.02.00.00.cfg.bin",
            "data/common/gamedata/character/chara_base_battle_1.09.00.cfg.bin",
            "data/common/gamedata/character/chara_base.cfg.bin",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        assert_eq!(
            latest_versioned(&keys, "data/common/gamedata/character/chara_base_", ".cfg.bin").as_deref(),
            Some("data/common/gamedata/character/chara_base_1.03.98.00.cfg.bin")
        );
        assert_eq!(latest_versioned(&keys, "data/x_", ".cfg.bin"), None);
    }

    #[test]
    fn whole_file_of_the_last_other_mod_is_the_base() {
        let root = std::env::temp_dir().join(format!("evt-te-fw-game-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let key = "data/common/text/en/menu_text.cfg.bin";
        let mk = |id: &str, li: u32, content: Option<&[u8]>| {
            let d = root.join(id);
            if let Some(c) = content {
                let p = mod_file(&d, key);
                std::fs::create_dir_all(p.parent().unwrap()).unwrap();
                std::fs::write(p, c).unwrap();
            }
            ModDir { id: id.into(), dir: d, load_index: li }
        };
        let game = root.join("game");
        let gp = game.join(key);
        std::fs::create_dir_all(gp.parent().unwrap()).unwrap();
        std::fs::write(&gp, b"game").unwrap();
        let mods = vec![mk("a", 0, Some(b"a")), mk("text_engine", 1, Some(b"own output")), mk("b", 2, Some(b"b")), mk("c", 3, None)];
        let mut src = DirSource(game.clone());
        let b = base_file(&mut src, &mods, "text_engine", key).unwrap().unwrap();
        assert_eq!((b.bytes.as_slice(), b.shadowed_by.as_deref()), (&b"b"[..], Some("b")));
        let b = base_file(&mut src, &mods[..2], "text_engine", key).unwrap().unwrap();
        assert_eq!((b.bytes.as_slice(), b.shadowed_by), (&b"a"[..], None));
        let b = base_file(&mut src, &mods[1..2], "text_engine", key).unwrap().unwrap();
        assert_eq!((b.bytes.as_slice(), b.label.as_str()), (&b"game"[..], "game"));
        assert!(base_file(&mut src, &mods[1..2], "text_engine", "data/none.cfg.bin").unwrap().is_none());
        let _ = std::fs::remove_dir_all(&root);
    }
}
