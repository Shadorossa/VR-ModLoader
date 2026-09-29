//! Pack format 2, the **mods layout** (ModLoader transition, phase 5: `docs/app/instalador-mods.md`).
//!
//! ```text
//! pack.toml                 formato = 2, product, version, loader_min, [juego], [cargador], [[mod]] (one per mod)
//! indice.json               every shipped file (cargador/**, mods/**) with size + CRC-32 (as in format 1)
//! evt_loader_modules.toml   optional: `[modules]` switches applied to evt_loader\config.toml when the ModLoader is ours
//! cargador/**               the ModLoader: winmm.dll, vr_loader.pdb, steam_appid.txt, evt_loader/** (base files)
//! mods/<id>/mod/**          copied into <game>\mods\<id>\ (mod.toml, DLL, lua\, files\, config defaults)
//! mods/<id>/data/**         whole data files installed loose + cpk_list (vr-gamefiles), grouped per mod
//! mods/<id>/delta/**.evtd   deltas against the player's retail file
//! mods/<id>/parches.json    the mod's deltas (`delta` relative to mods/<id>/, as `evt-pack datos` writes it)
//! ```
//! Format 1 (single bundle, [`crate::pack`]) stays the default until phase 4; the installer picks the mode from
//! `formato`. Pure planning (dependency order, uninstall sets, loader decision) lives here and is unit tested.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::pack::{GameBuild, IndexFile, PackFile, Patch, PatchesFile, INDEX_FILE, META_FILE, MODULES_FILE};

pub const FORMAT_MODS: u32 = 2;
/// ModLoader files of the pack (game-relative below it).
pub const LOADER_DIR: &str = "cargador";
pub const MODS_DIR: &str = "mods";
/// Below `mods/<id>/`: what is copied into `<game>\mods\<id>\`.
pub const MOD_FILES: &str = "mod";
/// Below `mods/<id>/`: the mod's deltas.
pub const MOD_PATCHES: &str = "parches.json";
/// `winmm.dll` embeds `EVT_MODLOADER_VERSION=<version>` (proposed for vr-loader; see [`scan_version_marker`]).
pub const VERSION_MARKER: &[u8] = b"EVT_MODLOADER_VERSION=";

// ---------------------------------------------------------------- pack.toml v2

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ModPackMeta {
    pub producto: String,
    pub version: String,
    #[serde(default)]
    pub formato: u32,
    #[serde(default)]
    pub compilado: String,
    #[serde(default)]
    pub commit: String,
    /// Lowest ModLoader the pack needs (the installer also takes the highest `loader_min` of its mods).
    #[serde(default)]
    pub loader_min: String,
    #[serde(default)]
    pub juego: GameBuild,
    #[serde(default)]
    pub cargador: LoaderMeta,
    #[serde(default, rename = "mod")]
    pub mods: Vec<ModEntry>,
}

/// `[cargador]`: the ModLoader shipped in `cargador/` (empty version = the pack ships none).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct LoaderMeta {
    #[serde(default)]
    pub version: String,
}

/// `[[mod]]` of pack.toml: copied from the mod's `mod.toml` by the pack builder (checked against it by
/// [`ModPack::validate`]), plus `activar`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ModEntry {
    pub id: String,
    pub version: String,
    #[serde(default)]
    pub nombre: String,
    /// `id` or `id<op>version` (`>=`, `>`, `=`, `<=`, `<`), as `mod.toml`.
    #[serde(default)]
    pub requires: Vec<String>,
    /// `name` or `name=version`.
    #[serde(default)]
    pub provides: Vec<String>,
    #[serde(default)]
    pub loader_min: String,
    #[serde(default)]
    pub plugin: String,
    /// Switch the mod on in `mods\enabled.toml` (default yes).
    #[serde(default = "yes")]
    pub activar: bool,
}

fn yes() -> bool {
    true
}

// ---------------------------------------------------------------- mod.toml (installer's own reader)

/// The `mod.toml` fields the installer needs. Tolerant reader (unknown keys ignored) so it works with the committed
/// and the phase-1 `evt-modfmt`; TODO unify with `evt_modfmt::parse_manifest` once `plugin` / `provides` /
/// `loader_min` are committed there.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct ModToml {
    pub id: String,
    #[serde(default)]
    pub version: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub requires: Vec<String>,
    #[serde(default)]
    pub provides: Vec<String>,
    #[serde(default)]
    pub loader_min: String,
    #[serde(default)]
    pub plugin: String,
}

