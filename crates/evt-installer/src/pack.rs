//! The release pack (`paquete`) of a single-pack install (built by a pack tool that is not part of this repository).
//!
//! ```text
//! pack.toml                 product, version, commit, the v7.1.2 build it was made on, mods to enable
//! indice.json               every shipped file (data/**, delta/**, juego/**) with size + CRC-32
//! parches.json              mod files that replace a retail file and ship as a delta: ruta, delta, tam
//! evt_loader_modules.toml   loader modules (the pack's list), applied to evt_loader\config.toml
//! data/**                   mod files shipped whole (new files, and replaced files whose delta is not worth it)
//! delta/<ruta>.evtd         deltas against the player's retail file ([`crate::delta`])
//! juego/**                  files copied into the game root: winmm.dll, evt_loader/**, mods/<id>/**, ...
//! ```
//! Everything is installed; there are no optional parts.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

pub const PACK_DIR: &str = "paquete";
pub const META_FILE: &str = "pack.toml";
pub const INDEX_FILE: &str = "indice.json";
pub const PATCHES_FILE: &str = "parches.json";
pub const MODULES_FILE: &str = "evt_loader_modules.toml";
pub const DELTA_DIR: &str = "delta";
pub const DELTA_EXT: &str = "evtd";
/// Game-root files of the pack live under this folder of the pack.
pub const GAME_FILES: &str = "juego";
/// Copied from the pack, then given the module switches.
pub const LOADER_CONFIG: &str = "evt_loader/config.toml";

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PackMeta {
    pub producto: String,
    pub version: String,
    #[serde(default)]
    pub formato: u32,
    #[serde(default)]
    pub compilado: String,
    #[serde(default)]
    pub commit: String,
    /// Mods of `juego/mods/` to switch on in `mods\enabled.toml`.
    #[serde(default)]
    pub mods: Vec<String>,
    #[serde(default)]
    pub juego: GameBuild,
}

/// The Steam build of v7.1.2 the pack was made on (read from the builder's appmanifest).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct GameBuild {
    #[serde(default)]
    pub nie_sha1: String,
    #[serde(default)]
    pub buildid: String,
    #[serde(default)]
    pub depot: String,
    #[serde(default)]
    pub manifest: String,
    /// Bytes of the depot (appmanifest `InstalledDepots/<depot>/size`).
    #[serde(default)]
    pub depot_bytes: u64,
}

/// One shipped file: path relative to the pack root, size, CRC-32.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PackFile {
    pub ruta: String,
    pub tam: u64,
    pub crc: u32,
}

/// A mod file shipped as a delta against the retail file of the same path.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Patch {
    /// Game path (`data/...`).
    pub ruta: String,
    /// Pack path of the delta (`delta/data/....evtd`).
    pub delta: String,
    /// Size of the result.
    pub tam: u64,
    /// The installed file is XOR-encrypted with this key (`crc32(basename)`, loose CRI files: acb / awb / usm);
    /// the delta is made on the plaintext, which is what the CPK gives.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub xor: Option<u32>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct IndexFile {
    pub archivos: Vec<PackFile>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct PatchesFile {
    pub parches: Vec<Patch>,
}

pub struct Pack {
    pub root: PathBuf,
    pub meta: PackMeta,
    pub files: Vec<PackFile>,
    pub patches: Vec<Patch>,
}

fn read(p: &Path) -> Result<String, String> {
    std::fs::read_to_string(p).map_err(|e| format!("{}: {e}", p.display()))
}

impl Pack {
    pub fn open(root: &Path) -> Result<Pack, String> {
        let meta: PackMeta = toml::from_str(&read(&root.join(META_FILE))?).map_err(|e| format!("{META_FILE}: {e}"))?;
        let index: IndexFile = serde_json::from_str(&read(&root.join(INDEX_FILE))?).map_err(|e| format!("{INDEX_FILE}: {e}"))?;
        let patches: PatchesFile = match root.join(PATCHES_FILE).is_file() {
            true => serde_json::from_str(&read(&root.join(PATCHES_FILE))?).map_err(|e| format!("{PATCHES_FILE}: {e}"))?,
            false => PatchesFile::default(),
        };
        Ok(Pack { root: root.to_path_buf(), meta, files: index.archivos, patches: patches.parches })
    }

