//! Install the project overlay into the game as loose files and patch `cpk_list`; undo it exactly.
//!
//! Procedure: `docs/formats/cpk_list.md` §5 and §8, `docs/game/engine/mod-loading.md`,
//! `docs/Finalizado/01-install-pipeline.md` (incremental install).
//!
//! Backups live outside the project, in a per-game folder chosen by the caller (the app uses
//! `%APPDATA%\com.expandedvictory.tool\backups\<game id>`), so they survive project changes.
//! Restore only undoes what we did:
//! - each installed file goes back to its previous content (or is deleted if it did not exist),
//!   but only if it still is the file we installed — anything changed since is left alone and reported;
//! - `cpk_list` is not replaced wholesale: only the entries we modified get their original fields back
//!   and the entries we added are removed, so later changes by other tools survive.
//!
//! Installs are incremental: the project is diffed against the previous manifest and only new, changed and removed
//! files are touched (unchanged ones are skipped; the backup of a retail original is never replaced by one of our
//! files). The result — game files, `cpk_list`, backup and manifest — is the same as the full path (restore the
//! previous install, then install everything), which still runs when there is no usable manifest, the project or game
//! folder changed, or the caller asks for it ([`InstallOptions::full`]).
//!
//! Scope: only `data/**` of the project is ever installed (`walk_files`), and only `data/**` is ever restored.
//! The loader (`winmm.dll`), its data folder (`evt_loader/`, incl. `lua_patches/**` and `_fingerprints.json`) and
//! anything else in the game root are **never** written, removed or "cleaned" by the project install — even if an
//! older manifest lists such a path (2026-09-27: a winmm.dll downgrade + a lost `_fingerprints.json` were traced to
//! the Loader card picking a stale build, not to this module; the guard makes the invariant explicit).

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::{Instant, UNIX_EPOCH};

use rayon::prelude::*;
use serde::{Deserialize, Serialize};

use crate::cpk;
use crate::cpk_list::{CpkItem, CpkList};
use crate::error::{Error, IoContext, Result};
use crate::timing::phase;

const MANIFEST: &str = "manifest.json";
/// Manifest written by this version. Older manifests are still restored exactly, but the next install is a full one.
const MANIFEST_VERSION: u32 = 3;
/// Persistent CRC-32 cache (project + game files) next to the manifest; deleted with the backup on restore.
const CRC_CACHE_FILE: &str = "crc_cache.json";
/// A file modified less than this long before it is hashed is not cached: a same-size rewrite within the file
/// system's timestamp granularity would keep its (size, mtime) key ("racy" mtime, as in git).
const RACY_NS: u64 = 2_000_000_000;

/// The only tree the install may write or restore. Everything else (`winmm.dll`, `evt_loader/**`, `mods/**`) is
/// owned by the loader / the user and is off limits for `install` and `restore`.
pub fn in_install_scope(rel: &str) -> bool {
    let r = rel.replace('\\', "/");
    let r = r.trim_start_matches("./");
    let lower = r.to_ascii_lowercase();
    lower.starts_with("data/") && !lower.contains("/../") && !lower.ends_with("cpk_list.cfg.bin")
}
const LEGACY_DIR: &str = ".evt_backup";

/// Original `cpk_list` fields of an entry we changed.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct EntrySnapshot {
    cpk_dir: Option<String>,
    cpk_name: Option<String>,
    size: i32,
}