pub fn parse_mod_toml(text: &str) -> Result<ModToml, String> {
    let m: ModToml = toml::from_str(text).map_err(|e| e.message().to_string())?;
    evt_modfmt::validate_id(&m.id)?;
    for r in &m.requires {
        evt_modfmt::parse_requirement(r)?;
    }
    Ok(m)
}

/// `name` / `name=version` of a `provides` entry.
pub fn split_provide(s: &str) -> (String, Option<String>) {
    match s.split_once('=') {
        Some((n, v)) => (n.trim().to_string(), Some(v.trim().to_string()).filter(|v| !v.is_empty())),
        None => (s.trim().to_string(), None),
    }
}

// ---------------------------------------------------------------- the pack

/// One mod of the pack with its files.
#[derive(Debug, Clone)]
pub struct PackMod {
    pub entry: ModEntry,
    /// Paths relative to `mods/<id>/mod/` (= relative to `<game>\mods\<id>\`).
    pub files: Vec<String>,
    pub files_bytes: u64,
    /// Whole data files, game paths (`data/...`), shipped at `mods/<id>/data/...`.
    pub data: Vec<String>,
    /// Deltas; `delta` relative to `mods/<id>/`.
    pub patches: Vec<Patch>,
    /// Bytes installed into the game's data (whole + patch results).
    pub data_bytes: u64,
}

impl PackMod {
    pub fn id(&self) -> &str {
        &self.entry.id
    }
    /// Every game data path this mod installs (whole + patched).
    pub fn data_paths(&self) -> Vec<String> {
        self.data.iter().cloned().chain(self.patches.iter().map(|p| p.ruta.clone())).collect()
    }
    pub fn has_data(&self) -> bool {
        !self.data.is_empty() || !self.patches.is_empty()
    }
}

pub struct ModPack {
    pub root: PathBuf,
    pub meta: ModPackMeta,
    pub files: Vec<PackFile>,
    /// Game-relative paths of the ModLoader files (below `cargador/`).
    pub loader_files: Vec<String>,
    pub loader_bytes: u64,
    /// In `meta.mods` order.
    pub mods: Vec<PackMod>,
    /// `[modules]` switches of `evt_loader_modules.toml` (empty = none).
    pub modules: Vec<(String, bool)>,
}

fn read(p: &Path) -> Result<String, String> {
    std::fs::read_to_string(p).map_err(|e| format!("{}: {e}", p.display()))
}

/// `formato` of the pack folder (`0` / `1` = single bundle).
pub fn pack_format(root: &Path) -> Result<u32, String> {
    #[derive(Deserialize)]
    struct F {
        #[serde(default)]
        formato: u32,
    }
    let f: F = toml::from_str(&read(&root.join(META_FILE))?).map_err(|e| format!("{META_FILE}: {e}"))?;
    Ok(f.formato)
}

impl ModPack {
    pub fn open(root: &Path) -> Result<ModPack, String> {
        let meta = parse_meta(&read(&root.join(META_FILE))?)?;
        let index: IndexFile = serde_json::from_str(&read(&root.join(INDEX_FILE))?).map_err(|e| format!("{INDEX_FILE}: {e}"))?;
        let mut loader_files = Vec::new();
        let mut loader_bytes = 0;
        let lp = format!("{LOADER_DIR}/");
        for f in &index.archivos {
            if let Some(rel) = f.ruta.strip_prefix(&lp) {
                loader_files.push(rel.to_string());
                loader_bytes += f.tam;
            }
        }
        let mut mods = Vec::new();
        for e in &meta.mods {
            let base = format!("{MODS_DIR}/{}/", e.id);
            let files_pre = format!("{base}{MOD_FILES}/");
            let data_pre = format!("{base}data/");
            let mut m = PackMod { entry: e.clone(), files: Vec::new(), files_bytes: 0, data: Vec::new(), patches: Vec::new(), data_bytes: 0 };
            for f in &index.archivos {
                if let Some(rel) = f.ruta.strip_prefix(&files_pre) {
                    m.files.push(rel.to_string());
                    m.files_bytes += f.tam;
                } else if f.ruta.starts_with(&data_pre) {
                    m.data.push(f.ruta[base.len()..].to_string());
                    m.data_bytes += f.tam;
                }
            }
            let pp = root.join(MODS_DIR).join(&e.id).join(MOD_PATCHES);
            if pp.is_file() {
                let pf: PatchesFile = serde_json::from_str(&read(&pp)?).map_err(|x| format!("{}: {x}", pp.display()))?;
                m.data_bytes += pf.parches.iter().map(|p| p.tam).sum::<u64>();
                m.patches = pf.parches;
            }
            mods.push(m);
        }
        let modules = std::fs::read_to_string(root.join(MODULES_FILE)).map(|t| crate::loadercfg::parse_module_list(&t)).unwrap_or_default();
        Ok(ModPack { root: root.to_path_buf(), meta, files: index.archivos, loader_files, loader_bytes, mods, modules })
    }

