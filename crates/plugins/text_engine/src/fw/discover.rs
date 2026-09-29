//! Per-mod declaration files of an engine: `<mod>\<name>.toml` (one file) and/or `<mod>\<name>\*.toml` (one file per
//! part, e.g. per language). Generic: the engine parses the texts; this only finds and reads them, in a stable order.

use std::path::Path;

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

/// Installed mod folders under `mods_dir` (not starting with `.` / `_`) that hold a `mod.toml`: `(folder name, path)`,
/// sorted. The id is the folder name (evt-modfmt warns when they differ).
pub fn installed(mods_dir: &Path) -> Vec<(String, std::path::PathBuf)> {
    let mut v: Vec<(String, std::path::PathBuf)> = std::fs::read_dir(mods_dir)
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
