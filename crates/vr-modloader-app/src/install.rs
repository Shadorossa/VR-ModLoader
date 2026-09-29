//! Installing and uninstalling mods in `<game>\mods`.
//!
//! * Install: the archive is staged in `mods\_vrml_staging\<n>\` (a `_` folder: the ModLoader and its Mods menu skip
//!   it, so a game start during an install never sees a half mod), each valid mod is compared with the installed one
//!   (new / upgrade / same / downgrade; the UI asks), the installed copy goes to the trash and the staged folder is
//!   renamed to `mods\<id>\` (same drive: instant, never half-copied).
//! * Uninstall = the folder is moved to `mods\_trash\<id>_<version>_<stamp>\` (also skipped by the loader). Only
//!   «Empty trash» deletes, after a confirmation.

use std::path::{Path, PathBuf};

use evt_installer::modpack::{satisfies, Avail};
use evt_modfmt::{self as fmt, compare_versions};

use crate::archive::{self, StagedMod};

pub const STAGING_DIR: &str = "_vrml_staging";
pub const TRASH_DIR: &str = "_trash";

#[derive(Debug, Clone, PartialEq)]
pub enum Existing {
    New,
    Upgrade(String),
    Same(String),
    Downgrade(String),
}

/// One mod of an archive, ready to be committed.
#[derive(Debug, Clone)]
pub struct Candidate {
    pub staged: StagedMod,
    pub existing: Existing,
    /// Install it (the UI unticks the invalid ones and lets the player untick the others).
    pub selected: bool,
}

/// An archive staged in the mods folder.
#[derive(Debug)]
pub struct Prepared {
    pub archive: PathBuf,
    pub staging: PathBuf,
    pub candidates: Vec<Candidate>,
}

impl Prepared {
    /// Forget the staging folder (whatever was not committed).
    pub fn discard(&self) {
        let _ = std::fs::remove_dir_all(&self.staging);
        let parent = self.staging.parent().map(Path::to_path_buf);
        if let Some(p) = parent {
            let _ = std::fs::remove_dir(p); // only when empty
        }
    }
}

fn installed_version(mods_root: &Path, id: &str) -> Option<String> {
    let t = std::fs::read_to_string(mods_root.join(id).join(fmt::MANIFEST)).ok()?;
    fmt::parse_manifest(&t).ok().map(|(m, _)| m.version).or_else(|| Some(String::new()))
}

/// Stage `archive` into `<mods_root>\_vrml_staging\` and compare its mods with the installed ones.
pub fn prepare(mods_root: &Path, archive: &Path) -> Result<Prepared, String> {
    let base = mods_root.join(STAGING_DIR);
    let staging = (0..1000).map(|n| base.join(format!("{}_{n}", evt_installer::now_secs()))).find(|p| !p.exists()).ok_or("staging")?;
    let staged = match archive::stage(archive, &staging) {
        Ok(s) => s,
        Err(e) => {
            let _ = std::fs::remove_dir_all(&staging);
            let _ = std::fs::remove_dir(&base);
            return Err(e);
        }
    };
    let mut candidates = Vec::new();
    for s in staged {
        let existing = match s.manifest.as_ref().and_then(|m| installed_version(mods_root, &m.id).map(|v| (m, v))) {
            None => Existing::New,
            Some((m, v)) => match compare_versions(&m.version, &v) {
                std::cmp::Ordering::Greater => Existing::Upgrade(v),
                std::cmp::Ordering::Equal => Existing::Same(v),
                std::cmp::Ordering::Less => Existing::Downgrade(v),
            },
        };
        // two mods with one id in the same archive: only the first can be selected
        let dup = candidates.iter().any(|c: &Candidate| c.staged.manifest.is_some() && c.staged.id() == s.id());
        let selected = s.ok() && !dup && !matches!(existing, Existing::Same(_) | Existing::Downgrade(_));
        candidates.push(Candidate { staged: s, existing, selected });
    }
    Ok(Prepared { archive: archive.to_path_buf(), staging, candidates })
}