    pub fn get(&self, id: &str) -> Option<&PackMod> {
        self.mods.iter().find(|m| m.entry.id == id)
    }

    /// Folder of a mod inside the pack (`mods/<id>`): the project folder of its data install, and the root its
    /// patches' `delta` paths are relative to.
    pub fn mod_dir(&self, id: &str) -> PathBuf {
        self.root.join(MODS_DIR).join(id)
    }

    /// The ModLoader version required: the highest of the pack's and its mods' `loader_min` (empty = any).
    pub fn loader_min(&self) -> String {
        max_version(std::iter::once(self.meta.loader_min.as_str()).chain(self.mods.iter().map(|m| m.entry.loader_min.as_str())))
    }

    /// Pack errors (empty = well formed).
    pub fn validate(&self) -> Vec<String> {
        let mut errs = Vec::new();
        if self.meta.formato != FORMAT_MODS {
            errs.push(format!("formato {} (este lector es el del formato {FORMAT_MODS})", self.meta.formato));
        }
        let has = |p: &str| self.files.iter().any(|f| f.ruta.eq_ignore_ascii_case(p));
        let shipped = self.meta.cargador.version.trim();
        if !self.loader_files.is_empty() && !self.loader_files.iter().any(|f| f.eq_ignore_ascii_case("winmm.dll")) {
            errs.push(format!("{LOADER_DIR}/ sin winmm.dll"));
        }
        if self.loader_files.is_empty() != shipped.is_empty() {
            errs.push(format!("[cargador] version y {LOADER_DIR}/ tienen que ir juntos"));
        }
        let min = self.loader_min();
        if !shipped.is_empty() && !min.is_empty() && evt_modfmt::compare_versions(shipped, &min).is_lt() {
            errs.push(format!("el ModLoader del paquete ({shipped}) es más viejo que el que piden los mods ({min})"));
        }
        let mut seen = BTreeSet::new();
        let mut owner: BTreeMap<String, String> = BTreeMap::new();
        for m in &self.mods {
            let id = m.id();
            if let Err(e) = evt_modfmt::validate_id(id) {
                errs.push(e);
            }
            if !seen.insert(id.to_string()) {
                errs.push(format!("mod «{id}» repetido"));
            }
            for r in &m.entry.requires {
                if let Err(e) = evt_modfmt::parse_requirement(r) {
                    errs.push(format!("mod «{id}»: requires {r:?}: {e}"));
                }
            }
            let mt = format!("{MODS_DIR}/{id}/{MOD_FILES}/{}", evt_modfmt::MANIFEST);
            if !has(&mt) {
                errs.push(format!("mod «{id}»: falta {mt}"));
            } else if let Ok(text) = std::fs::read_to_string(self.root.join(&mt)) {
                match parse_mod_toml(&text) {
                    Ok(t) if t.id != id => errs.push(format!("mod «{id}»: su mod.toml dice id = {:?}", t.id)),
                    Ok(t) if t.version != m.entry.version => {
                        errs.push(format!("mod «{id}»: pack.toml dice versión {} y su mod.toml {}", m.entry.version, t.version))
                    }
                    Ok(_) => {}
                    Err(e) => errs.push(format!("mod «{id}»: mod.toml: {e}")),
                }
            }
            if !m.entry.plugin.is_empty() && !has(&format!("{MODS_DIR}/{id}/{MOD_FILES}/{}", m.entry.plugin)) {
                errs.push(format!("mod «{id}»: falta su plugin {}", m.entry.plugin));
            }
            for p in &m.patches {
                if !has(&format!("{MODS_DIR}/{id}/{}", p.delta)) {
                    errs.push(format!("mod «{id}» {}: falta su delta {}", p.ruta, p.delta));
                }
                if m.data.iter().any(|d| d.eq_ignore_ascii_case(&p.ruta)) {
                    errs.push(format!("mod «{id}» {}: está entero y como delta a la vez", p.ruta));
                }
                if !p.ruta.to_lowercase().starts_with("data/") {
                    errs.push(format!("mod «{id}» {}: un delta solo puede ser de data/", p.ruta));
                }
            }
            for d in m.data_paths() {
                if let Some(o) = owner.insert(d.to_lowercase(), id.to_string()) {
                    errs.push(format!("{d}: lo instalan dos mods ({o} y {id}); cada archivo de datos tiene que ser de un solo mod"));
                }
            }
        }
        if let Err(e) = plan_install(&self.meta.mods, None, &[]).map(|p| p.blocked) {
            errs.push(e);
        }
        errs
    }