impl EntrySnapshot {
    fn of(it: &CpkItem) -> Self {
        Self { cpk_dir: it.cpk_dir.clone(), cpk_name: it.cpk_name.clone(), size: it.size }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct FileRecord {
    path: String,
    /// The game had this file before the install; its copy is in `files/<path>` of the backup.
    existed: bool,
    installed_size: u64,
    installed_crc: u32,
    /// `None` = the entry was added by us.
    original_entry: Option<EntrySnapshot>,
    /// Other (size, CRC-32) that also count as ours: the previous version of a changed file while an incremental
    /// install is copying, so an interrupted install still restores exactly. Empty in a finished install.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    also_ours: Vec<(u64, u32)>,
}

impl FileRecord {
    /// This game file content is one we installed.
    fn is_ours(&self, c: (u64, u32)) -> bool {
        c == (self.installed_size, self.installed_crc) || self.also_ours.contains(&c)
    }

    /// This `cpk_list` entry is still the loose entry we wrote.
    fn entry_is_ours(&self, it: &CpkItem) -> bool {
        let size = it.size as u64;
        it.is_loose() && (size == self.installed_size || self.also_ours.iter().any(|a| a.0 == size))
    }
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Manifest {
    version: u32,
    game_dir: String,
    project_dir: String,
    installed_at: u64,
    files: Vec<FileRecord>,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct InstallOptions {
    /// Restore the previous install and install everything again (the pre-incremental behaviour).
    pub full: bool,
}

#[derive(Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InstallReport {
    /// Files copied into the game by this run.
    pub written: usize,
    /// `cpk_list` entries added / changed by this run.
    pub new_entries: usize,
    pub modified_entries: usize,
    pub backup_dir: String,
    pub warnings: Vec<String>,
    /// Everything was restored and installed again (see `full_reason`); `false` = incremental.
    pub full: bool,
    /// Why the install was full; empty for an incremental one.
    pub full_reason: String,
    /// Project files installed after this run.
    pub files: usize,
    /// Files already installed and unchanged: not restored, backed up or copied.
    pub skipped: usize,
    /// Game files put back to their state before our install (files no longer in the project; in a full install,
    /// every file of the previous install).
    pub restored: usize,
    /// Game files copied into the backup by this run.
    pub backed_up: usize,
    pub elapsed_ms: u64,
    /// This run rewrote `data/cpk_list.cfg.bin` (an index built over it must re-read it).
    #[serde(skip)]
    pub list_written: bool,
}

#[derive(Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RestoreReport {
    pub restored: usize,
    pub removed: usize,
    pub list_entries: usize,
    /// Files or entries left untouched because something else changed them after our install.
    pub skipped: Vec<String>,
}

/// What is currently installed by us in the game, for the UI.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InstalledState {
    pub installed_at: u64,
    pub project_dir: String,
    pub backup_dir: String,
    pub files: Vec<InstalledFile>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InstalledFile {
    pub path: String,
    /// `replaced` (the game had it) or `new`.
    pub kind: &'static str,
    /// Still the file we installed.
    pub intact: bool,
}

fn copy(from: &Path, to: &Path) -> Result<()> {
    if let Some(d) = to.parent() {
        std::fs::create_dir_all(d).at(d)?;
    }
    std::fs::copy(from, to).at(from)?;
    Ok(())
}

/// [`copy`] of many files, in parallel (thousands of small files: the per-file overhead dominates).
fn par_copy(jobs: &[(PathBuf, PathBuf)]) -> Result<()> {
    jobs.par_iter().try_for_each(|(from, to)| copy(from, to))
}

fn file_crc(p: &Path) -> Option<(u64, u32)> {
    let b = std::fs::read(p).ok()?;
    Some((b.len() as u64, crc32fast::hash(&b)))
}

fn mtime_ns(m: &std::fs::Metadata) -> Option<u64> {
    m.modified().ok()?.duration_since(UNIX_EPOCH).ok().map(|d| d.as_nanos() as u64).filter(|&t| t != 0)
}

fn now_ns() -> u64 {
    std::time::SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_nanos() as u64)
}

fn cacheable(mtime: u64) -> bool {
    now_ns().saturating_sub(mtime) >= RACY_NS
}

/// (size, mtime ns, CRC-32).
type CrcEntry = (u64, u64, u32);

/// CRC-32 of files keyed by (size, mtime). `installed_state` runs every time the Proyecto page refreshes, and
/// re-reading every installed file (~470 MB for a 1.100-file project) took 0.3-1 s each time; with the cache it is
/// one `metadata` per file (~30-50 ms). A file rewritten by another program gets a new mtime and is read again.
/// `restore` keeps reading the bytes (it decides what to delete). [`DiskCrcCache`] persists it across runs.
static CRC_CACHE: OnceLock<Mutex<HashMap<PathBuf, CrcEntry>>> = OnceLock::new();

fn crc_cache() -> std::sync::MutexGuard<'static, HashMap<PathBuf, CrcEntry>> {
    CRC_CACHE.get_or_init(Default::default).lock().unwrap_or_else(|e| e.into_inner())
}

/// [`file_crc`] through [`CRC_CACHE`].
fn cached_crc(p: &Path) -> Option<(u64, u32)> {
    let m = std::fs::metadata(p).ok()?;
    let Some(mt) = mtime_ns(&m) else { return file_crc(p) };
    if let Some(&(len, t, crc)) = crc_cache().get(p) {
        if len == m.len() && t == mt {
            return Some((len, crc));
        }
    }
    let (len, crc) = file_crc(p)?;
    if len == m.len() && cacheable(mt) {
        crc_cache().insert(p.to_path_buf(), (len, mt, crc));
    }
    Some((len, crc))
}

/// The CRC cache of one install, stored as `crc_cache.json` in the backup folder: path → (size, mtime ns, CRC-32)
/// for the project files and the installed game files. A repeated install then reads only files whose size or mtime
/// changed. Only the entries used by the last install are written back, so it never grows stale.
#[derive(Default, Serialize, Deserialize)]
struct DiskCrcCache {
    #[serde(default)]
    entries: HashMap<String, CrcEntry>,
    #[serde(skip)]
    used: HashMap<String, CrcEntry>,
}

impl DiskCrcCache {
    fn load(backup: &Path) -> Self {
        std::fs::read_to_string(backup.join(CRC_CACHE_FILE)).ok().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default()
    }

    /// Best effort: a missing cache only costs a re-read.
    fn save(&self, backup: &Path) {
        let out = DiskCrcCache { entries: self.used.clone(), used: HashMap::new() };
        if let Ok(s) = serde_json::to_string(&out) {
            let _ = std::fs::write(backup.join(CRC_CACHE_FILE), s);
        }
    }

