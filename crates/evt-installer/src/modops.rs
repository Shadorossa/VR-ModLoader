//! Install / uninstall of a **mods layout** pack ([`crate::modpack`], format 2), one component at a time: the
//! ModLoader and each mod have their own backup and record, so a mod can be uninstalled alone.
//!
//! `<game>\evt_backup\componentes\`:
//! ```text
//! _modloader\evt_componente.json   the ModLoader: files written (winmm.dll, evt_loader\...), originals in originales\
//! <id>\evt_componente.json         one mod: files of mods\<id>\ + its data paths, whether we enabled it
//! <id>\originales\<ruta>           game files that existed and were replaced
//! <id>\datos\<hash>\               vr-gamefiles backup + manifest.json of the mod's data (loose files + cpk_list)
//! cpk_list.cfg.bin                 the list before the first mod with data (put back byte for byte after the last)
//! enabled_antes.json, enabled.toml mods\enabled.toml before the first mod we enabled (same idea)
//! ```
//! Each record is written (state "instalando") before its component writes anything, so an interrupted install is
//! undone like a finished one. Guide: `docs/app/instalador-mods.md`.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use vr_gamefiles::install::{self, InstallOptions};

use crate::modpack::{self, Avail, InstalledMod, LoaderAction, LoaderState, ModPack, PackMod, LOADER_DIR, MODS_DIR, MOD_FILES};
use crate::ops::{io, Extra, UninstallOutcome};
use crate::pack::LOADER_CONFIG;
use crate::{copy_file, file_crc, loadercfg, BACKUP_DIR};

pub const COMPONENTS_DIR: &str = "componentes";
pub const COMPONENT_FILE: &str = "evt_componente.json";
/// Component id of the ModLoader (not a valid mod id: mod ids start with a-z / 0-9).
pub const LOADER_ID: &str = "_modloader";
const RECORD_FORMAT: u32 = 1;
const LIST: &str = "cpk_list.cfg.bin";
const ORIGINALS: &str = "originales";
const DATA_BACKUPS: &str = "datos";
const STAGING: &str = "_montaje";
const ENABLED_BEFORE: &str = "enabled_antes.json";
const ENABLED_COPY: &str = "enabled.toml";

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ComponentRecord {
    pub formato: u32,
    /// "cargador" or "mod".
    pub tipo: String,
    pub id: String,
    pub version: String,
    pub producto: String,
    /// Version of the pack that installed it.
    pub paquete: String,
    /// "instalando" until the component finished, then "instalado".
    pub estado: String,
    pub instalado_en: u64,
    #[serde(default)]
    pub requires: Vec<String>,
    #[serde(default)]
    pub provides: Vec<String>,
    /// Game files written (`mods/<id>/...`, `winmm.dll`, ...).
    pub extras: Vec<Extra>,
    pub carpetas_creadas: Vec<String>,
    /// Game data paths installed through vr-gamefiles (loose + cpk_list).
    #[serde(default)]
    pub datos: Vec<String>,
    /// We switched the mod on in `mods\enabled.toml`.
    #[serde(default)]
    pub activado: bool,
    #[serde(default)]
    pub avisos: Vec<String>,
}

pub struct Component {
    pub dir: PathBuf,
    pub record: ComponentRecord,
}

impl Component {
    pub fn is_loader(&self) -> bool {
        self.record.id == LOADER_ID
    }
    /// Every file it wrote is still what it wrote.
    pub fn intact(&self, game: &Path) -> bool {
        self.record.estado == "instalado" && self.record.extras.iter().all(|e| file_crc(&game.join(&e.ruta)).ok() == Some((e.tam, e.crc)))
    }
}

pub fn components_root(game: &Path) -> PathBuf {
    game.join(BACKUP_DIR).join(COMPONENTS_DIR)
}

/// Components installed in this game: the ModLoader first, then mods by install time.
pub fn find_components(game: &Path) -> Vec<Component> {
    let mut out = Vec::new();
    let Ok(rd) = std::fs::read_dir(components_root(game)) else { return out };
    for e in rd.flatten() {
        let dir = e.path();
        if let Some(record) = std::fs::read_to_string(dir.join(COMPONENT_FILE)).ok().and_then(|t| serde_json::from_str::<ComponentRecord>(&t).ok()) {
            out.push(Component { dir, record });
        }
    }
    out.sort_by(|a, b| (!a.is_loader(), a.record.instalado_en, &a.record.id).cmp(&(!b.is_loader(), b.record.instalado_en, &b.record.id)));
    out
}