    /// Files missing from the pack folder or with another size (a partial extraction of the zip).
    pub fn check_files(&self) -> Vec<String> {
        crate::pack::check_index(&self.root, &self.files)
    }
}

pub fn parse_meta(text: &str) -> Result<ModPackMeta, String> {
    toml::from_str(text).map_err(|e| format!("{META_FILE}: {e}"))
}

/// Highest version of a list (empty entries ignored; empty result = none).
pub fn max_version<'a>(it: impl Iterator<Item = &'a str>) -> String {
    let mut best = String::new();
    for v in it.map(str::trim).filter(|v| !v.is_empty()) {
        if best.is_empty() || evt_modfmt::compare_versions(v, &best).is_gt() {
            best = v.to_string();
        }
    }
    best
}

// ---------------------------------------------------------------- dependency planning

/// A mod that can satisfy a requirement without being installed from the pack: already in `<game>\mods\`.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Avail {
    pub id: String,
    pub version: String,
    pub provides: Vec<String>,
}

impl Avail {
    fn of_entry(e: &ModEntry) -> Avail {
        Avail { id: e.id.clone(), version: e.version.clone(), provides: e.provides.clone() }
    }
}

/// `req` is satisfied by this mod: its id, or something it provides (with the provided version, else its own).
pub fn satisfies(a: &Avail, req: &evt_modfmt::Requirement) -> bool {
    if a.id == req.id {
        return req.accepts(&a.version);
    }
    a.provides.iter().any(|p| {
        let (name, v) = split_provide(p);
        name == req.id && req.accepts(v.as_deref().unwrap_or(&a.version))
    })
}

#[derive(Debug, Default, PartialEq)]
pub struct InstallPlan {
    /// Mods of the pack to install, dependencies first.
    pub order: Vec<String>,
    /// Mods that cannot be installed: (id, reason). Their dependents are blocked too.
    pub blocked: Vec<(String, String)>,
}

/// Which mods of the pack to install and in which order. `selected` = ids chosen by the player (`None` = all);
/// their dependencies in the pack are added. A requirement is met by a mod of the pack (installed before) or, when
/// no pack mod meets it, by an installed mod (`installed`). Unknown selection or a dependency cycle = `Err`.
pub fn plan_install(pack: &[ModEntry], selected: Option<&[String]>, installed: &[Avail]) -> Result<InstallPlan, String> {
    let pos = |id: &str| pack.iter().position(|m| m.id == id);
    let mut want: Vec<usize> = match selected {
        None => (0..pack.len()).collect(),
        Some(s) => s.iter().map(|id| pos(id).ok_or_else(|| format!("el paquete no tiene el mod «{id}»"))).collect::<Result<_, _>>()?,
    };
    let pack_avail: Vec<Avail> = pack.iter().map(Avail::of_entry).collect();
    // closure over pack dependencies; edges[i] = pack mods i needs
    let mut edges: BTreeMap<usize, Vec<usize>> = BTreeMap::new();
    let mut missing: BTreeMap<usize, Vec<String>> = BTreeMap::new();
    let mut k = 0;
    while k < want.len() {
        let i = want[k];
        k += 1;
        if edges.contains_key(&i) {
            continue;
        }
        let mut deps = Vec::new();
        for r in &pack[i].requires {
            let req = evt_modfmt::parse_requirement(r).map_err(|e| format!("mod «{}»: {e}", pack[i].id))?;
            if let Some(j) = pack_avail.iter().position(|a| a.id != pack[i].id && satisfies(a, &req)) {
                deps.push(j);
                if !want.contains(&j) {
                    want.push(j);
                }
            } else if !installed.iter().any(|a| satisfies(a, &req)) {
                let have = pack_avail.iter().chain(installed).find(|a| a.id == req.id).map(|a| format!(" (hay la {})", a.version)).unwrap_or_default();
                missing.entry(i).or_default().push(format!("{}{}{have}", req.id, if req.op.is_some() { format!(" {}", req.constraint()) } else { String::new() }));
            }
        }
        edges.insert(i, deps);
    }
    // topological order (Kahn), ties by pack order
    let nodes: Vec<usize> = edges.keys().copied().collect();
    let mut indeg: BTreeMap<usize, usize> = nodes.iter().map(|&n| (n, edges[&n].len())).collect();
    let mut order: Vec<usize> = Vec::new();
    loop {
        let Some(&n) = indeg.iter().filter(|(_, &d)| d == 0).map(|(n, _)| n).next() else { break };
        indeg.remove(&n);
        order.push(n);
        for (m, deps) in &edges {
            if indeg.contains_key(m) {
                let c = deps.iter().filter(|&&d| d == n).count();
                *indeg.get_mut(m).unwrap() -= c;
            }
        }
    }
    if !indeg.is_empty() {
        let ids: Vec<&str> = indeg.keys().map(|&i| pack[i].id.as_str()).collect();
        return Err(format!("dependencias en círculo entre: {}", ids.join(", ")));
    }
    // blocked: missing requirements, propagated to dependents (order = dependencies first)
    let mut plan = InstallPlan::default();
    let mut bad: BTreeSet<usize> = BTreeSet::new();
    for &i in &order {
        if let Some(m) = missing.get(&i) {
            bad.insert(i);
            plan.blocked.push((pack[i].id.clone(), format!("falta {}", m.join(", "))));
        } else if let Some(&d) = edges[&i].iter().find(|d| bad.contains(d)) {
            bad.insert(i);
            plan.blocked.push((pack[i].id.clone(), format!("necesita «{}», que no se puede instalar", pack[d].id)));
        } else {
            plan.order.push(pack[i].id.clone());
        }
    }
    Ok(plan)
}

