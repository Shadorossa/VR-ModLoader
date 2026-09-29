//! The ModLoader side of an engine (plugin API v1, `evt-plugin-sdk`): mods, folders, the early phase and serving.
//!
//! **Serving generated files.** An engine builds at the *early phase* (exe entry point, before any game code) and
//! hands each output to the loader with `file_serve(game path, file)`: the game reads it at its real size from the
//! first start. [`EarlyMode::of`] tells which case applies; [`serve_files`] serves a set of files:
//!
//! | [`EarlyMode`] | Loader | What the engine does |
//! |---|---|---|
//! | `Serve` | `phase()` = early, `file_serve` present | build into [`cache_dir`], serve with `file_serve` |
//! | `Late` | early phase run after the game started | build into the cache, serve nothing (shown from the next start) |
//! | `Legacy` | no `phase()` / `file_serve` (older API) | fallback: fixed-size slots in the engine's own `files\` ([`crate::slots`]) |
//!
//! `file_serve` only accepts files below the game folder: [`cache_dir`] (`evt_loader\cache\<engine>\`) is.

use crate::diag::{Lvl, Notes};
use crate::ModDir;
use evt_plugin_sdk::{Host, Level, EVT_PHASE_EARLY, EVT_PHASE_NONE};
use std::path::{Path, PathBuf};

/// Active mods in load order, as [`ModDir`]s.
pub fn active_mods(h: &Host) -> Vec<ModDir> {
    h.mods().into_iter().map(|m| ModDir { id: m.id, dir: m.dir, load_index: m.load_index }).collect()
}

/// Installed mods (folders of `mods_dir` with a `mod.toml`) that are not in `active` (load index 0).
pub fn inactive_mods(h: &Host, active: &[ModDir]) -> Vec<ModDir> {
    let Some(md) = h.path("mods_dir") else { return Vec::new() };
    crate::discover::installed(&md)
        .into_iter()
        .filter(|(id, _)| !active.iter().any(|a| a.id == *id))
        .map(|(id, dir)| ModDir { id, dir, load_index: 0 })
        .collect()
}

/// The game folder and `evt_loader` (`<game>\evt_loader` when the loader does not say).
pub fn game_and_loader_dirs(h: &Host) -> (PathBuf, PathBuf) {
    let game = h.path("game_dir").unwrap_or_default();
    let loader = h.path("loader_dir").unwrap_or_else(|| game.join("evt_loader"));
    (game, loader)
}

/// This engine's cache folder: `path_get("cache_dir")\<mod id>` (`evt_loader\cache\<mod id>`), with `loader_dir\cache`
/// on a loader without the key.
pub fn cache_dir(h: &Host) -> PathBuf {
    h.path("cache_dir").or_else(|| h.path("loader_dir").map(|d| d.join("cache"))).unwrap_or_default().join(&h.mod_id)
}

/// A build note's level as a loader log level.
pub fn level(l: Lvl) -> Level {
    match l {
        Lvl::Error => Level::Error,
        Lvl::Warn => Level::Warn,
        Lvl::Info => Level::Info,
        Lvl::Debug => Level::Debug,
    }
}

/// Log every note (the loader prefixes each line with `<mod id>: `).
pub fn log_notes(h: &Host, n: &Notes) {
    for (l, s) in &n.0 {
        h.log(level(*l), s);
    }
}

/// How the early phase can serve generated files (see the module doc).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EarlyMode {
    /// At the exe entry point with `file_serve`: shown at this start.
    Serve,
    /// The early phase ran after the game started: nothing can be served now (shown from the next start).
    Late,
    /// A loader without `phase` / `file_serve` (older API): only the slots fallback.
    Legacy,
}

impl EarlyMode {
    /// From `Host::phase()` (call it in the early callback).
    pub fn of(h: &Host) -> EarlyMode {
        EarlyMode::from_phase(h.phase())
    }

    pub fn from_phase(phase: u32) -> EarlyMode {
        match phase {
            EVT_PHASE_EARLY => EarlyMode::Serve,
            EVT_PHASE_NONE => EarlyMode::Legacy,
            _ => EarlyMode::Late,
        }
    }
}

/// `file_serve` every `(game path, file)`: the keys served, and `(key, loader code)` of the ones refused.
pub fn serve_files<'a>(h: &Host, files: impl IntoIterator<Item = (&'a str, &'a Path)>) -> (Vec<String>, Vec<(String, i32)>) {
    let (mut ok, mut bad) = (Vec::new(), Vec::new());
    for (key, path) in files {
        match h.file_serve(key, path) {
            Ok(()) => ok.push(key.to_string()),
            Err(c) => bad.push((key.to_string(), c)),
        }
    }
    (ok, bad)
}

/// The game's own files through the ModLoader (`game_file_path`: the loose file when the install has one, else
/// extracted from its CPK into the loader's cache; never the mods overlay). None on a loader without the function.
pub struct HostGame(pub &'static Host);

impl crate::game::GameFiles for HostGame {
    fn path(&self, key: &str) -> Option<PathBuf> {
        self.0.game_file_path(key)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn early_modes() {
        assert_eq!(EarlyMode::from_phase(EVT_PHASE_EARLY), EarlyMode::Serve);
        assert_eq!(EarlyMode::from_phase(EVT_PHASE_NONE), EarlyMode::Legacy);
        assert_eq!(EarlyMode::from_phase(evt_plugin_sdk::EVT_PHASE_EARLY_LATE), EarlyMode::Late);
        assert_eq!(level(Lvl::Warn), Level::Warn);
    }
}
