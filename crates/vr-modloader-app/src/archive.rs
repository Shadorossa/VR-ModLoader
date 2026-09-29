//! Mod archives: a `.zip` (read with the `zip` crate, every path checked before it is written) or, best effort, a
//! `.7z` / `.rar` (Windows' own `tar.exe`, libarchive, which refuses `..` and absolute paths). The archive is
//! extracted into a staging folder, the mods in it are found (every shallowest folder with a `mod.toml`) and each
//! one is validated with the `evt-mod check` rules (`evt_modfmt::scan_mod` + a single-mod plan).

use std::io::Read;
use std::path::{Path, PathBuf};

use evt_modfmt::{self as fmt, Manifest, Severity};

/// Refuse archives that would unpack to more than this (a mod with a full voice pack is well below).
pub const MAX_UNPACKED: u64 = 16 << 30;
/// Deepest folder level searched for `mod.toml` below the archive root.
const MAX_DEPTH: usize = 5;

/// One mod folder found in an archive, validated.
#[derive(Debug, Clone)]
pub struct StagedMod {
    /// The mod's folder inside the staging area.
    pub dir: PathBuf,
    /// None = `mod.toml` unreadable (see `errors`).
    pub manifest: Option<Manifest>,
    pub errors: Vec<String>,
    pub warnings: Vec<String>,
    pub files: usize,
    pub bytes: u64,
}

impl StagedMod {
    pub fn ok(&self) -> bool {
        self.manifest.is_some() && self.errors.is_empty()
    }
    pub fn id(&self) -> &str {
        self.manifest.as_ref().map(|m| m.id.as_str()).unwrap_or("?")
    }
}

/// Unpack `archive` into `staging` (must be empty or missing) and return the mods found in it.
pub fn stage(archive: &Path, staging: &Path) -> Result<Vec<StagedMod>, String> {
    std::fs::create_dir_all(staging).map_err(|e| format!("{}: {e}", staging.display()))?;
    let is_zip = archive.extension().is_some_and(|x| x.eq_ignore_ascii_case("zip")) || starts_with_pk(archive);
    if is_zip {
        extract_zip(archive, staging)?;
    } else {
        extract_with_tar(archive, staging)?;
    }
    let dirs = find_mod_dirs(staging);
    if dirs.is_empty() {
        return Err(crate::i18n::tr("No mod.toml found in the archive.").to_string());
    }
    Ok(dirs.iter().map(|d| validate(d)).collect())
}

fn starts_with_pk(p: &Path) -> bool {
    let mut b = [0u8; 4];
    std::fs::File::open(p).and_then(|mut f| f.read_exact(&mut b)).is_ok() && &b == b"PK\x03\x04"
}

/// Extract every file of a zip below `to`. Refused as a whole (nothing written) when an entry is encrypted, a
/// symlink, escapes the folder (`..`, absolute, drive letters) or the total size is too big.
pub fn extract_zip(zip: &Path, to: &Path) -> Result<usize, String> {
    let f = std::fs::File::open(zip).map_err(|e| format!("{}: {e}", zip.display()))?;
    let mut za = zip::ZipArchive::new(std::io::BufReader::new(f)).map_err(|e| format!("{}: {e}", zip.display()))?;
    // 1. check every entry first
    let mut total = 0u64;
    let mut plan: Vec<(usize, PathBuf)> = Vec::new();
    for i in 0..za.len() {
        let e = za.by_index_raw(i).map_err(|e| e.to_string())?;
        let name = e.name().to_string();
        if e.encrypted() {
            return Err(format!("{name}: encrypted entries are not supported"));
        }
        if e.is_symlink() {
            return Err(format!("{name}: symbolic links are not allowed"));
        }
        let rel = safe_rel(&name).ok_or_else(|| format!("{name}: unsafe path in the archive"))?;
        total = total.saturating_add(e.size());
        if total > MAX_UNPACKED {
            return Err(format!("{}: more than {} unpacked", zip.display(), evt_installer::human_size(MAX_UNPACKED)));
        }
        if !e.is_dir() && !rel.as_os_str().is_empty() {
            plan.push((i, rel));
        }
    }
    // 2. write
    for (i, rel) in &plan {
        let mut e = za.by_index(*i).map_err(|e| e.to_string())?;
        let out = to.join(rel);
        if let Some(d) = out.parent() {
            std::fs::create_dir_all(d).map_err(|x| format!("{}: {x}", d.display()))?;
        }
        let mut w = std::fs::File::create(&out).map_err(|x| format!("{}: {x}", out.display()))?;
        // never trust the declared size: copy at most what it declared (+1 to notice a lie)
        let limit = e.size();
        let n = std::io::copy(&mut (&mut e).take(limit + 1), &mut w).map_err(|x| format!("{}: {x}", out.display()))?;
        if n > limit {
            return Err(format!("{}: entry bigger than declared", rel.display()));
        }
    }
    Ok(plan.len())
}