/// Mods installed by this installer (for the uninstall planning).
pub fn installed_mods(game: &Path) -> Vec<InstalledMod> {
    find_components(game)
        .into_iter()
        .filter(|c| !c.is_loader())
        .map(|c| InstalledMod { id: c.record.id, version: c.record.version, requires: c.record.requires, provides: c.record.provides })
        .collect()
}

/// Mods present in `<game>\mods\` (any origin), with a readable `mod.toml`.
pub fn mods_on_disk(game: &Path) -> Vec<Avail> {
    let mut out = Vec::new();
    let Ok(rd) = std::fs::read_dir(game.join(MODS_DIR)) else { return out };
    for e in rd.flatten() {
        let Ok(t) = std::fs::read_to_string(e.path().join(evt_modfmt::MANIFEST)) else { continue };
        if let Ok(m) = modpack::parse_mod_toml(&t) {
            out.push(Avail { id: m.id, version: m.version, provides: m.provides });
        }
    }
    out.sort_by(|a, b| a.id.cmp(&b.id));
    out
}

/// The ModLoader in the game: our record (when winmm.dll is still the one we wrote), else the version marker of
/// the DLL, else unknown.
pub fn loader_state(game: &Path) -> LoaderState {
    let dll = game.join("winmm.dll");
    if !dll.is_file() {
        return LoaderState::Missing;
    }
    let comp = find_components(game).into_iter().find(|c| c.is_loader());
    if let Some(c) = &comp {
        let rec = c.record.extras.iter().find(|e| e.ruta.eq_ignore_ascii_case("winmm.dll"));
        if rec.is_some_and(|e| file_crc(&dll).ok() == Some((e.tam, e.crc))) && c.record.estado == "instalado" {
            return LoaderState::Known { version: c.record.version.clone(), ours: true };
        }
    }
    match std::fs::read(&dll).ok().and_then(|b| modpack::scan_version_marker(&b)) {
        Some(version) => LoaderState::Known { version, ours: comp.is_some() },
        None => LoaderState::Unknown { ours: comp.is_some() },
    }
}

fn read_component(dir: &Path) -> Result<ComponentRecord, String> {
    serde_json::from_str(&io(std::fs::read_to_string(dir.join(COMPONENT_FILE)), dir)?).map_err(|e| format!("{}: {e}", dir.display()))
}

fn write_component(dir: &Path, r: &ComponentRecord) -> Result<(), String> {
    io(std::fs::create_dir_all(dir), dir)?;
    let p = dir.join(COMPONENT_FILE);
    let tmp = dir.join(format!("{COMPONENT_FILE}.tmp"));
    io(std::fs::write(&tmp, serde_json::to_string_pretty(r).map_err(|e| e.to_string())?), &tmp)?;
    io(std::fs::rename(&tmp, &p), &p)
}

// ---------------------------------------------------------------- install

#[derive(Debug, Clone, Default)]
pub struct ModsOptions {
    /// Mods to install (`None` = all of the pack); their dependencies in the pack are added.
    pub select: Option<Vec<String>>,
    /// Install what can be installed when some mods lack a dependency (default: refuse everything).
    pub partial: bool,
    /// Never touch the ModLoader (`--conservar-cargador`).
    pub keep_loader: bool,
    /// Replace an older ModLoader even when it meets `loader_min` (`--actualizar-cargador`).
    pub update_loader: bool,
    /// Reinstall mods whose installed version is the same and intact.
    pub reinstall: bool,
}

/// What [`install`] will do (shown before asking).
#[derive(Debug, Default)]
pub struct Prepared {
    pub loader_min: String,
    pub loader: Option<LoaderAction>,
    /// Mods to install or update, dependencies first.
    pub to_install: Vec<String>,
    /// Same version already installed and intact.
    pub unchanged: Vec<String>,
    pub blocked: Vec<(String, String)>,
}