/// An installed mod for the uninstall planning.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct InstalledMod {
    pub id: String,
    pub version: String,
    pub requires: Vec<String>,
    pub provides: Vec<String>,
}

impl InstalledMod {
    fn avail(&self) -> Avail {
        Avail { id: self.id.clone(), version: self.version.clone(), provides: self.provides.clone() }
    }
}

/// Mods to remove to uninstall `targets`, dependents first. A mod depends on a removed one when one of its
/// requirements is met by it and by no mod that stays. `cascade` = remove those dependents too; otherwise `Err`
/// names them.
pub fn uninstall_order(targets: &[String], installed: &[InstalledMod], cascade: bool) -> Result<Vec<String>, String> {
    for t in targets {
        if !installed.iter().any(|m| &m.id == t) {
            return Err(format!("el mod «{t}» no lo ha instalado este instalador"));
        }
    }
    let mut remove: BTreeSet<String> = targets.iter().cloned().collect();
    // who needs whom (among installed), for the cascade and the order
    let needs = |m: &InstalledMod, removing: &BTreeSet<String>| -> Vec<String> {
        let mut out = Vec::new();
        for r in &m.requires {
            let Ok(req) = evt_modfmt::parse_requirement(r) else { continue };
            let providers: Vec<&InstalledMod> = installed.iter().filter(|o| o.id != m.id && satisfies(&o.avail(), &req)).collect();
            if !providers.is_empty() && providers.iter().all(|o| removing.contains(&o.id)) {
                out.extend(providers.iter().map(|o| o.id.clone()));
            }
        }
        out
    };
    loop {
        let dependents: Vec<(String, Vec<String>)> = installed
            .iter()
            .filter(|m| !remove.contains(&m.id))
            .filter_map(|m| {
                let n = needs(m, &remove);
                (!n.is_empty()).then(|| (m.id.clone(), n))
            })
            .collect();
        if dependents.is_empty() {
            break;
        }
        if !cascade {
            let list: Vec<String> = dependents.iter().map(|(d, n)| format!("«{d}» necesita {}", n.join(", "))).collect();
            return Err(format!("no se puede quitar sin quitar también los mods que lo necesitan: {}", list.join("; ")));
        }
        remove.extend(dependents.into_iter().map(|(d, _)| d));
    }
    // order: a mod goes before every mod it needs (dependents first)
    let all_needs: BTreeMap<String, Vec<String>> = installed
        .iter()
        .filter(|m| remove.contains(&m.id))
        .map(|m| {
            let mut n = Vec::new();
            for r in &m.requires {
                if let Ok(req) = evt_modfmt::parse_requirement(r) {
                    n.extend(installed.iter().filter(|o| o.id != m.id && remove.contains(&o.id) && satisfies(&o.avail(), &req)).map(|o| o.id.clone()));
                }
            }
            (m.id.clone(), n)
        })
        .collect();
    let mut out: Vec<String> = Vec::new();
    let mut left: Vec<String> = installed.iter().filter(|m| remove.contains(&m.id)).map(|m| m.id.clone()).collect();
    while !left.is_empty() {
        // next: a mod no remaining mod needs
        let pick = left.iter().position(|c| !left.iter().any(|o| o != c && all_needs[o].contains(c))).unwrap_or(0);
        out.push(left.remove(pick));
    }
    Ok(out)
}