    /// (size, CRC-32) of each file (`None` = missing), through this cache and [`CRC_CACHE`]; the misses are read in
    /// parallel. `meta` = (size, mtime ns) when the caller already has it (the project walk), else one `metadata`.
    fn crcs(&mut self, files: &[(PathBuf, Option<(u64, u64)>)]) -> Vec<Option<(u64, u32)>> {
        let disk = &self.entries;
        let found: Vec<Option<(CrcEntry, bool)>> = files
            .par_iter()
            .map(|(p, meta)| {
                let (len, mt) = match meta {
                    Some(m) => *m,
                    None => {
                        let m = std::fs::metadata(p).ok().filter(|m| m.is_file())?;
                        (m.len(), mtime_ns(&m).unwrap_or(0))
                    }
                };
                if mt != 0 {
                    if let Some(&(l, t, c)) = disk.get(p.to_string_lossy().as_ref()) {
                        if (l, t) == (len, mt) {
                            return Some(((l, t, c), true));
                        }
                    }
                    if let Some(&(l, t, c)) = crc_cache().get(p) {
                        if (l, t) == (len, mt) {
                            return Some(((l, t, c), true));
                        }
                    }
                }
                let (l, c) = file_crc(p)?;
                Some(((l, mt, c), l == len && mt != 0 && cacheable(mt)))
            })
            .collect();
        let mut mem = crc_cache();
        files
            .iter()
            .zip(found)
            .map(|((p, _), f)| {
                let (e, keep) = f?;
                if keep {
                    self.used.insert(p.to_string_lossy().into_owned(), e);
                    mem.insert(p.clone(), e);
                }
                Some((e.0, e.2))
            })
            .collect()
    }

    /// Seed both caches with a file we just wrote and whose CRC we know, so the next install / status is cheap.
    fn remember(&mut self, p: &Path, len: u64, crc: u32) {
        let Ok(m) = std::fs::metadata(p) else { return };
        let Some(mt) = mtime_ns(&m) else { return };
        if m.len() == len && cacheable(mt) {
            self.used.insert(p.to_string_lossy().into_owned(), (len, mt, crc));
            crc_cache().insert(p.to_path_buf(), (len, mt, crc));
        }
    }
}

/// (size, mtime ns) of each file from one listing per folder, in parallel: on Windows `metadata` opens the file
/// (~0.1 ms each, 300 ms for 3,000 files), while the listing already carries both. `None` = not listed (the caller
/// then asks `metadata`, which also covers a missing file).
fn listed_meta(files: &[PathBuf]) -> Vec<Option<(u64, u64)>> {
    let key = |p: &Path| Some((p.parent()?.to_path_buf(), p.file_name()?.to_string_lossy().to_lowercase()));
    let mut by_dir: HashMap<PathBuf, HashMap<String, Option<(u64, u64)>>> = HashMap::new();
    for p in files {
        if let Some((d, n)) = key(p) {
            by_dir.entry(d).or_default().insert(n, None);
        }
    }
    by_dir.par_iter_mut().for_each(|(dir, want)| {
        let Ok(rd) = std::fs::read_dir(dir) else { return };
        for e in rd.flatten() {
            if let Some(slot) = want.get_mut(&e.file_name().to_string_lossy().to_lowercase()) {
                if let Some(m) = e.metadata().ok().filter(|m| m.is_file()) {
                    *slot = mtime_ns(&m).map(|t| (m.len(), t));
                }
            }
        }
    });
    files.iter().map(|p| key(p).and_then(|(d, n)| by_dir.get(&d)?.get(&n).copied().flatten())).collect()
}

fn now() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

/// Backup folder for one game install, under `backups_root`.
pub fn backup_dir_for(backups_root: &Path, game_dir: &Path) -> PathBuf {
    let key = game_dir.to_string_lossy().to_lowercase().replace('/', "\\");
    backups_root.join(format!("{:08x}", crc32fast::hash(key.as_bytes())))
}

fn read_manifest(dir: &Path) -> Result<Option<Manifest>> {
    let p = dir.join(MANIFEST);
    if !p.exists() {
        return Ok(None);
    }
    Ok(Some(serde_json::from_str(&std::fs::read_to_string(&p).at(&p)?)?))
}

fn write_manifest(dir: &Path, m: &Manifest) -> Result<()> {
    std::fs::create_dir_all(dir).at(dir)?;
    std::fs::write(dir.join(MANIFEST), serde_json::to_string_pretty(m)?).at(dir.join(MANIFEST))
}

pub fn installed_state(backup: &Path) -> Result<Option<InstalledState>> {
    let Some(m) = read_manifest(backup)? else { return Ok(None) };
    let game = PathBuf::from(&m.game_dir);
    let files = m
        .files
        .iter()
        .map(|f| InstalledFile {
            path: f.path.clone(),
            kind: if f.existed { "replaced" } else { "new" },
            intact: cached_crc(&game.join(&f.path)) == Some((f.installed_size, f.installed_crc)),
        })
        .collect();
    Ok(Some(InstalledState { installed_at: m.installed_at, project_dir: m.project_dir, backup_dir: backup.display().to_string(), files }))
}