pub fn prepare(game: &Path, pack: &ModPack, opts: &ModsOptions) -> Result<Prepared, String> {
    if !crate::ops::find_installs(game).is_empty() {
        return Err(format!("hay una instalación de {} en formato de paquete único: desinstálala antes", crate::PRODUCT));
    }
    let comps = find_components(game);
    let in_pack = |id: &str| pack.get(id).is_some();
    let avail: Vec<Avail> = mods_on_disk(game).into_iter().filter(|a| !in_pack(&a.id)).collect();
    let plan = modpack::plan_install(&pack.meta.mods, opts.select.as_deref(), &avail)?;
    if !plan.blocked.is_empty() && !opts.partial {
        let l: Vec<String> = plan.blocked.iter().map(|(id, why)| format!("«{id}»: {why}")).collect();
        return Err(format!("faltan dependencias, no se instala nada: {}", l.join("; ")));
    }
    // a data file can belong to one installed mod only
    let mut clash = Vec::new();
    for id in &plan.order {
        let pm = pack.get(id).expect("planned from the pack");
        let mine: BTreeSet<String> = pm.data_paths().iter().map(|p| p.to_lowercase()).collect();
        for c in comps.iter().filter(|c| !c.is_loader() && &c.record.id != id) {
            if let Some(p) = c.record.datos.iter().find(|p| mine.contains(&p.to_lowercase())) {
                clash.push(format!("{p}: «{id}» y «{}» (ya instalado)", c.record.id));
            }
        }
    }
    if !clash.is_empty() {
        return Err(format!("dos mods instalan el mismo archivo de datos: {}", clash.join("; ")));
    }
    let loader_min = modpack::max_version(
        std::iter::once(pack.meta.loader_min.as_str()).chain(plan.order.iter().filter_map(|id| pack.get(id)).map(|m| m.entry.loader_min.as_str())),
    );
    let loader = if plan.order.is_empty() {
        None
    } else {
        Some(modpack::loader_action(&loader_state(game), &loader_min, &pack.meta.cargador.version, opts.keep_loader, opts.update_loader)?)
    };
    let mut out = Prepared { loader_min, loader, blocked: plan.blocked, ..Prepared::default() };
    for id in plan.order {
        let pm = pack.get(&id).expect("planned from the pack");
        let same = comps.iter().find(|c| c.record.id == id).is_some_and(|c| c.record.version == pm.entry.version && c.intact(game));
        if same && !opts.reinstall {
            out.unchanged.push(id);
        } else {
            out.to_install.push(id);
        }
    }
    Ok(out)
}

#[derive(Debug, Default)]
pub struct ModsOutcome {
    /// What happened to the ModLoader.
    pub loader: String,
    pub loader_installed: bool,
    /// `id version` of each mod installed / updated, in install order.
    pub installed: Vec<String>,
    pub unchanged: Vec<String>,
    pub blocked: Vec<(String, String)>,
    pub data_files: usize,
    pub new_entries: usize,
    pub modified_entries: usize,
    pub modules_changed: Vec<String>,
    pub warnings: Vec<String>,
}

/// Install the ModLoader when needed, then the mods of `pack` (dependencies first). The caller checked that the game
/// is closed. No game file changes before every delta of every mod has been applied into the staging folder.
pub fn install(game: &Path, pack: &ModPack, opts: &ModsOptions, log: &mut dyn FnMut(&str)) -> Result<ModsOutcome, String> {
    let prep = prepare(game, pack, opts)?;
    let mut out = ModsOutcome { unchanged: prep.unchanged.clone(), blocked: prep.blocked.clone(), ..ModsOutcome::default() };
    let staging = pack.root.join(STAGING);
    let res = (|| {
        // 0. Every mod's data staged first (deltas applied to the player's retail files).
        let mut projects: Vec<(String, Option<PathBuf>)> = Vec::new();
        let comps = find_components(game);
        for id in &prep.to_install {
            let pm = pack.get(id).expect("prepared from the pack");
            // an installed version of this mod replaced some retail files: their originals are in its backup
            let backed: std::collections::HashMap<String, Orig> = comps.iter().find(|c| &c.record.id == id).map(originals_of).unwrap_or_default();
            let project = if !pm.has_data() {
                None
            } else if pm.patches.is_empty() {
                Some(pack.mod_dir(id))
            } else {
                log(&format!("«{id}»: aplicando {} parches a tus archivos originales del juego...", pm.patches.len()));
                let st = staging.join(id);
                let src = |path: &str, list: &vr_gamefiles::cpk_list::CpkList, by_path: &std::collections::HashMap<String, usize>| {
                    backed.get(&path.to_lowercase()).map(|o| o.bytes(game, path, list, by_path))
                };
                crate::ops::stage_into(game, &pack.mod_dir(id), &pm.data, &pm.patches, &st, Some(&src), log)?;
                Some(st)
            };
            projects.push((id.clone(), project));
        }
        // 1. ModLoader.
        match &prep.loader {
            Some(LoaderAction::Install(why)) => {
                log(&format!("Instalando el ModLoader {} ({why})...", pack.meta.cargador.version));
                install_loader(game, pack)?;
                out.loader = format!("ModLoader {} instalado ({why})", pack.meta.cargador.version);
                out.loader_installed = true;
            }
            Some(LoaderAction::Keep(why)) => out.loader = format!("ModLoader: {why}"),
            None => {}
        }
        // 2. Mods, dependencies first; an installed older / broken copy is uninstalled first.
        for (id, project) in &projects {
            let pm = pack.get(id).expect("prepared from the pack");
            if let Some(c) = find_components(game).into_iter().find(|c| &c.record.id == id) {
                log(&format!("«{id}»: quitando la versión instalada ({})...", c.record.version));
                let u = uninstall_component(game, &c, log)?;
                out.warnings.extend(u.warnings);
            }
            log(&format!("«{id}» {}: instalando...", pm.entry.version));
            let r = install_mod(game, pack, pm, project.as_deref(), log)?;
            out.data_files += r.0;
            out.new_entries += r.1;
            out.modified_entries += r.2;
            out.warnings.extend(r.3);
            out.installed.push(format!("{id} {}", pm.entry.version));
        }
        // 3. Loader module switches of the pack (only on a ModLoader this installer owns).
        if !pack.modules.is_empty() && !prep.to_install.is_empty() {
            match find_components(game).into_iter().find(|c| c.is_loader()) {
                Some(c) => out.modules_changed = apply_modules(game, &c, &pack.modules)?,
                None => out.warnings.push("evt_loader_modules.toml del paquete no aplicado: el ModLoader no lo instaló este instalador".into()),
            }
        }
        Ok::<(), String>(())
    })();
    let _ = std::fs::remove_dir_all(&staging);
    res.map(|_| out)
}