// ---------------------------------------------------------------- the ModLoader

/// The ModLoader in the game folder.
#[derive(Debug, Clone, PartialEq)]
pub enum LoaderState {
    /// No `winmm.dll`.
    Missing,
    /// A `winmm.dll` whose version is known (`ours` = installed by this installer and unchanged).
    Known { version: String, ours: bool },
    /// A `winmm.dll` of unknown version (someone else's proxy, or a ModLoader without the version marker).
    Unknown { ours: bool },
}

#[derive(Debug, Clone, PartialEq)]
pub enum LoaderAction {
    Keep(String),
    Install(String),
}

/// What to do with the ModLoader. `min` = the version the pack needs, `shipped` = the one it carries.
/// `keep` = never touch it (`--conservar-cargador`), `update` = also replace an older one that still meets `min`.
pub fn loader_action(state: &LoaderState, min: &str, shipped: &str, keep: bool, update: bool) -> Result<LoaderAction, String> {
    let lt = |a: &str, b: &str| !b.is_empty() && evt_modfmt::compare_versions(a, b).is_lt();
    let install = |why: String| -> Result<LoaderAction, String> {
        if shipped.is_empty() {
            return Err(format!("{why}, y este paquete no trae el ModLoader: instálalo antes"));
        }
        if lt(shipped, min) {
            return Err(format!("{why}, y el ModLoader del paquete ({shipped}) es más viejo que el necesario ({min})"));
        }
        Ok(LoaderAction::Install(why))
    };
    match state {
        LoaderState::Missing if keep => Err("no hay ModLoader (winmm.dll) y se ha pedido no instalarlo".into()),
        LoaderState::Missing => install("no hay ModLoader".into()),
        LoaderState::Known { version, .. } if keep => {
            Ok(LoaderAction::Keep(if lt(version, min) { format!("se conserva el {version} (AVISO: los mods piden {min})") } else { format!("se conserva el {version}") }))
        }
        LoaderState::Known { version, .. } if lt(version, min) => install(format!("el ModLoader instalado ({version}) es más viejo que el necesario ({min})")),
        LoaderState::Known { version, .. } if update && lt(version, shipped) => install(format!("se actualiza el {version} al {shipped}")),
        LoaderState::Known { version, .. } => Ok(LoaderAction::Keep(format!("el ModLoader {version} ya vale"))),
        LoaderState::Unknown { .. } if keep => Ok(LoaderAction::Keep("se conserva el winmm.dll actual (versión desconocida)".into())),
        LoaderState::Unknown { .. } if shipped.is_empty() => Ok(LoaderAction::Keep("winmm.dll de versión desconocida; el paquete no trae ModLoader".into())),
        LoaderState::Unknown { .. } => install("hay un winmm.dll de versión desconocida (se guarda una copia)".into()),
    }
}