/// Undo our last install in `game_dir`. Returns `None` when nothing of ours is installed.
pub fn restore(game_dir: &Path, backup: &Path) -> Result<Option<RestoreReport>> {
    let Some(m) = read_manifest(backup)? else { return Ok(None) };
    let mut report = RestoreReport::default();

    // Files: only touch what is still exactly ours.
    let mut foreign: std::collections::HashSet<&str> = std::collections::HashSet::new();
    for f in &m.files {
        if !in_install_scope(&f.path) {
            report.skipped.push(format!("{}: fuera de data/, no se toca (loader / evt_loader no forman parte de la instalación)", f.path));
            foreign.insert(f.path.as_str());
            continue;
        }
        let target = game_dir.join(&f.path);
        if file_crc(&target).is_some_and(|c| !f.is_ours(c)) {
            report.skipped.push(format!("{}: modificado por otro programa después de instalar, no se toca", f.path));
            foreign.insert(f.path.as_str());
            continue;
        }
        if f.existed {
            copy(&backup.join("files").join(&f.path), &target)?;
            report.restored += 1;
        } else if target.exists() {
            std::fs::remove_file(&target).at(&target)?;
            report.removed += 1;
        }
    }

    // cpk_list: revert only our entries.
    let list_path = game_dir.join("data").join("cpk_list.cfg.bin");
    let mut list = phase("install/leer cpk_list", || cpk::read_list(&list_path))?;
    let mut remove: Vec<usize> = Vec::new();
    let by_path = list.path_index();
    for f in &m.files {
        let Some(&i) = by_path.get(&f.path.to_lowercase()) else { continue };
        let it = &mut list.items[i];
        let still_ours = !foreign.contains(f.path.as_str()) && f.entry_is_ours(it);
        if !still_ours {
            report.skipped.push(format!("{}: la entrada de cpk_list cambió después de instalar, no se toca", f.path));
            continue;
        }
        match &f.original_entry {
            Some(o) => {
                it.cpk_dir = o.cpk_dir.clone();
                it.cpk_name = o.cpk_name.clone();
                it.size = o.size;
            }
            None => remove.push(i),
        }
        report.list_entries += 1;
    }
    remove.sort_unstable();
    for i in remove.into_iter().rev() {
        list.items.remove(i);
    }
    list.sort();
    cpk::write_list(&list_path, &list)?;
    cpk::read_list(&list_path)?;

    std::fs::remove_dir_all(backup).at(backup)?;
    Ok(Some(report))
}

/// Undo an install made by the first version of the tool (backup inside `<project>/.evt_backup`).
pub fn restore_legacy(game_dir: &Path, project: &Path) -> Result<bool> {
    #[derive(Deserialize)]
    struct Legacy {
        replaced: Vec<String>,
        created: Vec<String>,
    }
    let root = project.join(LEGACY_DIR);
    let p = root.join(MANIFEST);
    if !p.exists() {
        return Ok(false);
    }
    let m: Legacy = serde_json::from_str(&std::fs::read_to_string(&p).at(&p)?)?;
    for rel in &m.replaced {
        if !in_install_scope(rel) {
            continue;
        }
        copy(&root.join(rel), &game_dir.join(rel))?;
    }
    for rel in &m.created {
        if !in_install_scope(rel) {
            continue;
        }
        let t = game_dir.join(rel);
        if t.exists() {
            std::fs::remove_file(&t).at(&t)?;
        }
    }
    std::fs::remove_dir_all(&root).at(&root)?;
    Ok(true)
}

/// One project file to install: logical path + the size and CRC-32 of its current content.
struct Src {
    rel: String,
    size: u64,
    crc: u32,
}

fn same_dir(a: &str, b: &Path) -> bool {
    let norm = |s: &str| s.to_lowercase().replace('/', "\\").trim_end_matches('\\').to_string();
    norm(a) == norm(&b.to_string_lossy())
}

/// Mark `s` loose in the list with its size (adding the entry if the list lacks it). `Some(true)` = added,
/// `Some(false)` = an existing entry changed, `None` = it already was exactly that.
fn set_loose(list: &mut CpkList, by_path: &HashMap<String, usize>, s: &Src) -> Option<bool> {
    match by_path.get(&s.rel.to_lowercase()) {
        Some(&i) => {
            let it = &mut list.items[i];
            let want = Some(String::new());
            if it.cpk_dir == want && it.cpk_name == want && it.size == s.size as i32 {
                return None;
            }
            // Proven in-game: empty strings mark a loose entry (what Viola writes).
            it.cpk_dir = want.clone();
            it.cpk_name = want;
            it.size = s.size as i32;
            Some(false)
        }
        None => {
            let (dir, name) = CpkItem::split_path(&s.rel);
            list.items.push(CpkItem { dir, name, cpk_dir: Some(String::new()), cpk_name: Some(String::new()), size: s.size as i32 });
            Some(true)
        }
    }
}

/// Every copied file must have its list size on disk (a loose file of another size crashes the game).
fn check_sizes<'a>(game: &Path, copied: impl Iterator<Item = &'a Src>) -> Result<()> {
    for s in copied {
        let p = game.join(&s.rel);
        let on_disk = std::fs::metadata(&p).at(&p)?.len();
        if on_disk != s.size {
            return Err(Error::Format(format!("{}: tamaño en disco {on_disk} ≠ {}", s.rel, s.size)));
        }
    }
    Ok(())
}

