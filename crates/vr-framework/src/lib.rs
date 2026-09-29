//! **VR-Framework**: the shared base of the VR-ModLoader engine plugins (guide:
//! `crates/vr-framework/README.md`).
//!
//! An *engine* is a native plugin (`evt-plugin-sdk`) that takes over one system of the game: mods ship small, readable
//! data files and the engine turns them into what the game reads, merged across every active mod. Every engine needs
//! the same plumbing, and this crate is where it lives:
//!
//! | Module | What it gives |
//! |---|---|
//! | [`discover`] | per-mod data files (`<mod>\<name>.toml`, `<mod>\<dir>\*.toml`), installed mods, mods that use an engine |
//! | [`host`] | the ModLoader side: active / inactive mods in load order, the cache folder, the early-phase serving mode, `file_serve` of a set of files, logging of [`Notes`] |
//! | [`diag`] | [`Notes`] (error / warn / info / debug lines an engine returns and the plugin logs) and [`diag::Diagnostic`] (file, line, key) |
//! | [`data`] | strict TOML data files: a `schema = "<kind>/<version>"` line in every file, unknown keys are errors with file, line and key |
//! | [`ids`] | mod ids, namespaced own ids (`<mod_id>.<name>`), game ids (crc32 of the name), stable id allocation persisted between starts |
//! | [`layer`] | cross-mod merge rules: keyed claims with precedence tiers, "the mod that loads later wins" with conflict notes |
//! | [`cache`] | build cache: input hash, dependency stats, JSON manifest; per-output stamps |
//! | [`game`] | where base files come from: another mod's whole file, else the game (loose file / CPK, or `game_file_path`) |
//! | [`slots`] | serving generated files on a ModLoader without `file_serve` (fixed-size files rewritten in place) |
//! | [`lua`] | `CMND_EVT_*` registration, Lua values ↔ JSON, one-time warnings, C strings of DLL exports |
//! | [`options`] | a mod's settings: `options.toml` (toggle / list / number) and their current values (tab «Opciones de mods») |
//! | [`state`] | reading the loader's numbered event rings through `game_state` |
//! | [`fsx`] | atomic file writes, paths of game keys (`data/...`) under a folder |
//!
//! Format rules every engine follows: references to the game use its **internal ids**
//! (`c01020010`, `whs01980`); new entries get **own ids namespaced by the mod** (`<mod_id>.<name>`) that are stable;
//! **keys and values are English**; every data file carries a **`schema`** version; validation is **strict** (an
//! unknown key is an error naming the file, the line and the key); **every setting a mod adds lives in the Opciones
//! tab «Opciones de mods»**, declared in the mod's `options.toml` ([`options`]).

pub mod cache;
pub mod data;
pub mod diag;
pub mod discover;
pub mod fsx;
pub mod game;
pub mod host;
pub mod ids;
pub mod layer;
pub mod lua;
pub mod options;
pub mod slots;
pub mod state;

pub use diag::{Lvl, Notes};

/// One mod as the engines see it (from the ModLoader's mod list, an `evt_modfmt` load plan, or an installed folder).
#[derive(Debug, Clone, PartialEq)]
pub struct ModDir {
    pub id: String,
    pub dir: std::path::PathBuf,
    /// Position in the load order (later = wins conflicts).
    pub load_index: u32,
}

impl ModDir {
    pub fn new(id: impl Into<String>, dir: impl Into<std::path::PathBuf>, load_index: u32) -> ModDir {
        ModDir { id: id.into(), dir: dir.into(), load_index }
    }
}

/// `mods` sorted by load order (stable: equal indices keep their order).
pub fn in_load_order(mods: &[ModDir]) -> Vec<&ModDir> {
    let mut v: Vec<&ModDir> = mods.iter().collect();
    v.sort_by_key(|m| m.load_index);
    v
}