/// Minimal reader of vr-gamefiles' `manifest.json` (camelCase): the pre-install state of each installed data file.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct VpEntry {
    cpk_dir: Option<String>,
    cpk_name: Option<String>,
    size: i32,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct VpFile {
    path: String,
    existed: bool,
    #[serde(default)]
    original_entry: Option<VpEntry>,
}

#[derive(Deserialize)]
struct VpManifest {
    files: Vec<VpFile>,
}

/// The retail bytes of a data file an installed mod replaced.
enum Orig {
    /// The file on disk before the install (vr-gamefiles backup `files/<path>`).
    File(PathBuf),
    /// Not on disk before: the original `cpk_list` entry (a CPK file).
    Entry(VpEntry),
    /// A file the mod added: no original.
    New,
}

impl Orig {
    fn bytes(&self, game: &Path, path: &str, list: &vr_gamefiles::cpk_list::CpkList, by_path: &std::collections::HashMap<String, usize>) -> Result<Vec<u8>, String> {
        match self {
            Orig::File(p) => std::fs::read(p).map_err(|e| format!("{}: {e}", p.display())),
            Orig::Entry(e) => {
                let i = *by_path.get(&path.to_lowercase()).ok_or("no está en el cpk_list del juego")?;
                let mut it = list.items[i].clone();
                it.cpk_dir = e.cpk_dir.clone();
                it.cpk_name = e.cpk_name.clone();
                it.size = e.size;
                match it.cpk_path() {
                    Some(cpk) => vr_gamefiles::cpk::extract_from_cpk(&game.join(&cpk), &it.dir, &it.name).map_err(|e| e.to_string()),
                    None => Err("la copia de seguridad del mod instalado no tiene el original".into()),
                }
            }
            Orig::New => Err("archivo nuevo de la versión instalada del mod: no hay original".into()),
        }
    }
}

/// Originals of the data files a component installed (lowercase path → source), from its vr-gamefiles backups.
fn originals_of(c: &Component) -> std::collections::HashMap<String, Orig> {
    let mut out = std::collections::HashMap::new();
    let Ok(rd) = std::fs::read_dir(c.dir.join(DATA_BACKUPS)) else { return out };
    for e in rd.flatten() {
        let Some(m) = std::fs::read_to_string(e.path().join("manifest.json")).ok().and_then(|t| serde_json::from_str::<VpManifest>(&t).ok()) else { continue };
        for f in m.files {
            let o = if f.existed {
                Orig::File(e.path().join("files").join(&f.path))
            } else {
                match f.original_entry {
                    Some(en) => Orig::Entry(en),
                    None => Orig::New,
                }
            };
            out.insert(f.path.to_lowercase(), o);
        }
    }
    out
}