fn write_checked_list(list_path: &Path, list: &mut CpkList) -> Result<()> {
    list.sort();
    if !list.is_sorted() {
        return Err(Error::Format("cpk_list no quedó ordenado".into()));
    }
    cpk::write_list(list_path, list)?;
    cpk::read_list(list_path)?;
    Ok(())
}

/// Install `project/data/**` into the game at `game` (loose files + `cpk_list` entries), backing up what it replaces
/// under [`backup_dir_for`]`(backups_root, game)`. Incremental unless `opts.full` or the previous install cannot be
/// diffed; [`InstallReport::list_written`] says whether `cpk_list` changed.
pub fn install_dirs(game: &Path, project: &Path, backups_root: &Path, opts: InstallOptions) -> Result<InstallReport> {
    let t0 = Instant::now();
    let (game, project) = (game.to_path_buf(), project.to_path_buf());
    let backup = backup_dir_for(backups_root, &game);

    let walked = phase("install/recorrer proyecto", || crate::walk::walk_files_meta(&project))?;
    if let Some((rel, ..)) = walked.iter().find(|(rel, ..)| !in_install_scope(rel)) {
        return Err(Error::Config(format!("{rel}: fuera de data/, el proyecto no puede instalar ahí")));
    }
    if walked.is_empty() {
        return Err(Error::Config("El proyecto no tiene archivos".into()));
    }
    if let Some((rel, ..)) = walked.iter().find(|(_, size, _)| *size >= i32::MAX as u64) {
        return Err(Error::Format(format!("{rel}: archivo demasiado grande para cpk_list")));
    }

    let mut cache = phase("install/cargar caché CRC", || DiskCrcCache::load(&backup));
    let jobs: Vec<(PathBuf, Option<(u64, u64)>)> =
        walked.iter().map(|(rel, size, mt)| (project.join(rel), Some((*size, *mt as u64)))).collect();
    let mut src = Vec::with_capacity(walked.len());
    let crcs = phase("install/CRC proyecto", || cache.crcs(&jobs));
    for ((rel, ..), c) in walked.iter().zip(crcs) {
        let (size, crc) = c.ok_or_else(|| Error::NotFound(rel.clone()))?;
        if size >= i32::MAX as u64 {
            return Err(Error::Format(format!("{rel}: archivo demasiado grande para cpk_list")));
        }
        src.push(Src { rel: rel.clone(), size, crc });
    }

    let legacy = restore_legacy(&game, &project)?;
    let prev = phase("install/leer manifiesto", || read_manifest(&backup))?;
    let full_reason = match &prev {
        _ if opts.full => Some("pedida (--full)".to_string()),
        _ if legacy => Some("instalación antigua dentro del proyecto (.evt_backup)".to_string()),
        None => Some("no hay instalación previa".to_string()),
        Some(m) if m.version != MANIFEST_VERSION => Some(format!("manifiesto de una versión anterior (v{})", m.version)),
        Some(m) if !same_dir(&m.game_dir, &game) => Some("el manifiesto es de otra carpeta del juego".to_string()),
        Some(m) if !same_dir(&m.project_dir, &project) => Some("la instalación anterior era de otra carpeta de proyecto".to_string()),
        Some(_) => None,
    };

    let (mut report, list_written) = match (full_reason, prev) {
        (None, Some(prev)) => install_incremental(&game, &project, &backup, &src, prev, &mut cache)?,
        (reason, _) => {
            let (mut r, w) = install_full(&game, &project, &backup, &src, &mut cache)?;
            r.full = true;
            r.full_reason = reason.unwrap_or_default();
            (r, w)
        }
    };
    for s in &src {
        let lower = s.rel.to_lowercase();
        if lower.ends_with(".acb") || lower.ends_with(".awb") || lower.ends_with(".usm") {
            report.warnings.push(format!("{}: audio/vídeo suelto sin CPK no está probado en el juego", s.rel));
        }
    }
    phase("install/guardar caché CRC", || cache.save(&backup));
    report.list_written = list_written;
    report.files = src.len();
    report.backup_dir = backup.display().to_string();
    report.elapsed_ms = t0.elapsed().as_millis() as u64;
    Ok(report)
}

fn new_manifest(game: &Path, project: &Path, files: Vec<FileRecord>) -> Manifest {
    Manifest {
        version: MANIFEST_VERSION,
        game_dir: game.display().to_string(),
        project_dir: project.display().to_string(),
        installed_at: now(),
        files,
    }
}

