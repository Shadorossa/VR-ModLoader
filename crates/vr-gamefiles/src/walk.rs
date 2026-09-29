//! The `data/**` files of a folder (a project, a mod) as logical game paths (`data/...`).

use std::path::Path;

use crate::error::{IoContext, Result};

/// Files under `<root>/data/**` as logical paths (`data/...`), skipping our own metadata.
pub fn walk_files(root: &Path) -> Result<Vec<(String, u64)>> {
    Ok(walk_files_meta(root)?.into_iter().map(|(p, s, _)| (p, s)).collect())
}

/// [`walk_files`] with the modification time (ns since the epoch) of every file, for cache keys.
pub fn walk_files_meta(root: &Path) -> Result<Vec<(String, u64, u128)>> {
    let mut out = Vec::new();
    let data = root.join("data");
    if !data.is_dir() {
        return Ok(out);
    }
    let mut stack = vec![data];
    while let Some(dir) = stack.pop() {
        for e in std::fs::read_dir(&dir).at(&dir)? {
            let e = e.at(&dir)?;
            let p = e.path();
            let ft = e.file_type().at(&p)?;
            if ft.is_dir() {
                stack.push(p);
            } else if ft.is_file() {
                let rel = p.strip_prefix(root).unwrap().to_string_lossy().replace('\\', "/");
                if is_junk(&rel) {
                    continue;
                }
                let m = e.metadata().at(&p)?;
                let mtime = m.modified().ok().and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok()).map(|d| d.as_nanos()).unwrap_or(0);
                out.push((rel, m.len(), mtime));
            }
        }
    }
    out.sort();
    Ok(out)
}

/// Files that must never become `cpk_list` entries (see docs/formats/cpk_list.md §8.G).
pub fn is_junk(path: &str) -> bool {
    let name = path.rfind('/').map_or(path, |i| &path[i + 1..]).to_lowercase();
    name == "thumbs.db"
        || name == ".ds_store"
        || name == "desktop.ini"
        || name.ends_with(".log")
        || name.ends_with(".off")
        || name.ends_with(".bak")
        || name.ends_with(".json")
        || name.ends_with(".tmp")
        || name.ends_with("cpk_list.cfg.bin")
}