/// A zip entry name as a relative path with only normal components (`\` and `/` both separators). None = unsafe.
pub fn safe_rel(name: &str) -> Option<PathBuf> {
    let mut out = PathBuf::new();
    if name.starts_with(['/', '\\']) {
        return None;
    }
    for part in name.split(['/', '\\']) {
        match part {
            "" | "." => continue,
            ".." => return None,
            p if p.contains(':') || p.chars().any(|c| c.is_control()) => return None,
            p if p.trim_end_matches([' ', '.']).is_empty() => return None,
            p => out.push(p),
        }
    }
    Some(out)
}

fn extract_with_tar(archive: &Path, to: &Path) -> Result<(), String> {
    let mut c = std::process::Command::new("tar");
    c.arg("-xf").arg(archive).arg("-C").arg(to);
    let o = evt_installer::no_window(&mut c).output().map_err(|e| format!("tar: {e}"))?;
    if !o.status.success() {
        return Err(format!(
            "{}: cannot unpack this archive ({}); use a .zip",
            archive.display(),
            String::from_utf8_lossy(&o.stderr).trim()
        ));
    }
    Ok(())
}

/// Every shallowest folder below `root` holding a `mod.toml` (a mod's own sub-folders are not searched), sorted.
pub fn find_mod_dirs(root: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut level = vec![root.to_path_buf()];
    for _ in 0..=MAX_DEPTH {
        let mut next = Vec::new();
        for d in level {
            if d.join(fmt::MANIFEST).is_file() {
                out.push(d);
                continue;
            }
            if let Ok(rd) = std::fs::read_dir(&d) {
                next.extend(rd.flatten().map(|e| e.path()).filter(|p| p.is_dir()));
            }
        }
        if next.is_empty() {
            break;
        }
        next.sort();
        level = next;
    }
    out.sort();
    out
}