/// The original path: restore the previous install, back up everything the project touches, copy everything.
fn install_full(game: &Path, project: &Path, backup: &Path, src: &[Src], cache: &mut DiskCrcCache) -> Result<(InstallReport, bool)> {
    let mut report = InstallReport::default();

    // 1. Start from the game as it was before our previous install.
    if let Some(r) = phase("install/restaurar anterior", || restore(game, backup))? {
        report.restored = r.restored + r.removed;
        report.warnings.extend(r.skipped);
    }
    let list_path = game.join("data").join("cpk_list.cfg.bin");
    let mut list = phase("install/leer cpk_list", || cpk::read_list(&list_path))?;
    let by_path = list.path_index();

    // 2. Record and back up everything we are about to touch, before touching anything.
    let mut records = Vec::with_capacity(src.len());
    let mut backups = Vec::new();
    for s in src {
        let target = game.join(&s.rel);
        let existed = target.exists();
        if existed {
            backups.push((target, backup.join("files").join(&s.rel)));
        }
        let original_entry = by_path.get(&s.rel.to_lowercase()).map(|&i| EntrySnapshot::of(&list.items[i]));
        records.push(FileRecord { path: s.rel.clone(), existed, installed_size: s.size, installed_crc: s.crc, original_entry, also_ours: Vec::new() });
    }
    par_copy(&backups)?;
    report.backed_up = backups.len();
    // Emergency copy of the whole list as it was (restore does not use it).
    copy(&list_path, &backup.join("cpk_list.before.cfg.bin"))?;
    write_manifest(backup, &new_manifest(game, project, records))?;

    // 3. Copy files and patch the list.
    let copies: Vec<(PathBuf, PathBuf)> = src.iter().map(|s| (project.join(&s.rel), game.join(&s.rel))).collect();
    phase("install/copiar", || par_copy(&copies))?;
    report.written = src.len();
    for s in src {
        match by_path.get(&s.rel.to_lowercase()) {
            Some(_) => report.modified_entries += 1,
            None => report.new_entries += 1,
        }
        set_loose(&mut list, &by_path, s);
        cache.remember(&game.join(&s.rel), s.size, s.crc);
    }

    // 4. Validate, then write.
    check_sizes(game, src.iter())?;
    write_checked_list(&list_path, &mut list)?;
    Ok((report, true))
}