/// Game folders of `rel` (a file path) that do not exist, added to `set` (parents included).
fn note_created(game: &Path, rel: &str, set: &mut BTreeSet<String>) {
    let mut anc = Path::new(rel).parent();
    while let Some(a) = anc.filter(|a| !a.as_os_str().is_empty()) {
        if game.join(a).is_dir() || !set.insert(a.to_string_lossy().replace('\\', "/")) {
            break;
        }
        anc = a.parent();
    }
}

/// Add `rel` to the component's files: its original saved when it exists (once: a file already listed keeps the
/// first original).
fn track(game: &Path, dir: &Path, rec: &mut ComponentRecord, created: &mut BTreeSet<String>, rel: &str) -> Result<(), String> {
    if rec.extras.iter().any(|e| e.ruta.eq_ignore_ascii_case(rel)) {
        return Ok(());
    }
    let target = game.join(rel);
    let existia = target.is_file();
    if existia {
        copy_file(&target, &dir.join(ORIGINALS).join(rel))?;
    }
    note_created(game, rel, created);
    rec.extras.push(Extra { ruta: rel.to_string(), existia, tam: 0, crc: 0 });
    Ok(())
}

fn finish(game: &Path, dir: &Path, rec: &mut ComponentRecord) -> Result<(), String> {
    for e in &mut rec.extras {
        let p = game.join(&e.ruta);
        if p.is_file() {
            let (tam, crc) = io(file_crc(&p), &p)?;
            e.tam = tam;
            e.crc = crc;
        }
    }
    rec.estado = "instalado".into();
    write_component(dir, rec)
}

/// Install or update only the ModLoader of `pack` (a pack with `[cargador]` and no mods is enough; used by the
/// VR-ModLoader manager). `update` = also replace an older ModLoader (as `--actualizar-cargador`); without it an
/// installed ModLoader of known version is kept. Returns what was done. The caller checked that the game is closed.
pub fn install_loader_only(game: &Path, pack: &ModPack, update: bool, log: &mut dyn FnMut(&str)) -> Result<(LoaderAction, String), String> {
    if !crate::ops::find_installs(game).is_empty() {
        return Err(format!("hay una instalación de {} en formato de paquete único: desinstálala antes", crate::PRODUCT));
    }
    let action = modpack::loader_action(&loader_state(game), &pack.loader_min(), &pack.meta.cargador.version, false, update)?;
    let msg = match &action {
        LoaderAction::Install(why) => {
            log(&format!("Instalando el ModLoader {} ({why})...", pack.meta.cargador.version));
            install_loader(game, pack)?;
            format!("ModLoader {} instalado ({why})", pack.meta.cargador.version)
        }
        LoaderAction::Keep(why) => format!("ModLoader: {why}"),
    };
    Ok((action, msg))
}

/// The ModLoader files of the pack; an existing component of ours is updated in place (its originals kept, the
/// player's `evt_loader\config.toml` kept).
fn install_loader(game: &Path, pack: &ModPack) -> Result<(), String> {
    let dir = components_root(game).join(LOADER_ID);
    let existing = read_component(&dir).ok();
    let fresh = existing.is_none();
    let mut rec = existing.unwrap_or_else(|| ComponentRecord { formato: RECORD_FORMAT, tipo: "cargador".into(), id: LOADER_ID.into(), ..Default::default() });
    rec.version = pack.meta.cargador.version.clone();
    rec.producto = pack.meta.producto.clone();
    rec.paquete = pack.meta.version.clone();
    rec.estado = "instalando".into();
    rec.instalado_en = crate::now_secs();
    let mut created: BTreeSet<String> = rec.carpetas_creadas.iter().cloned().collect();
    for rel in &pack.loader_files {
        track(game, &dir, &mut rec, &mut created, rel)?;
    }
    rec.carpetas_creadas = created.into_iter().collect();
    write_component(&dir, &rec)?;
    for rel in &pack.loader_files {
        if !fresh && rel.eq_ignore_ascii_case(LOADER_CONFIG) && game.join(rel).is_file() {
            continue; // an update keeps the player's settings
        }
        copy_file(&pack.root.join(LOADER_DIR).join(rel), &game.join(rel))?;
    }
    finish(game, &dir, &mut rec)
}

