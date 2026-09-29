//! Install and uninstall.
//!
//! `<game>\evt_backup\<stamp>\` of one install:
//! ```text
//! evt_install.json      Record: what was installed (extras with existed / size / CRC-32, folders created)
//! cpk_list.cfg.bin      the game's list before the install (put back byte for byte when nothing else changed it)
//! originales\<ruta>     game-root files that existed and were replaced (winmm.dll, evt_loader\config.toml, ...)
//! datos\<id>\           vr-gamefiles backup + manifest.json of the data install (`vr_gamefiles::install`)
//! ```
//! The record is written (state "instalando") after the originals are saved and before anything is written, so an
//! interrupted install can still be undone.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use vr_gamefiles::install::{self, InstallOptions};

use crate::pack::{Pack, GAME_FILES, LOADER_CONFIG};
use crate::{copy_file, file_crc, loadercfg, BACKUP_DIR, RECORD_FILE};

const RECORD_FORMAT: u32 = 1;
const LIST: &str = "cpk_list.cfg.bin";
const ORIGINALS: &str = "originales";
const DATA_BACKUPS: &str = "datos";
/// Data folder of the install: whole files hard-linked + patched files (inside the pack: links stay on one volume).
const STAGING: &str = "_montaje";
const ENABLED: &str = "mods/enabled.toml";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Extra {
    /// Game-relative path, `/` separated.
    pub ruta: String,
    /// The game had this file before; its copy is `originales/<ruta>`.
    pub existia: bool,
    pub tam: u64,
    pub crc: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Record {
    pub formato: u32,
    pub producto: String,
    pub version: String,
    /// "instalando" until the install finished, then "instalado".
    pub estado: String,
    pub instalado_en: u64,
    pub juego: String,
    pub extras: Vec<Extra>,
    /// Game folders that did not exist before (removed on uninstall when empty).
    pub carpetas_creadas: Vec<String>,
    #[serde(default)]
    pub avisos: Vec<String>,
}

/// One install found in `<game>\evt_backup`.
pub struct Installed {
    pub dir: PathBuf,
    pub record: Record,
}

/// Installs of this game, oldest first.
pub fn find_installs(game: &Path) -> Vec<Installed> {
    let mut out = Vec::new();
    let Ok(rd) = std::fs::read_dir(game.join(BACKUP_DIR)) else { return out };
    let mut dirs: Vec<PathBuf> = rd.filter_map(|e| e.ok().map(|e| e.path())).filter(|p| p.join(RECORD_FILE).is_file()).collect();
    dirs.sort();
    for dir in dirs {
        if let Some(record) = std::fs::read_to_string(dir.join(RECORD_FILE)).ok().and_then(|t| serde_json::from_str(&t).ok()) {
            out.push(Installed { dir, record });
        }
    }
    out
}

pub(crate) fn write_record(dir: &Path, r: &Record) -> Result<(), String> {
    let p = dir.join(RECORD_FILE);
    let tmp = dir.join(format!("{RECORD_FILE}.tmp"));
    std::fs::write(&tmp, serde_json::to_string_pretty(r).map_err(|e| e.to_string())?).map_err(|e| format!("{}: {e}", tmp.display()))?;
    std::fs::rename(&tmp, &p).map_err(|e| format!("{}: {e}", p.display()))
}

#[derive(Debug, Default)]
pub struct InstallOutcome {
    pub backup_dir: PathBuf,
    pub data_files: usize,
    pub data_written: usize,
    pub new_entries: usize,
    pub modified_entries: usize,
    pub game_files: usize,
    pub modules_changed: Vec<String>,
    pub mods_enabled: Vec<String>,
    pub warnings: Vec<String>,
}

pub(crate) fn io<T>(r: std::io::Result<T>, p: &Path) -> Result<T, String> {
    r.map_err(|e| format!("{}: {e}", p.display()))
}

/// Install `pack` into `game`. The caller has uninstalled any previous install ([`find_installs`] must be empty)
/// and checked that the game is closed. Nothing in the game changes before every delta has been applied (into a
/// staging folder inside the pack), so a player file that is not the v7.1.2 one stops the install cleanly.
pub fn install(game: &Path, pack: &Pack, log: &mut dyn FnMut(&str)) -> Result<InstallOutcome, String> {
    if !find_installs(game).is_empty() {
        return Err("ya hay una instalación: desinstálala antes".into());
    }
    if !crate::modops::find_components(game).is_empty() {
        return Err("hay mods instalados con el formato por mods: desinstálalos antes".into());
    }
    let plan = pack.plan();
    let mut out = InstallOutcome::default();

    // 0. Data folder to install: the pack itself, or a staging copy with the patched files.
    let project = if pack.patches.is_empty() {
        pack.root.clone()
    } else {
        log(&format!("Aplicando {} parches a tus archivos originales del juego...", pack.patches.len()));
        match stage(game, pack, &plan.data, log) {
            Ok(p) => p,
            Err(e) => {
                let _ = std::fs::remove_dir_all(pack.root.join(STAGING));
                return Err(e);
            }
        }
    };
    let res = install_staged(game, pack, &plan, &project, &mut out, log);
    if project != pack.root {
        let _ = std::fs::remove_dir_all(&project);
    }
    res.map(|_| out)
}

fn install_staged(game: &Path, pack: &Pack, plan: &crate::pack::Plan, project: &Path, out: &mut InstallOutcome, log: &mut dyn FnMut(&str)) -> Result<(), String> {
    // 1. Backup folder, original list.
    let root = game.join(BACKUP_DIR);
    let base = crate::stamp(crate::now_secs());
    let mut bdir = root.join(&base);
    let mut n = 1;
    while bdir.exists() {
        n += 1;
        bdir = root.join(format!("{base}-{n}"));
    }
    io(std::fs::create_dir_all(&bdir), &bdir)?;
    let list = game.join("data").join(LIST);
    copy_file(&list, &bdir.join(LIST))?;
    out.backup_dir = bdir.clone();

    // 2. Extras: originals saved, folders to create noted, record written before any game file changes.
    let mut extras: Vec<String> = plan.game.clone();
    extras.push(LOADER_CONFIG.to_string());
    if !plan.mods.is_empty() {
        extras.push(ENABLED.to_string());
    }
    let mut record = Record {
        formato: RECORD_FORMAT,
        producto: pack.meta.producto.clone(),
        version: pack.meta.version.clone(),
        estado: "instalando".into(),
        instalado_en: crate::now_secs(),
        juego: game.display().to_string(),
        extras: Vec::new(),
        carpetas_creadas: Vec::new(),
        avisos: Vec::new(),
    };
    let mut created: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    for rel in &extras {
        let target = game.join(rel);
        let existia = target.is_file();
        if existia {
            copy_file(&target, &bdir.join(ORIGINALS).join(rel))?;
        }
        let mut anc = Path::new(rel).parent();
        while let Some(a) = anc.filter(|a| !a.as_os_str().is_empty()) {
            if !game.join(a).is_dir() {
                created.insert(a.to_string_lossy().replace('\\', "/"));
            }
            anc = a.parent();
        }
        record.extras.push(Extra { ruta: rel.clone(), existia, tam: 0, crc: 0 });
    }
    // folders the data install will create (the data install removes its files on restore, not the folders)
    for rel in plan.data.iter().chain(pack.patches.iter().map(|p| &p.ruta)) {
        let mut anc = Path::new(rel).parent();
        while let Some(a) = anc.filter(|a| !a.as_os_str().is_empty()) {
            if game.join(a).is_dir() || !created.insert(a.to_string_lossy().replace('\\', "/")) {
                break; // exists (so do its parents) or already noted with its parents
            }
            anc = a.parent();
        }
    }
    record.carpetas_creadas = created.into_iter().collect();
    write_record(&bdir, &record)?;
    log(&format!("Copia de seguridad en {}", bdir.display()));

    // 3. Data through vr-gamefiles (as the app's «Instalar»).
    log(&format!("Instalando {} archivos de datos ({}); puede tardar unos minutos...", plan.data.len() + pack.patches.len(), crate::human_size(plan.data_bytes)));
    let r = install::install_dirs(game, project, &bdir.join(DATA_BACKUPS), InstallOptions { full: true }).map_err(|e| e.to_string())?;
    out.data_files = r.files;
    out.data_written = r.written;
    out.new_entries = r.new_entries;
    out.modified_entries = r.modified_entries;
    out.warnings.extend(r.warnings);

    // 4. Loader, evt_loader files, mods.
    log(&format!("Copiando el loader y sus archivos ({} archivos)...", plan.game.len()));
    for rel in &plan.game {
        copy_file(&pack.root.join(GAME_FILES).join(rel), &game.join(rel))?;
    }
    out.game_files = plan.game.len();

    // 5. evt_loader\config.toml: the pack's, with the module switches of evt_loader_modules.toml.
    let src = pack.root.join(GAME_FILES).join(LOADER_CONFIG);
    let text = io(std::fs::read_to_string(&src), &src)?;
    let (cfg, changed) = loadercfg::apply_modules(&text, &plan.modules)?;
    let cfg_path = game.join(LOADER_CONFIG);
    io(std::fs::create_dir_all(cfg_path.parent().unwrap()), &cfg_path)?;
    io(std::fs::write(&cfg_path, cfg), &cfg_path)?;
    out.modules_changed = changed;

    // 6. mods\enabled.toml: the pack's mods switched on, other mods of the player kept as they were.
    if !plan.mods.is_empty() {
        let mods_root = game.join(evt_modfmt::MODS_DIR);
        let mut installed: Vec<String> = std::fs::read_dir(&mods_root)
            .map(|rd| {
                rd.filter_map(|e| e.ok())
                    .filter(|e| e.path().join(evt_modfmt::MANIFEST).is_file())
                    .map(|e| e.file_name().to_string_lossy().to_string())
                    .collect()
            })
            .unwrap_or_default();
        installed.sort();
        let mut current = evt_modfmt::read_enabled(&mods_root).ok().flatten();
        for id in &plan.mods {
            current = Some(evt_modfmt::toggle_enabled(current, &installed, id, true));
        }
        let list = current.unwrap_or_default();
        let p = game.join(ENABLED);
        io(std::fs::write(&p, evt_modfmt::enabled_text(&list)), &p)?;
        out.mods_enabled = plan.mods.clone();
    }

    // 7. Final record.
    for e in &mut record.extras {
        let (tam, crc) = io(file_crc(&game.join(&e.ruta)), &game.join(&e.ruta))?;
        e.tam = tam;
        e.crc = crc;
    }
    record.estado = "instalado".into();
    record.avisos = out.warnings.clone();
    write_record(&bdir, &record)?;
    Ok(())
}

/// `<pack>/_montaje/`: the whole files hard-linked (or copied) and every delta applied to the player's own retail
/// file (read from its CPK, or loose). Returns that folder as the project folder of the data install.
fn stage(game: &Path, pack: &Pack, data: &[String], log: &mut dyn FnMut(&str)) -> Result<PathBuf, String> {
    let st = pack.root.join(STAGING);
    stage_into(game, &pack.root, data, &pack.patches, &st, None, log)?;
    Ok(st)
}

/// Where the retail bytes of a path come from when the game no longer has them (the path is installed by a mod that
/// is being updated): `None` = the game ([`retail_bytes`]), else the bytes (or why not).
pub type OrigSource<'a> = &'a dyn Fn(&str, &vr_gamefiles::cpk_list::CpkList, &std::collections::HashMap<String, usize>) -> Option<Result<Vec<u8>, String>>;

/// Staging of one data set into `st` (emptied first): the whole files `data` (paths relative to `src`, which are also
/// their game paths) hard-linked or copied, and every patch applied (`delta` relative to `src`). Used by the single
/// bundle ([`install`], `src` = the pack) and per mod by [`crate::modops`] (`src` = `mods/<id>` of the pack; `orig`
/// gives the retail bytes of files that an installed older version of the mod replaced).
pub fn stage_into(
    game: &Path,
    src: &Path,
    data: &[String],
    patches: &[crate::pack::Patch],
    st: &Path,
    orig: Option<OrigSource>,
    log: &mut dyn FnMut(&str),
) -> Result<(), String> {
    if st.exists() {
        io(std::fs::remove_dir_all(st), st)?;
    }
    io(std::fs::create_dir_all(st), st)?;
    for rel in data {
        let from = src.join(rel);
        let to = st.join(rel);
        io(std::fs::create_dir_all(to.parent().unwrap()), &to)?;
        if std::fs::hard_link(&from, &to).is_err() {
            io(std::fs::copy(&from, &to), &to)?;
        }
    }
    let list_path = game.join("data").join(LIST);
    let list = vr_gamefiles::cpk::read_list(&list_path).map_err(|e| e.to_string())?;
    let by_path = list.path_index();
    let mut bad: Vec<String> = Vec::new();
    for (n, p) in patches.iter().enumerate() {
        if n % 50 == 0 && n > 0 {
            log(&format!("{n} de {} parches", patches.len()));
        }
        let from_backup = orig.and_then(|f| f(&p.ruta, &list, &by_path));
        let source = match from_backup.unwrap_or_else(|| retail_bytes(game, &list, &by_path, &p.ruta)) {
            Ok(b) => b,
            Err(e) => {
                bad.push(format!("{}: {e}", p.ruta));
                continue;
            }
        };
        let delta = io(std::fs::read(src.join(&p.delta)), &src.join(&p.delta))?;
        let to = st.join(&p.ruta);
        io(std::fs::create_dir_all(to.parent().unwrap()), &to)?;
        let f = io(std::fs::File::create(&to), &to)?;
        let mut w = l5_cpk::xor::XorWriter::new(std::io::BufWriter::new(f), p.xor, 0);
        match crate::delta::apply(&source, &delta, &mut w) {
            Ok(_) => {
                use std::io::Write;
                io(w.flush(), &to)?;
            }
            Err(crate::delta::ApplyError::SourceMismatch) => {
                bad.push(format!("{}: no es el archivo original de la v7.1.2", p.ruta));
            }
            Err(e) => return Err(format!("{}: {e}", p.ruta)),
        }
    }
    if !bad.is_empty() {
        let mut msg = format!(
            "{} archivos originales de tu juego no son los de la v7.1.2, así que no se puede aplicar el parche (no se ha tocado nada del juego):",
            bad.len()
        );
        for b in bad.iter().take(10) {
            msg.push_str(&format!("
    {b}"));
        }
        msg.push_str("
  Solución: quita otros mods, o en Steam «Propiedades → Archivos instalados → Verificar integridad», y comprueba que el juego es la v7.1.2.");
        return Err(msg);
    }
    Ok(())
}

/// The bytes the game reads for `path` without any mod: its CPK entry, or the loose file.
pub fn retail_bytes(game: &Path, list: &vr_gamefiles::cpk_list::CpkList, by_path: &std::collections::HashMap<String, usize>, path: &str) -> Result<Vec<u8>, String> {
    let i = *by_path.get(&path.to_lowercase()).ok_or("no está en el cpk_list del juego")?;
    let it = &list.items[i];
    match it.cpk_path() {
        Some(cpk) => vr_gamefiles::cpk::extract_from_cpk(&game.join(&cpk), &it.dir, &it.name).map_err(|e| e.to_string()),
        None => {
            let p = game.join(path);
            std::fs::read(&p).map_err(|e| format!("{}: {e}", p.display()))
        }
    }
}

#[derive(Debug, Default)]
pub struct UninstallOutcome {
    pub data_restored: usize,
    pub data_removed: usize,
    pub list_entries: usize,
    /// The original `cpk_list` bytes were put back.
    pub list_exact: bool,
    pub files_restored: usize,
    pub files_removed: usize,
    pub warnings: Vec<String>,
}

/// Undo the install recorded in `dir` (a folder of `<game>\evt_backup`), then delete that backup.
pub fn uninstall(game: &Path, dir: &Path, log: &mut dyn FnMut(&str)) -> Result<UninstallOutcome, String> {
    let record: Record = serde_json::from_str(&io(std::fs::read_to_string(dir.join(RECORD_FILE)), dir)?).map_err(|e| e.to_string())?;
    let mut out = UninstallOutcome::default();

    // 1. Data (files + our cpk_list entries) through vr-gamefiles.
    log("Restaurando los archivos de datos...");
    let data = dir.join(DATA_BACKUPS);
    if let Ok(rd) = std::fs::read_dir(&data) {
        for e in rd.filter_map(|e| e.ok()) {
            if e.path().join("manifest.json").is_file() {
                if let Some(r) = install::restore(game, &e.path()).map_err(|e| e.to_string())? {
                    out.data_restored += r.restored;
                    out.data_removed += r.removed;
                    out.list_entries += r.list_entries;
                    out.warnings.extend(r.skipped);
                }
            }
        }
    }

    // 2. The original list bytes, when its content is the same again (no other tool changed it meanwhile).
    let saved = dir.join(LIST);
    let list = game.join("data").join(LIST);
    if saved.is_file() {
        let plain = |p: &Path| vr_gamefiles::cpk::read_list(p).map(|l| l.to_bytes()).ok();
        match (plain(&saved), plain(&list)) {
            (Some(a), Some(b)) if a == b => {
                if std::fs::read(&saved).ok() != std::fs::read(&list).ok() {
                    copy_file(&saved, &list)?;
                }
                out.list_exact = true;
            }
            _ => out.warnings.push(
                "data/cpk_list.cfg.bin: otro programa lo cambió después de instalar; se han quitado solo nuestras entradas".into(),
            ),
        }
    }

    // 3. Game-root files: originals back, ours removed.
    log("Restaurando el loader y sus archivos...");
    for e in record.extras.iter().rev() {
        let target = game.join(&e.ruta);
        if e.existia {
            let orig = dir.join(ORIGINALS).join(&e.ruta);
            if orig.is_file() {
                copy_file(&orig, &target)?;
                out.files_restored += 1;
            } else {
                out.warnings.push(format!("{}: falta la copia original en la copia de seguridad", e.ruta));
            }
        } else if target.is_file() {
            io(std::fs::remove_file(&target), &target)?;
            out.files_removed += 1;
        }
    }
    let mut dirs = record.carpetas_creadas.clone();
    dirs.sort_by_key(|d| std::cmp::Reverse(d.matches('/').count()));
    for d in dirs {
        let p = game.join(&d);
        if p.is_dir() && std::fs::remove_dir(&p).is_err() {
            out.warnings.push(format!("{d}: se conserva (contiene archivos creados al jugar, p. ej. registros o datos de partida del mod)"));
        }
    }

    // 4. The backup itself.
    io(std::fs::remove_dir_all(dir), dir)?;
    let root = game.join(BACKUP_DIR);
    if std::fs::read_dir(&root).is_ok_and(|mut rd| rd.next().is_none()) {
        let _ = std::fs::remove_dir(&root);
    }
    Ok(out)
}

/// Uninstall every install of the game, newest first.
pub fn uninstall_all(game: &Path, log: &mut dyn FnMut(&str)) -> Result<Vec<UninstallOutcome>, String> {
    let mut v = Vec::new();
    for i in find_installs(game).into_iter().rev() {
        log(&format!("Desinstalando {} {} ({})...", i.record.producto, i.record.version, i.dir.file_name().unwrap_or_default().to_string_lossy()));
        v.push(uninstall(game, &i.dir, log)?);
    }
    Ok(v)
}