/// Only what changed since `prev` (same game and project folders, current manifest version). Per file, the outcome
/// is the one [`install_full`] would reach (restore `prev`, then install `src`); see the module docs and
/// `docs/Finalizado/01-install-pipeline.md` for the cases.
fn install_incremental(
    game: &Path,
    project: &Path,
    backup: &Path,
    src: &[Src],
    prev: Manifest,
    cache: &mut DiskCrcCache,
) -> Result<(InstallReport, bool)> {
    let mut report = InstallReport::default();
    let list_path = game.join("data").join("cpk_list.cfg.bin");
    let mut list = phase("install/leer cpk_list", || cpk::read_list(&list_path))?;
    let by_path = list.path_index();
    let files_dir = backup.join("files");

    // Current content of every game file of the previous install (only the folder listings when cached).
    let scoped: Vec<&FileRecord> = prev.files.iter().filter(|f| in_install_scope(&f.path)).collect();
    let paths: Vec<PathBuf> = scoped.iter().map(|f| game.join(&f.path)).collect();
    let metas = phase("install/listar juego", || listed_meta(&paths));
    let jobs: Vec<(PathBuf, Option<(u64, u64)>)> = paths.into_iter().zip(metas).collect();
    let game_now: HashMap<String, Option<(u64, u32)>> =
        scoped.iter().zip(phase("install/CRC juego", || cache.crcs(&jobs))).map(|(f, c)| (f.path.to_lowercase(), c)).collect();
    let prev_by: HashMap<String, &FileRecord> = prev.files.iter().map(|f| (f.path.to_lowercase(), f)).collect();

    // 1. Plan, per project file.
    let mut records = Vec::with_capacity(src.len());
    let mut interim = Vec::with_capacity(src.len() + prev.files.len());
    let mut backups: Vec<(PathBuf, PathBuf)> = Vec::new();
    let mut copied: Vec<&Src> = Vec::new();
    for s in src {
        let key = s.rel.to_lowercase();
        let target = game.join(&s.rel);
        let entry = by_path.get(&key).map(|&i| &list.items[i]);
        let mut also_ours = Vec::new();
        let (existed, original_entry, needs_copy) = match prev_by.get(&key) {
            Some(r) => {
                let now = game_now.get(&key).copied().flatten();
                if now.is_some_and(|c| !r.is_ours(c)) {
                    // Changed by another program: like a full install, its version becomes the state to restore.
                    report.warnings.push(format!(
                        "{}: modificado por otro programa después de instalar; esa versión pasa a la copia de seguridad y se instala encima la del proyecto",
                        s.rel
                    ));
                    backups.push((target.clone(), files_dir.join(&s.rel)));
                    (true, entry.map(EntrySnapshot::of), true)
                } else {
                    // Ours (or deleted): the backup and the original entry stay as they are.
                    let original_entry = match entry {
                        Some(e) if r.entry_is_ours(e) => r.original_entry.clone(),
                        Some(e) => {
                            report.warnings.push(format!(
                                "{}: la entrada de cpk_list cambió después de instalar; su estado actual pasa a ser el original",
                                s.rel
                            ));
                            Some(EntrySnapshot::of(e))
                        }
                        None => None,
                    };
                    let needs_copy = now != Some((s.size, s.crc));
                    if needs_copy {
                        also_ours.push((r.installed_size, r.installed_crc));
                        also_ours.extend(r.also_ours.iter().copied());
                        also_ours.extend(now);
                        also_ours.retain(|&c| c != (s.size, s.crc));
                        also_ours.dedup();
                    }
                    (r.existed, original_entry, needs_copy)
                }
            }
            None => {
                let existed = target.exists();
                if existed {
                    backups.push((target.clone(), files_dir.join(&s.rel)));
                }
                (existed, entry.map(EntrySnapshot::of), true)
            }
        };
        if needs_copy {
            copied.push(s);
        } else {
            report.skipped += 1;
        }
        let rec = FileRecord { path: s.rel.clone(), existed, installed_size: s.size, installed_crc: s.crc, original_entry, also_ours: Vec::new() };
        interim.push(FileRecord { also_ours, ..rec.clone() });
        records.push(rec);
    }

    // 2. Plan, per file no longer in the project: exactly what `restore` does for it.
    let in_project: HashSet<String> = src.iter().map(|s| s.rel.to_lowercase()).collect();
    let mut put_back: Vec<(PathBuf, PathBuf)> = Vec::new();
    let mut delete: Vec<PathBuf> = Vec::new();
    let mut entry_reverts: Vec<(usize, Option<EntrySnapshot>)> = Vec::new();
    let mut gone: Vec<&FileRecord> = Vec::new();
    for r in prev.files.iter().filter(|r| !in_project.contains(&r.path.to_lowercase())) {
        gone.push(r);
        interim.push(r.clone());
        let key = r.path.to_lowercase();
        let foreign = if !in_install_scope(&r.path) {
            report.warnings.push(format!("{}: fuera de data/, no se toca (loader / evt_loader no forman parte de la instalación)", r.path));
            true
        } else {
            let now = game_now.get(&key).copied().flatten();
            let foreign = now.is_some_and(|c| !r.is_ours(c));
            if foreign {
                report.warnings.push(format!("{}: modificado por otro programa después de instalar, no se toca", r.path));
            } else if r.existed {
                put_back.push((files_dir.join(&r.path), game.join(&r.path)));
            } else if now.is_some() {
                delete.push(game.join(&r.path));
            }
            foreign
        };
        if let Some(&i) = by_path.get(&key) {
            if !foreign && r.entry_is_ours(&list.items[i]) {
                entry_reverts.push((i, r.original_entry.clone()));
            } else {
                report.warnings.push(format!("{}: la entrada de cpk_list cambió después de instalar, no se toca", r.path));
            }
        }
    }

    // 3. Back up originals first, then say in the manifest what is about to happen (old and new contents both count
    //    as ours, files leaving the project are still listed), so an interrupted install restores exactly.
    let busy = !copied.is_empty() || !gone.is_empty();
    par_copy(&backups)?;
    report.backed_up = backups.len();
    if !backup.join("cpk_list.before.cfg.bin").exists() {
        copy(&list_path, &backup.join("cpk_list.before.cfg.bin"))?;
    }
    if busy {
        write_manifest(backup, &new_manifest(game, project, interim))?;
    }

    // 4. Copy what changed.
    let jobs: Vec<(PathBuf, PathBuf)> = copied.iter().map(|s| (project.join(&s.rel), game.join(&s.rel))).collect();
    phase("install/copiar", || par_copy(&jobs))?;
    report.written = copied.len();
    for s in &copied {
        cache.remember(&game.join(&s.rel), s.size, s.crc);
    }
    check_sizes(game, copied.iter().copied())?;

    // 5. cpk_list: only the entries that need it. The list is written before the removed files are put back, so the
    //    game never sees a loose entry whose file is already the original of another size.
    let mut changed = false;
    let mut remove = Vec::new();
    for (i, original) in entry_reverts {
        match original {
            Some(o) => {
                let it = &mut list.items[i];
                it.cpk_dir = o.cpk_dir;
                it.cpk_name = o.cpk_name;
                it.size = o.size;
                report.modified_entries += 1;
            }
            None => remove.push(i),
        }
        changed = true;
    }
    for s in src {
        match set_loose(&mut list, &by_path, s) {
            Some(true) => report.new_entries += 1,
            Some(false) => report.modified_entries += 1,
            None => continue,
        }
        changed = true;
    }
    remove.sort_unstable();
    for i in remove.into_iter().rev() {
        list.items.remove(i);
    }
    if changed {
        phase("install/escribir cpk_list", || write_checked_list(&list_path, &mut list))?;
    }

    // 6. Put back the files that left the project, and drop their backups.
    par_copy(&put_back)?;
    for p in &delete {
        std::fs::remove_file(p).at(p)?;
    }
    report.restored = put_back.len() + delete.len();
    for r in &gone {
        let b = files_dir.join(&r.path);
        if in_install_scope(&r.path) && b.is_file() {
            std::fs::remove_file(&b).at(&b)?;
        }
    }

    phase("install/manifiesto", || write_manifest(backup, &new_manifest(game, project, records)))?;
    Ok((report, changed))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crc_cache_follows_the_file() {
        let p = std::env::temp_dir().join(format!("evt_crc_cache_{}.bin", std::process::id()));
        std::fs::write(&p, b"uno").unwrap();
        assert_eq!(cached_crc(&p), file_crc(&p));
        assert_eq!(cached_crc(&p), file_crc(&p)); // from the cache
        // Rewritten by "another program": new size + mtime, so the cache must not answer with the old CRC.
        std::fs::write(&p, b"otro contenido").unwrap();
        assert_eq!(cached_crc(&p), file_crc(&p));
        std::fs::remove_file(&p).unwrap();
        assert_eq!(cached_crc(&p), None);
    }

    /// A file hashed right after it was written is not cached (racy mtime); an old one is, on disk too.
    #[test]
    fn disk_cache_skips_racy_files() {
        let dir = std::env::temp_dir().join(format!("evt_disk_crc_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let (fresh, old) = (dir.join("fresh.bin"), dir.join("old.bin"));
        std::fs::write(&fresh, b"fresh").unwrap();
        std::fs::write(&old, b"old").unwrap();
        let f = std::fs::File::options().write(true).open(&old).unwrap();
        f.set_modified(std::time::SystemTime::now() - std::time::Duration::from_secs(3600)).unwrap();
        drop(f);
        let mut c = DiskCrcCache::default();
        let got = c.crcs(&[(fresh.clone(), None), (old.clone(), None), (dir.join("missing"), None)]);
        assert_eq!(got, vec![file_crc(&fresh), file_crc(&old), None]);
        c.save(&dir);
        let loaded = DiskCrcCache::load(&dir);
        assert!(loaded.entries.contains_key(old.to_string_lossy().as_ref()));
        assert!(!loaded.entries.contains_key(fresh.to_string_lossy().as_ref()));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn scope_is_data_only() {
        assert!(in_install_scope("data/common/text/x.cfg.bin"));
        assert!(in_install_scope("data\\dx11\\chr\\a.g4tx"));
        assert!(!in_install_scope("winmm.dll"));
        assert!(!in_install_scope("evt_loader/lua_patches/_fingerprints.json"));
        assert!(!in_install_scope("evt_loader/lua_patches/title_main/01.lua"));
        assert!(!in_install_scope("mods/x/lua/a.lua"));
        assert!(!in_install_scope("data/cpk_list.cfg.bin"));
        assert!(!in_install_scope("data/../winmm.dll"));
    }

    /// A manifest that (wrongly) lists the loader and a lua_patches root file must not make `restore` delete or
    /// overwrite them: the loader and `evt_loader/**` are never part of the project install.
    #[test]
    fn restore_never_touches_loader_or_lua_patches() {
        let tmp = std::env::temp_dir().join(format!("evt_restore_scope_{}", std::process::id()));
        let game = tmp.join("game");
        let backup = tmp.join("backup");
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(game.join("data")).unwrap();
        std::fs::create_dir_all(game.join("evt_loader/lua_patches/title_main")).unwrap();
        std::fs::create_dir_all(backup.join("files")).unwrap();
        let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../assets/v7.1.2/data/cpk_list.cfg.bin");
        if !fixture.is_file() {
            eprintln!("assets/v7.1.2/data/cpk_list.cfg.bin (your own retail cpk_list) not found: skipped");
            return;
        }
        std::fs::copy(&fixture, game.join("data/cpk_list.cfg.bin")).unwrap();
        let dll = b"NEW LOADER CMND_EVT_LOADER_VERSION".to_vec();
        std::fs::write(game.join("winmm.dll"), &dll).unwrap();
        std::fs::write(game.join("evt_loader/lua_patches/_fingerprints.json"), b"{}").unwrap();
        std::fs::write(game.join("evt_loader/lua_patches/title_main/01_test.lua"), b"-- p").unwrap();
        std::fs::write(backup.join("files/winmm.dll"), b"OLD LOADER").unwrap();
        // Records: winmm.dll "existed" (a backup copy is there), the json and the patch "did not exist";
        // CRCs match the current content so a naive restore would revert / delete them.
        let rec = |path: &str, existed: bool| {
            let (installed_size, installed_crc) = file_crc(&game.join(path)).unwrap();
            FileRecord { path: path.into(), existed, installed_size, installed_crc, original_entry: None, also_ours: Vec::new() }
        };
        let m = Manifest {
            version: 2,
            game_dir: game.display().to_string(),
            project_dir: String::new(),
            installed_at: 0,
            files: vec![
                rec("winmm.dll", true),
                rec("evt_loader/lua_patches/_fingerprints.json", false),
                rec("evt_loader/lua_patches/title_main/01_test.lua", false),
            ],
        };
        std::fs::write(backup.join(MANIFEST), serde_json::to_string(&m).unwrap()).unwrap();

        let r = restore(&game, &backup).unwrap().unwrap();
        assert_eq!(std::fs::read(game.join("winmm.dll")).unwrap(), dll, "winmm.dll was downgraded");
        assert!(game.join("evt_loader/lua_patches/_fingerprints.json").is_file(), "_fingerprints.json was deleted");
        assert!(game.join("evt_loader/lua_patches/title_main/01_test.lua").is_file());
        assert_eq!((r.restored, r.removed, r.list_entries), (0, 0, 0));
        assert_eq!(r.skipped.len(), 3);
        let _ = std::fs::remove_dir_all(&tmp);
    }
}