/// `evt-mod check` of one mod folder: the scan issues plus the single-mod plan's (missing plugin, invalid deltas…).
pub fn validate(dir: &Path) -> StagedMod {
    let (m, issues) = fmt::scan_mod(dir);
    let (files, bytes) = evt_installer::version::folder_size(dir);
    let mut errors = Vec::new();
    let mut warnings = Vec::new();
    let mut push = |i: &fmt::Issue| {
        let text = match &i.path {
            Some(p) => format!("{}: {}", p.strip_prefix(dir).unwrap_or(p).display(), i.msg),
            None => i.msg.clone(),
        };
        match i.severity {
            Severity::Error => errors.push(text),
            Severity::Warning => warnings.push(text),
        }
    };
    issues.iter().for_each(&mut push);
    let manifest = m.map(|m| {
        let manifest = m.manifest.clone();
        let plan = fmt::build_plan(vec![m], None);
        plan.issues.iter().for_each(&mut push);
        manifest
    });
    if manifest.is_none() && errors.is_empty() {
        errors.push(format!("{}: invalid", fmt::MANIFEST));
    }
    StagedMod { dir: dir.to_path_buf(), manifest, errors, warnings, files, bytes }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::io::Write;

    /// Write a zip with `(name, content)` entries (stored = no compression, and deflated when `deflate`).
    pub fn make_zip(path: &Path, entries: &[(&str, &str)], deflate: bool) {
        let f = std::fs::File::create(path).unwrap();
        let mut z = zip::ZipWriter::new(f);
        let method = if deflate { zip::CompressionMethod::Deflated } else { zip::CompressionMethod::Stored };
        let opts = zip::write::SimpleFileOptions::default().compression_method(method);
        for (name, content) in entries {
            if name.ends_with('/') {
                z.add_directory(*name, opts).unwrap();
            } else {
                z.start_file(*name, opts).unwrap();
                z.write_all(content.as_bytes()).unwrap();
            }
        }
        z.finish().unwrap();
    }

    const GOOD: &str = "id = \"good_mod\"\nname = \"Good\"\nversion = \"1.2.0\"\nauthor = \"me\"\n";

    #[test]
    fn safe_paths() {
        assert_eq!(safe_rel("a/b\\c.txt"), Some(PathBuf::from("a").join("b").join("c.txt")));
        assert_eq!(safe_rel("./a/./b"), Some(PathBuf::from("a").join("b")));
        for bad in ["../x", "a/../../x", "/abs", "\\abs", "C:/x", "a/c:x", "a/ /b", "a/../b"] {
            assert_eq!(safe_rel(bad), None, "{bad}");
        }
    }

    #[test]
    fn zip_with_nested_mod_folder() {
        let d = crate::temp_dir("arch_nested").unwrap();
        let z = d.join("m.zip");
        make_zip(
            &z,
            &[
                ("GoodMod v1.2/", ""),
                ("GoodMod v1.2/good_mod/mod.toml", GOOD),
                ("GoodMod v1.2/good_mod/files/data/common/x.bin", "x"),
                ("GoodMod v1.2/good_mod/lua/title_menu_2/10_a.lua", "print(1)"),
                ("GoodMod v1.2/readme.txt", "hi"),
            ],
            true,
        );
        let st = stage(&z, &d.join("stage")).unwrap();
        assert_eq!(st.len(), 1);
        assert!(st[0].ok(), "{:?}", st[0].errors);
        assert_eq!(st[0].id(), "good_mod");
        assert!(st[0].dir.ends_with("good_mod"));
        assert_eq!(st[0].files, 3);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn zip_with_two_mods_and_a_bad_one() {
        let d = crate::temp_dir("arch_multi").unwrap();
        let z = d.join("m.zip");
        make_zip(
            &z,
            &[
                ("lib/mod.toml", "id = \"lib\"\nname = \"Lib\"\nversion = \"1.0\"\n"),
                ("main/mod.toml", "id = \"main\"\nname = \"Main\"\nversion = \"1.0\"\nrequires = [\"lib>=1.0\"]\n"),
                ("broken/mod.toml", "id = \"Broken Id\"\nversion = \"1\"\n"),
                ("plug/mod.toml", "id = \"plug\"\nname = \"P\"\nversion = \"1.0\"\nplugin = \"plug.dll\"\n"),
            ],
            false,
        );
        let st = stage(&z, &d.join("stage")).unwrap();
        let ids: Vec<&str> = st.iter().map(|s| s.id()).collect();
        assert_eq!(ids, vec!["?", "lib", "main", "plug"]);
        assert!(!st[0].ok());
        assert!(st[1].ok() && st[2].ok());
        // the plugin DLL named in mod.toml is missing: evt-mod check refuses it
        assert!(!st[3].ok(), "{:?}", st[3]);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn zip_refusals() {
        let d = crate::temp_dir("arch_bad").unwrap();
        let z = d.join("evil.zip");
        make_zip(&z, &[("good_mod/mod.toml", GOOD), ("../../evil.txt", "x")], false);
        let e = stage(&z, &d.join("stage")).unwrap_err();
        assert!(e.contains("unsafe path"), "{e}");
        assert!(!d.join("evil.txt").exists() && !d.parent().unwrap().join("evil.txt").exists());
        // nothing was written for a refused archive
        assert!(std::fs::read_dir(d.join("stage")).unwrap().next().is_none());
        let z2 = d.join("empty.zip");
        make_zip(&z2, &[("readme.txt", "no mod here")], false);
        assert!(stage(&z2, &d.join("stage2")).is_err());
        let z3 = d.join("notzip.zip");
        std::fs::write(&z3, "not a zip").unwrap();
        assert!(stage(&z3, &d.join("stage3")).is_err());
        let _ = std::fs::remove_dir_all(&d);
    }
}
