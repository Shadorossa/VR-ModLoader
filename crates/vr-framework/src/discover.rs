//! Finding the data of an engine in the mod folders. Generic: the engine parses the texts; this only finds and reads
//! them, in a stable order.
//!
//! * [`read`] / [`has`]: `<mod>\<name>.toml` (one file) and/or `<mod>\<name>\*.toml` (one file per part).
//! * [`toml_files`]: `<dir>\*.toml` sorted by file name, `_*.toml` skipped (drafts / shared parts).
//! * [`mods_with`]: the mods (load order) that have one given file.
//! * [`installed`]: every installed mod folder (active or not).
//! * [`uses_engine`]: does a mod use an engine (`requires` / `provides` in its `mod.toml`)?

use crate::ModDir;
use std::path::{Path, PathBuf};

/// One declaration file: path relative to the mod folder (`/` separators) and its text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeclFile {
    pub rel: String,
    pub text: String,
}

/// `<dir>\<name>.toml` first, then `<dir>\<name>\*.toml` by file name (case-insensitive `.toml`). Unreadable files
/// come back with an error text instead (the engine reports them).
pub fn read(dir: &Path, name: &str) -> Vec<Result<DeclFile, String>> {
    let mut out = Vec::new();
    let one = dir.join(format!("{name}.toml"));
    if one.is_file() {
        out.push(std::fs::read_to_string(&one).map(|text| DeclFile { rel: format!("{name}.toml"), text }).map_err(|e| format!("{name}.toml: {e}")));
    }
    if let Ok(rd) = std::fs::read_dir(dir.join(name)) {
        let mut files: Vec<_> = rd
            .flatten()
            .filter(|e| e.path().is_file() && e.file_name().to_string_lossy().to_ascii_lowercase().ends_with(".toml"))
            .collect();
        files.sort_by_key(|e| e.file_name().to_string_lossy().to_ascii_lowercase());
        for e in files {
            let rel = format!("{name}/{}", e.file_name().to_string_lossy());
            out.push(std::fs::read_to_string(e.path()).map(|text| DeclFile { rel: rel.clone(), text }).map_err(|err| format!("{rel}: {err}")));
        }
    }
    out
}

/// Does the mod have any declaration of this engine?
pub fn has(dir: &Path, name: &str) -> bool {
    dir.join(format!("{name}.toml")).is_file() || dir.join(name).is_dir()
}

/// `<dir>\*.toml` (case-insensitive extension) sorted by path, without the files whose stem starts with `_`. Missing
/// folder = none.
pub fn toml_files(dir: &Path) -> Vec<PathBuf> {
    let Ok(rd) = std::fs::read_dir(dir) else { return Vec::new() };
    let mut files: Vec<PathBuf> = rd
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.is_file() && p.extension().is_some_and(|x| x.eq_ignore_ascii_case("toml")))
        .filter(|p| !p.file_stem().is_some_and(|s| s.to_string_lossy().starts_with('_')))
        .collect();
    files.sort();
    files
}

/// The mods (load order) that have `rel` in their folder, with its text.
pub fn mods_with(mods: &[ModDir], rel: &str) -> Vec<(ModDir, String)> {
    crate::in_load_order(mods).into_iter().filter_map(|m| std::fs::read_to_string(m.dir.join(rel)).ok().map(|t| (m.clone(), t))).collect()
}

/// Installed mod folders under `mods_dir` (not starting with `.` / `_`) that hold a `mod.toml`: `(folder name, path)`,
/// sorted. The id is the folder name (evt-modfmt warns when they differ).
pub fn installed(mods_dir: &Path) -> Vec<(String, PathBuf)> {
    let mut v: Vec<(String, PathBuf)> = std::fs::read_dir(mods_dir)
        .map(|rd| {
            rd.flatten()
                .filter(|e| e.path().is_dir())
                .map(|e| (e.file_name().to_string_lossy().into_owned(), e.path()))
                .filter(|(n, p)| !n.starts_with('.') && !n.starts_with('_') && p.join("mod.toml").is_file())
                .collect()
        })
        .unwrap_or_default();
    v.sort();
    v
}