/// `[modules]` switches into `evt_loader\config.toml` (tracked by the ModLoader component).
pub fn apply_modules(game: &Path, loader: &Component, modules: &[(String, bool)]) -> Result<Vec<String>, String> {
    let cfg = game.join(LOADER_CONFIG);
    if !cfg.is_file() {
        return Ok(Vec::new());
    }
    let mut rec = loader.record.clone();
    let mut created: BTreeSet<String> = rec.carpetas_creadas.iter().cloned().collect();
    track(game, &loader.dir, &mut rec, &mut created, LOADER_CONFIG)?;
    write_component(&loader.dir, &rec)?;
    let text = io(std::fs::read_to_string(&cfg), &cfg)?;
    let (new, changed) = loadercfg::apply_modules(&text, modules)?;
    if new != text {
        io(std::fs::write(&cfg, new), &cfg)?;
    }
    finish(game, &loader.dir, &mut rec)?;
    Ok(changed)
}

/// Ids of the mod folders in `<game>\mods\` (with a mod.toml), sorted.
fn mod_folders(game: &Path) -> Vec<String> {
    let mut v: Vec<String> = std::fs::read_dir(game.join(MODS_DIR))
        .map(|rd| rd.flatten().filter(|e| e.path().join(evt_modfmt::MANIFEST).is_file()).map(|e| e.file_name().to_string_lossy().to_string()).collect())
        .unwrap_or_default();
    v.sort();
    v
}

#[derive(Serialize, Deserialize)]
struct EnabledBefore {
    existia: bool,
    lista: Vec<String>,
}

/// One mod: record, data (vr-gamefiles, backup in `<id>\datos`), files into `mods\<id>\`, enabled.toml.
/// Returns (data files, new cpk_list entries, modified entries, warnings).
fn install_mod(game: &Path, pack: &ModPack, pm: &PackMod, project: Option<&Path>, log: &mut dyn FnMut(&str)) -> Result<(usize, usize, usize, Vec<String>), String> {
    let id = pm.id();
    let root = components_root(game);
    let dir = root.join(id);
    if dir.exists() {
        io(std::fs::remove_dir_all(&dir), &dir)?; // leftover without a readable record
    }
    io(std::fs::create_dir_all(&dir), &dir)?;
    let others = find_components(game);
    let mut warnings = Vec::new();

    // shared originals: the list before the first mod with data, enabled.toml before the first mod we enable
    if pm.has_data() && !others.iter().any(|c| !c.record.datos.is_empty()) && !root.join(LIST).is_file() {
        copy_file(&game.join("data").join(LIST), &root.join(LIST))?;
    }
    let enabled_root = game.join(MODS_DIR);
    if pm.entry.activar && !others.iter().any(|c| c.record.activado) && !root.join(ENABLED_BEFORE).is_file() {
        let cur = evt_modfmt::read_enabled(&enabled_root).ok().flatten();
        let before = EnabledBefore { existia: cur.is_some(), lista: cur.unwrap_or_else(|| mod_folders(game).into_iter().filter(|m| m != id).collect()) };
        if before.existia {
            copy_file(&enabled_root.join(evt_modfmt::ENABLED_FILE), &root.join(ENABLED_COPY))?;
        }
        let p = root.join(ENABLED_BEFORE);
        io(std::fs::write(&p, serde_json::to_string_pretty(&before).map_err(|e| e.to_string())?), &p)?;
    }

    let mut rec = ComponentRecord {
        formato: RECORD_FORMAT,
        tipo: "mod".into(),
        id: id.to_string(),
        version: pm.entry.version.clone(),
        producto: pack.meta.producto.clone(),
        paquete: pack.meta.version.clone(),
        estado: "instalando".into(),
        instalado_en: crate::now_secs(),
        requires: pm.entry.requires.clone(),
        provides: pm.entry.provides.clone(),
        datos: pm.data_paths(),
        ..Default::default()
    };
    let mod_root = format!("{MODS_DIR}/{id}");
    if game.join(&mod_root).is_dir() {
        let ours: BTreeSet<String> = pm.files.iter().map(|f| f.to_lowercase()).collect();
        let extra = walk(&game.join(&mod_root)).into_iter().filter(|f| !ours.contains(&f.to_lowercase())).count();
        if extra > 0 {
            warnings.push(format!("{mod_root}: ya existía; se conservan {extra} archivos que no son del paquete"));
        }
    }
    let mut created = BTreeSet::new();
    for rel in &pm.files {
        track(game, &dir, &mut rec, &mut created, &format!("{mod_root}/{rel}"))?;
    }
    for rel in &rec.datos {
        note_created(game, rel, &mut created);
    }
    rec.carpetas_creadas = created.into_iter().collect();
    write_component(&dir, &rec)?;

    let (mut files, mut new_e, mut mod_e) = (0, 0, 0);
    if let Some(project) = project {
        log(&format!("«{id}»: {} archivos de datos ({})...", rec.datos.len(), crate::human_size(pm.data_bytes)));
        let r = install::install_dirs(game, project, &dir.join(DATA_BACKUPS), InstallOptions { full: true }).map_err(|e| e.to_string())?;
        files = r.files;
        new_e = r.new_entries;
        mod_e = r.modified_entries;
        warnings.extend(r.warnings);
    }
    let src = pack.mod_dir(id).join(MOD_FILES);
    for rel in &pm.files {
        copy_file(&src.join(rel), &game.join(&mod_root).join(rel))?;
    }
    if pm.entry.activar {
        let cur = evt_modfmt::read_enabled(&enabled_root).ok().flatten();
        let list = evt_modfmt::toggle_enabled(cur, &mod_folders(game), id, true);
        let p = enabled_root.join(evt_modfmt::ENABLED_FILE);
        io(std::fs::write(&p, evt_modfmt::enabled_text(&list)), &p)?;
        rec.activado = true;
    }
    rec.avisos = warnings.clone();
    finish(game, &dir, &mut rec)?;
    Ok((files, new_e, mod_e, warnings))
}