/// Install the selected candidates (installed copies go to the trash first). Returns `id version` of each one.
pub fn commit(mods_root: &Path, prep: &Prepared) -> Result<Vec<String>, String> {
    let mut done = Vec::new();
    for c in prep.candidates.iter().filter(|c| c.selected) {
        let Some(m) = c.staged.manifest.as_ref().filter(|_| c.staged.ok()) else { continue };
        let target = mods_root.join(&m.id);
        if target.exists() {
            move_to_trash(mods_root, &m.id)?;
        }
        std::fs::rename(&c.staged.dir, &target).map_err(|e| format!("{} → {}: {e}", c.staged.dir.display(), target.display()))?;
        done.push(format!("{} {}", m.id, m.version));
    }
    prep.discard();
    Ok(done)
}

/// Requirements of the selected candidates that neither the installed mods nor the other selected candidates meet:
/// `(id, constraint, needed by)`.
pub fn missing_after(mods_root: &Path, prep: &Prepared) -> Vec<(String, String, Vec<String>)> {
    let mut avail: Vec<Avail> = evt_installer::modops::mods_on_disk(mods_root.parent().unwrap_or(mods_root));
    for c in prep.candidates.iter().filter(|c| c.selected) {
        if let Some(m) = &c.staged.manifest {
            avail.retain(|a| a.id != m.id);
            avail.push(Avail { id: m.id.clone(), version: m.version.clone(), provides: m.provides.clone() });
        }
    }
    let mut out: Vec<(String, String, Vec<String>)> = Vec::new();
    for c in prep.candidates.iter().filter(|c| c.selected) {
        let Some(m) = &c.staged.manifest else { continue };
        for r in m.requires.iter().filter_map(|r| fmt::parse_requirement(r).ok()) {
            if avail.iter().any(|a| satisfies(a, &r)) {
                continue;
            }
            match out.iter_mut().find(|(id, _, _)| *id == r.id) {
                Some(e) => e.2.push(m.id.clone()),
                None => out.push((r.id.clone(), r.constraint(), vec![m.id.clone()])),
            }
        }
    }
    out
}

pub fn trash_root(mods_root: &Path) -> PathBuf {
    mods_root.join(TRASH_DIR)
}

/// Move `mods\<id>` to `mods\_trash\<id>_<version>_<stamp>` and return the new place.
pub fn move_to_trash(mods_root: &Path, id: &str) -> Result<PathBuf, String> {
    fmt::validate_id(id)?;
    let src = mods_root.join(id);
    if !src.is_dir() {
        return Err(format!("{}: not installed", src.display()));
    }
    let ver = installed_version(mods_root, id).unwrap_or_default();
    let ver: String = ver.chars().map(|c| if c.is_ascii_alphanumeric() || ".-+".contains(c) { c } else { '_' }).collect();
    let trash = trash_root(mods_root);
    std::fs::create_dir_all(&trash).map_err(|e| format!("{}: {e}", trash.display()))?;
    let stamp = evt_installer::stamp(evt_installer::now_secs());
    let dest = (0..100)
        .map(|n| trash.join(if n == 0 { format!("{id}_{ver}_{stamp}") } else { format!("{id}_{ver}_{stamp}_{n}") }))
        .find(|p| !p.exists())
        .ok_or("trash: name")?;
    std::fs::rename(&src, &dest).map_err(|e| format!("{} → {}: {e}", src.display(), dest.display()))?;
    Ok(dest)
}

/// Items in the trash.
pub fn trash_items(mods_root: &Path) -> usize {
    std::fs::read_dir(trash_root(mods_root)).map(|rd| rd.count()).unwrap_or(0)
}

/// Permanently delete the trash (the UI asks first).
pub fn empty_trash(mods_root: &Path) -> Result<(), String> {
    let t = trash_root(mods_root);
    if t.exists() {
        std::fs::remove_dir_all(&t).map_err(|e| format!("{}: {e}", t.display()))?;
    }
    Ok(())
}