    /// Pack errors (empty = well formed).
    pub fn validate(&self) -> Vec<String> {
        let mut errs = Vec::new();
        let has = |p: &str| self.files.iter().any(|f| f.ruta.eq_ignore_ascii_case(p));
        if !has(&format!("{GAME_FILES}/{LOADER_CONFIG}")) {
            errs.push(format!("falta {GAME_FILES}/{LOADER_CONFIG}"));
        }
        if !has(&format!("{GAME_FILES}/winmm.dll")) {
            errs.push(format!("falta {GAME_FILES}/winmm.dll"));
        }
        for p in &self.patches {
            if !has(&p.delta) {
                errs.push(format!("{}: falta su delta {}", p.ruta, p.delta));
            }
            if has(&p.ruta) {
                errs.push(format!("{}: está entero y como delta a la vez", p.ruta));
            }
            if !p.ruta.to_lowercase().starts_with("data/") {
                errs.push(format!("{}: un delta solo puede ser de data/", p.ruta));
            }
        }
        for m in &self.meta.mods {
            if !has(&format!("{GAME_FILES}/mods/{m}/{}", evt_modfmt::MANIFEST)) {
                errs.push(format!("mod «{m}»: falta {GAME_FILES}/mods/{m}/mod.toml"));
            }
        }
        errs
    }

    /// Files missing from the pack folder or with another size (a partial extraction of the zip).
    pub fn check_files(&self) -> Vec<String> {
        check_index(&self.root, &self.files)
    }

    /// Everything the install does.
    pub fn plan(&self) -> Plan {
        let mut plan = Plan::default();
        for f in &self.files {
            let lower = f.ruta.to_lowercase();
            if lower.starts_with("data/") {
                plan.data.push(f.ruta.clone());
                plan.data_bytes += f.tam;
            } else if let Some(rel) = lower.strip_prefix(&format!("{GAME_FILES}/")) {
                if rel != LOADER_CONFIG {
                    plan.game.push(f.ruta[GAME_FILES.len() + 1..].to_string());
                    plan.game_bytes += f.tam;
                }
            }
        }
        plan.data_bytes += self.patches.iter().map(|p| p.tam).sum::<u64>();
        if let Ok(t) = std::fs::read_to_string(self.root.join(MODULES_FILE)) {
            plan.modules = crate::loadercfg::parse_module_list(&t);
        }
        plan.mods = self.meta.mods.clone();
        plan
    }
}

/// Index entries missing from `root` or with another size.
pub fn check_index(root: &Path, files: &[PackFile]) -> Vec<String> {
    let mut bad = Vec::new();
    for f in files {
        let p = root.join(&f.ruta);
        match std::fs::metadata(&p) {
            Ok(m) if m.len() == f.tam => {}
            Ok(m) => bad.push(format!("{}: tamaño {} (esperado {})", f.ruta, m.len(), f.tam)),
            Err(_) => bad.push(format!("{}: falta", f.ruta)),
        }
    }
    bad
}

#[derive(Debug, Default)]
pub struct Plan {
    /// Pack paths (`data/...`) of the whole files.
    pub data: Vec<String>,
    /// Bytes of data installed (whole files + patch results).
    pub data_bytes: u64,
    /// Game-relative paths (without `juego/`) copied into the game root; the loader config is apart.
    pub game: Vec<String>,
    pub game_bytes: u64,
    /// `[modules]` switches for `evt_loader\config.toml`, in order.
    pub modules: Vec<(String, bool)>,
    /// Mod ids to enable in `mods\enabled.toml`.
    pub mods: Vec<String>,
}