/// The name part of a dependency entry: `"match_engine>=1.0"` → `match_engine`.
pub fn dep_name(entry: &str) -> &str {
    entry.split(['<', '>', '=', ' ']).next().unwrap_or("").trim()
}

/// Does the mod `id` with `mod.toml` text `manifest` use the engine `engine_id` (it is the engine's own mod
/// `own_id`, or its `requires` / `provides` names `engine_id` or `own_id`, with or without a version)?
pub fn uses_engine(engine_id: &str, own_id: &str, id: &str, manifest: &str) -> bool {
    if id == own_id {
        return true;
    }
    #[derive(serde::Deserialize, Default)]
    #[serde(default)]
    struct M {
        requires: Vec<String>,
        provides: Vec<String>,
    }
    let Ok(m) = toml::from_str::<M>(manifest) else { return false };
    let named = |s: &String| {
        let name = dep_name(s);
        name == engine_id || name == own_id
    };
    m.requires.iter().any(named) || m.provides.iter().any(named)
}

/// [`uses_engine`] with the mod's `mod.toml` read from its folder.
pub fn mod_uses_engine(engine_id: &str, own_id: &str, m: &ModDir) -> bool {
    let manifest = std::fs::read_to_string(m.dir.join("mod.toml")).unwrap_or_default();
    uses_engine(engine_id, own_id, &m.id, &manifest)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(p: &Path, b: &str) {
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, b).unwrap();
    }

    #[test]
    fn declaration_files_and_folders() {
        let d = std::env::temp_dir().join(format!("vr-fw-discover-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        let m = d.join("mods").join("a");
        write(&m.join("mod.toml"), "id = \"a\"\n");
        write(&m.join("text.toml"), "x = 1");
        write(&m.join("text").join("B.TOML"), "b = 1");
        write(&m.join("text").join("a.toml"), "a = 1");
        write(&m.join("text").join("notes.txt"), "-");
        write(&m.join("text").join("_draft.toml"), "-");
        let r: Vec<String> = read(&m, "text").into_iter().map(|f| f.unwrap().rel).collect();
        assert_eq!(r, ["text.toml", "text/_draft.toml", "text/a.toml", "text/B.TOML"]);
        assert!(has(&m, "text") && !has(&m, "audio"));
        let names: Vec<String> = toml_files(&m.join("text")).iter().map(|p| p.file_name().unwrap().to_string_lossy().into_owned()).collect();
        assert_eq!(names, ["B.TOML", "a.toml"]);
        std::fs::create_dir_all(d.join("mods").join("_local")).unwrap();
        std::fs::create_dir_all(d.join("mods").join("nomanifest")).unwrap();
        assert_eq!(installed(&d.join("mods")).iter().map(|(n, _)| n.as_str()).collect::<Vec<_>>(), ["a"]);
        let mods = vec![ModDir::new("b", d.join("mods").join("b"), 1), ModDir::new("a", m.clone(), 0)];
        assert_eq!(mods_with(&mods, "text.toml").iter().map(|(m, t)| (m.id.as_str(), t.as_str())).collect::<Vec<_>>(), [("a", "x = 1")]);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn engine_users() {
        assert!(uses_engine("match_engine", "match_engine", "match_engine", ""));
        assert!(uses_engine("match_engine", "match_engine", "x", "requires = [\"match_engine>=1.0\"]"));
        assert!(uses_engine("match_engine", "my_engine", "x", "requires = [\"my_engine\"]"));
        assert!(uses_engine("match_engine", "match_engine", "x", "provides = [\"match_engine = 1.0\"]"));
        assert!(!uses_engine("match_engine", "match_engine", "x", "requires = [\"match_engine_extra\"]"));
        assert!(!uses_engine("match_engine", "match_engine", "x", "requires = 3"));
        assert_eq!(dep_name("vr_framework>=1.0"), "vr_framework");
    }
}