/// Files below `root`, relative, `/` separated.
fn walk(root: &Path) -> Vec<String> {
    let mut out = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(d) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&d) else { continue };
        for e in rd.flatten() {
            let p = e.path();
            if p.is_dir() {
                stack.push(p);
            } else if let Ok(r) = p.strip_prefix(root) {
                out.push(r.to_string_lossy().replace('\\', "/"));
            }
        }
    }
    out
}

// ---------------------------------------------------------------- uninstall

/// Undo one component (data, files, enabled.toml), delete its backup, and put back the shared originals when it was
/// the last one using them. Does not look at dependents: see [`uninstall_mods`].
pub fn uninstall_component(game: &Path, c: &Component, log: &mut dyn FnMut(&str)) -> Result<UninstallOutcome, String> {
    let rec = &c.record;
    let mut out = UninstallOutcome::default();
    // 1. Data through vr-gamefiles.
    if let Ok(rd) = std::fs::read_dir(c.dir.join(DATA_BACKUPS)) {
        log(&format!("«{}»: restaurando sus archivos de datos...", rec.id));
        for e in rd.flatten() {
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
    // 2. Files: originals back, ours removed.
    for e in rec.extras.iter().rev() {
        let target = game.join(&e.ruta);
        if e.existia {
            let orig = c.dir.join(ORIGINALS).join(&e.ruta);
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
    // 3. enabled.toml
    let enabled_root = game.join(MODS_DIR);
    if rec.activado {
        if let Ok(Some(cur)) = evt_modfmt::read_enabled(&enabled_root) {
            let list = evt_modfmt::toggle_enabled(Some(cur), &[], &rec.id, false);
            let p = enabled_root.join(evt_modfmt::ENABLED_FILE);
            io(std::fs::write(&p, evt_modfmt::enabled_text(&list)), &p)?;
        }
    }
    // 4. Folders it created, when empty.
    let mut dirs = rec.carpetas_creadas.clone();
    dirs.sort_by_key(|d| std::cmp::Reverse(d.matches('/').count()));
    let mut kept: Vec<String> = Vec::new();
    for d in dirs {
        let p = game.join(&d);
        if p.is_dir() && std::fs::remove_dir(&p).is_err() {
            kept.push(d);
        }
    }
    // 5. Its backup, then the shared originals if nothing uses them any more (this may empty `mods\`).
    io(std::fs::remove_dir_all(&c.dir), &c.dir)?;
    restore_shared(game, &mut out)?;
    // 6. A folder it created that still holds files of other components (e.g. `mods\`) is handed over to one of
    //    them, so the last one out removes it; anything else left inside is the player's.
    kept.sort_by_key(|d| d.matches('/').count()); // parents first: shallow folders are handed over as a whole
    for d in kept {
        let p = game.join(&d);
        if !p.is_dir() || std::fs::remove_dir(&p).is_ok() {
            continue;
        }
        let pre = format!("{}/", d.to_lowercase());
        let under = |r: &str| r.to_lowercase().starts_with(&pre);
        let heir = find_components(game).into_iter().rev().find(|o| {
            o.record.extras.iter().any(|e| under(&e.ruta)) || o.record.datos.iter().any(|x| under(x)) || (d == MODS_DIR && o.record.activado)
        });
        match heir {
            Some(mut h) => {
                if !h.record.carpetas_creadas.contains(&d) {
                    h.record.carpetas_creadas.push(d);
                    write_component(&h.dir, &h.record)?;
                }
            }
            None => out.warnings.push(format!("{d}: se conserva (contiene archivos creados al jugar)")),
        }
    }
    Ok(out)
}

fn restore_shared(game: &Path, out: &mut UninstallOutcome) -> Result<(), String> {
    let root = components_root(game);
    let left = find_components(game);
    let saved = root.join(LIST);
    if saved.is_file() && !left.iter().any(|c| !c.record.datos.is_empty()) {
        let list = game.join("data").join(LIST);
        // same entries (in the game's sort order) = our entries are all gone: put the original bytes back
        let plain = |p: &Path| {
            vr_gamefiles::cpk::read_list(p)
                .map(|mut l| {
                    l.sort();
                    l.to_bytes()
                })
                .ok()
        };
        match (plain(&saved), plain(&list)) {
            (Some(a), Some(b)) if a == b => {
                if std::fs::read(&saved).ok() != std::fs::read(&list).ok() {
                    copy_file(&saved, &list)?;
                }
                out.list_exact = true;
            }
            _ => out.warnings.push("data/cpk_list.cfg.bin: otro programa lo cambió después de instalar; se han quitado solo nuestras entradas".into()),
        }
        io(std::fs::remove_file(&saved), &saved)?;
    }
    let before = root.join(ENABLED_BEFORE);
    if before.is_file() && !left.iter().any(|c| c.record.activado) {
        let enabled_root = game.join(MODS_DIR);
        let b: Option<EnabledBefore> = std::fs::read_to_string(&before).ok().and_then(|t| serde_json::from_str(&t).ok());
        let cur = evt_modfmt::read_enabled(&enabled_root).ok().flatten();
        if let (Some(b), Some(cur)) = (b, cur) {
            if cur == b.lista {
                let p = enabled_root.join(evt_modfmt::ENABLED_FILE);
                if b.existia {
                    copy_file(&root.join(ENABLED_COPY), &p)?;
                } else {
                    io(std::fs::remove_file(&p), &p)?;
                    let _ = std::fs::remove_dir(&enabled_root); // only when empty
                }
            }
        }
        let _ = std::fs::remove_file(root.join(ENABLED_COPY));
        io(std::fs::remove_file(&before), &before)?;
    }
    if std::fs::read_dir(&root).is_ok_and(|mut rd| rd.next().is_none()) {
        let _ = std::fs::remove_dir(&root);
    }
    let b = game.join(BACKUP_DIR);
    if std::fs::read_dir(&b).is_ok_and(|mut rd| rd.next().is_none()) {
        let _ = std::fs::remove_dir(&b);
    }
    Ok(())
}

/// Uninstall mods by id. Refused when an installed mod needs one of them, unless `cascade` (then those go first).
pub fn uninstall_mods(game: &Path, ids: &[String], cascade: bool, log: &mut dyn FnMut(&str)) -> Result<Vec<(String, UninstallOutcome)>, String> {
    let order = modpack::uninstall_order(ids, &installed_mods(game), cascade)?;
    let mut out = Vec::new();
    for id in order {
        let Some(c) = find_components(game).into_iter().find(|c| c.record.id == id) else { continue };
        log(&format!("Quitando «{id}» {}...", c.record.version));
        out.push((id, uninstall_component(game, &c, log)?));
    }
    Ok(out)
}

/// Uninstall the ModLoader component (only when no mod of ours is left).
pub fn uninstall_loader(game: &Path, log: &mut dyn FnMut(&str)) -> Result<Option<UninstallOutcome>, String> {
    let comps = find_components(game);
    if let Some(m) = comps.iter().find(|c| !c.is_loader()) {
        return Err(format!("quedan mods instalados (p. ej. «{}»): quítalos antes que el ModLoader", m.record.id));
    }
    match comps.into_iter().find(|c| c.is_loader()) {
        Some(c) => {
            log("Quitando el ModLoader...");
            uninstall_component(game, &c, log).map(Some)
        }
        None => Ok(None),
    }
}

/// Every mod (dependents first), then the ModLoader.
pub fn uninstall_all(game: &Path, log: &mut dyn FnMut(&str)) -> Result<Vec<(String, UninstallOutcome)>, String> {
    let ids: Vec<String> = installed_mods(game).into_iter().map(|m| m.id).collect();
    let mut out = uninstall_mods(game, &ids, true, log)?;
    if let Some(u) = uninstall_loader(game, log)? {
        out.push((LOADER_ID.to_string(), u));
    }
    Ok(out)
}