/// `EVT_MODLOADER_VERSION=<version>` inside a winmm.dll (version chars: `0-9 A-Z a-z . - +`).
pub fn scan_version_marker(bytes: &[u8]) -> Option<String> {
    let at = bytes.windows(VERSION_MARKER.len()).position(|w| w == VERSION_MARKER)? + VERSION_MARKER.len();
    let v: String = bytes[at..].iter().take(32).take_while(|b| b.is_ascii_alphanumeric() || b"._-+".contains(b)).map(|&b| b as char).collect();
    (!v.is_empty()).then_some(v)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn e(id: &str, v: &str, req: &[&str]) -> ModEntry {
        ModEntry { id: id.into(), version: v.into(), requires: req.iter().map(|s| s.to_string()).collect(), activar: true, ..Default::default() }
    }

    #[test]
    fn manifest_v2_parses() {
        let t = r#"
producto = "Example Pack"
version = "0.2.0"
formato = 2
loader_min = "1.0.0"

[juego]
buildid = "24370575"

[cargador]
version = "1.0.0"

[[mod]]
id = "match_engine"
version = "1.0.0"
plugin = "match_engine.dll"
provides = ["match_engine_api=1"]

[[mod]]
id = "story_plus"
version = "0.2.0"
requires = ["match_engine>=1.0", "clean_hud"]
loader_min = "1.1.0"

[[mod]]
id = "clean_hud"
version = "1.0.0"
activar = false
"#;
        let m = parse_meta(t).unwrap();
        assert_eq!(m.formato, 2);
        assert_eq!(m.cargador.version, "1.0.0");
        assert_eq!(m.mods.len(), 3);
        assert_eq!(m.mods[0].provides, vec!["match_engine_api=1"]);
        assert!(m.mods[0].activar && !m.mods[2].activar);
        assert_eq!(m.mods[1].requires, vec!["match_engine>=1.0", "clean_hud"]);
        assert_eq!(max_version([m.loader_min.as_str()].into_iter().chain(m.mods.iter().map(|x| x.loader_min.as_str()))), "1.1.0");
        // format 1 packs read as format 1
        assert_eq!(parse_meta("producto = \"x\"\nversion = \"1\"\nformato = 1\nmods = [\"a\"]\n").unwrap().formato, 1);
    }

    #[test]
    fn mod_toml_reader_is_tolerant() {
        let t = "id = \"quit_fix\"\nname = \"Cierre\"\nversion = \"1.0.0\"\npriority = 0\nloader_modules = [\"quit_fix\"]\nplugin = \"quit_fix.dll\"\nloader_min = \"1.0.0\"\n";
        let m = parse_mod_toml(t).unwrap();
        assert_eq!((m.id.as_str(), m.version.as_str(), m.plugin.as_str(), m.loader_min.as_str()), ("quit_fix", "1.0.0", "quit_fix.dll", "1.0.0"));
        assert!(parse_mod_toml("id = \"Bad Id\"\nversion = \"1\"\n").is_err());
        assert!(parse_mod_toml("id = \"a\"\nversion = \"1\"\nrequires = [\"b>>1\"]\n").is_err());
    }

    #[test]
    fn dependency_order_dependencies_first() {
        let pack = vec![e("story_plus", "0.2.0", &["match_engine>=1.0", "clean_hud"]), e("clean_hud", "1.0.0", &[]), e("match_engine", "1.0.0", &[]), e("quit_fix", "1.0.0", &[])];
        let p = plan_install(&pack, None, &[]).unwrap();
        assert!(p.blocked.is_empty());
        let at = |id: &str| p.order.iter().position(|x| x == id).unwrap();
        assert!(at("clean_hud") < at("story_plus") && at("match_engine") < at("story_plus"));
        assert_eq!(p.order.len(), 4);
        // ties keep pack order
        assert_eq!(p.order, vec!["clean_hud", "match_engine", "story_plus", "quit_fix"]);
        // selecting only IF pulls its dependencies in
        let p = plan_install(&pack, Some(&["story_plus".to_string()]), &[]).unwrap();
        assert_eq!(p.order, vec!["clean_hud", "match_engine", "story_plus"]);
        assert!(plan_install(&pack, Some(&["nope".to_string()]), &[]).is_err());
    }

    #[test]
    fn missing_dependency_blocks_mod_and_dependents() {
        let pack = vec![e("a", "1.0.0", &["voces_es>=2"]), e("b", "1.0.0", &["a"]), e("c", "1.0.0", &[])];
        let p = plan_install(&pack, None, &[]).unwrap();
        assert_eq!(p.order, vec!["c"]);
        assert_eq!(p.blocked.len(), 2);
        assert!(p.blocked[0].1.contains("voces_es >= 2"), "{:?}", p.blocked);
        assert!(p.blocked[1].1.contains("«a»"));
        // installed but too old: still blocked, and the message says what is there
        let old = [Avail { id: "voces_es".into(), version: "1.5".into(), provides: vec![] }];
        let p = plan_install(&pack, None, &old).unwrap();
        assert!(p.blocked[0].1.contains("hay la 1.5"), "{:?}", p.blocked);
        // installed and new enough: fine
        let ok = [Avail { id: "voces_es".into(), version: "2.1".into(), provides: vec![] }];
        assert_eq!(plan_install(&pack, None, &ok).unwrap().order, vec!["a", "b", "c"]);
    }

    #[test]
    fn provides_and_versions_and_cycles() {
        let mut me = e("match_engine", "1.3.0", &[]);
        me.provides = vec!["match_rules_api=2".into()];
        let pack = vec![e("x", "1", &["match_rules_api>=2"]), me];
        assert_eq!(plan_install(&pack, None, &[]).unwrap().order, vec!["match_engine", "x"]);
        let pack2 = vec![e("x", "1", &["match_rules_api>=3"]), pack[1].clone()];
        assert_eq!(plan_install(&pack2, None, &[]).unwrap().blocked.len(), 1);
        let cyc = vec![e("a", "1", &["b"]), e("b", "1", &["a"])];
        assert!(plan_install(&cyc, None, &[]).unwrap_err().contains("círculo"));
    }

    fn im(id: &str, req: &[&str]) -> InstalledMod {
        InstalledMod { id: id.into(), version: "1.0.0".into(), requires: req.iter().map(|s| s.to_string()).collect(), provides: vec![] }
    }

    #[test]
    fn uninstall_set_respects_dependents() {
        let inst = vec![im("match_engine", &[]), im("clean_hud", &[]), im("story_plus", &["match_engine>=1.0", "clean_hud"]), im("quit_fix", &[])];
        // a leaf goes alone
        assert_eq!(uninstall_order(&["quit_fix".into()], &inst, false).unwrap(), vec!["quit_fix"]);
        assert_eq!(uninstall_order(&["story_plus".into()], &inst, false).unwrap(), vec!["story_plus"]);
        // a dependency: refused, or with cascade its dependents go first
        let err = uninstall_order(&["match_engine".into()], &inst, false).unwrap_err();
        assert!(err.contains("story_plus"), "{err}");
        assert_eq!(uninstall_order(&["match_engine".into()], &inst, true).unwrap(), vec!["story_plus", "match_engine"]);
        // everything: dependents before their dependencies
        let all: Vec<String> = inst.iter().map(|m| m.id.clone()).collect();
        let o = uninstall_order(&all, &inst, false).unwrap();
        let at = |id: &str| o.iter().position(|x| x == id).unwrap();
        assert!(at("story_plus") < at("match_engine") && at("story_plus") < at("clean_hud"));
        assert!(uninstall_order(&["nope".into()], &inst, false).is_err());
    }

    #[test]
    fn uninstall_keeps_dependent_when_another_provider_stays() {
        let mut alt = im("alt_engine", &[]);
        alt.provides = vec!["match_engine=1.5".into()];
        let inst = vec![im("match_engine", &[]), alt, im("x", &["match_engine>=1.0"])];
        assert_eq!(uninstall_order(&["match_engine".into()], &inst, false).unwrap(), vec!["match_engine"]);
    }

    #[test]
    fn loader_decision() {
        use LoaderAction::*;
        let known = |v: &str| LoaderState::Known { version: v.into(), ours: false };
        assert!(matches!(loader_action(&LoaderState::Missing, "1.0.0", "1.0.0", false, false), Ok(Install(_))));
        assert!(loader_action(&LoaderState::Missing, "1.0.0", "", false, false).is_err());
        assert!(matches!(loader_action(&known("0.9"), "1.0.0", "1.2.0", false, false), Ok(Install(_))));
        assert!(matches!(loader_action(&known("1.1.0"), "1.0.0", "1.2.0", false, false), Ok(Keep(_))));
        assert!(matches!(loader_action(&known("1.1.0"), "1.0.0", "1.2.0", false, true), Ok(Install(_))));
        assert!(matches!(loader_action(&known("1.2.0"), "1.0.0", "1.2.0", false, true), Ok(Keep(_))));
        assert!(matches!(loader_action(&known("0.9"), "1.0.0", "1.2.0", true, false), Ok(Keep(w)) if w.contains("AVISO")));
        assert!(loader_action(&known("0.9"), "1.3.0", "1.2.0", false, false).is_err(), "shipped older than needed");
        assert!(matches!(loader_action(&LoaderState::Unknown { ours: false }, "1.0.0", "1.0.0", false, false), Ok(Install(_))));
        assert!(matches!(loader_action(&LoaderState::Unknown { ours: false }, "", "", false, false), Ok(Keep(_))));
        assert!(matches!(loader_action(&known("1.0.0"), "", "", false, false), Ok(Keep(_))));
    }

    #[test]
    fn version_marker_scan() {
        let mut dll = vec![0u8; 100];
        dll.extend_from_slice(b"EVT_MODLOADER_VERSION=1.2.3-beta\0rest");
        assert_eq!(scan_version_marker(&dll).as_deref(), Some("1.2.3-beta"));
        assert_eq!(scan_version_marker(b"no marker here"), None);
        assert_eq!(scan_version_marker(b"EVT_MODLOADER_VERSION=\0"), None);
    }
}