/// Leftover staging folders of an interrupted install (removed at start).
pub fn clean_staging(mods_root: &Path) {
    let _ = std::fs::remove_dir_all(mods_root.join(STAGING_DIR));
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::archive::tests::make_zip;

    fn game(name: &str) -> (PathBuf, PathBuf) {
        let d = crate::temp_dir(name).unwrap();
        let mods = d.join("mods");
        std::fs::create_dir_all(&mods).unwrap();
        (d, mods)
    }

    fn manifest(id: &str, ver: &str, extra: &str) -> String {
        format!("id = \"{id}\"\nname = \"{id}\"\nversion = \"{ver}\"\n{extra}")
    }

    #[test]
    fn install_upgrade_uninstall() {
        let (d, mods) = game("inst_flow");
        let z1 = d.join("v1.zip");
        let m1 = manifest("cool", "1.0.0", "requires = [\"lib>=1.0\"]\n");
        make_zip(&z1, &[("cool/mod.toml", &m1), ("cool/files/data/common/a.bin", "v1")], true);
        // v1: new, missing lib
        let p = prepare(&mods, &z1).unwrap();
        assert_eq!(p.candidates.len(), 1);
        assert_eq!(p.candidates[0].existing, Existing::New);
        assert!(p.candidates[0].selected);
        assert_eq!(missing_after(&mods, &p), vec![("lib".to_string(), ">= 1.0".to_string(), vec!["cool".to_string()])]);
        // the staging folder is invisible to the loader
        assert!(evt_modfmt::scan_root(&mods).0.is_empty());
        assert_eq!(commit(&mods, &p).unwrap(), vec!["cool 1.0.0"]);
        assert!(!mods.join(STAGING_DIR).exists());
        assert_eq!(std::fs::read_to_string(mods.join("cool/files/data/common/a.bin")).unwrap(), "v1");
        // v2 of the same mod plus its library in one archive: upgrade, nothing missing
        let z2 = d.join("v2.zip");
        let m2 = manifest("cool", "1.1.0", "requires = [\"lib>=1.0\"]\n");
        let ml = manifest("lib", "1.0.0", "");
        make_zip(&z2, &[("cool/mod.toml", &m2), ("cool/files/data/common/a.bin", "v2"), ("lib/mod.toml", &ml)], false);
        let p = prepare(&mods, &z2).unwrap();
        let cool = p.candidates.iter().find(|c| c.staged.id() == "cool").unwrap();
        assert_eq!(cool.existing, Existing::Upgrade("1.0.0".into()));
        assert!(missing_after(&mods, &p).is_empty());
        commit(&mods, &p).unwrap();
        assert_eq!(std::fs::read_to_string(mods.join("cool/files/data/common/a.bin")).unwrap(), "v2");
        // the old copy is in the trash, not deleted; the loader does not see it
        assert_eq!(trash_items(&mods), 1);
        let (seen, _) = evt_modfmt::scan_root(&mods);
        assert_eq!(seen.iter().map(|m| m.label()).collect::<Vec<_>>(), vec!["cool@1.1.0", "lib@1.0.0"]);
        // same version again: not selected by default
        let p = prepare(&mods, &z2).unwrap();
        assert!(p.candidates.iter().all(|c| matches!(c.existing, Existing::Same(_)) && !c.selected));
        p.discard();
        // downgrade: not selected by default
        let p = prepare(&mods, &z1).unwrap();
        assert_eq!(p.candidates[0].existing, Existing::Downgrade("1.1.0".into()));
        assert!(!p.candidates[0].selected);
        p.discard();
        assert!(!mods.join(STAGING_DIR).exists());
        // uninstall = to the trash
        let t = move_to_trash(&mods, "lib").unwrap();
        assert!(t.join("mod.toml").is_file() && !mods.join("lib").exists());
        assert!(t.file_name().unwrap().to_string_lossy().starts_with("lib_1.0.0_"));
        assert_eq!(trash_items(&mods), 2);
        assert!(move_to_trash(&mods, "lib").is_err());
        assert!(move_to_trash(&mods, "../x").is_err());
        empty_trash(&mods).unwrap();
        assert_eq!(trash_items(&mods), 0);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn invalid_mods_are_never_selected() {
        let (d, mods) = game("inst_invalid");
        let z = d.join("bad.zip");
        make_zip(&z, &[("x/mod.toml", "id = \"x\"\n")], false); // no version
        let p = prepare(&mods, &z).unwrap();
        assert!(!p.candidates[0].selected);
        assert!(commit(&mods, &p).unwrap().is_empty());
        assert!(!mods.join("x").exists());
        let _ = std::fs::remove_dir_all(&d);
    }
}
