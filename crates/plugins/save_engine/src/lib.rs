//! Plugin `save_engine` (mod `mods\save_engine\`, `save_engine.dll`; docs/game/engine/save-engine.md): **per-mod
//! persistent data kept apart from the retail save**, tied to the player's save slot.
//!
//! * Storage: `<game>\evt_loader\saves\slot<N>\<mod id>.json` (slot scope) and `...\saves\global\<mod id>.json`
//!   (global scope), written atomically (tmp + flush + rename) — [`store`].
//! * Slot data follows the retail save: writes stay in memory until **the game saves** that slot (then they are
//!   committed); a slot switch without a save drops them, as the game drops its own unsaved progress. Options: a mod
//!   in `immediate_mods` (or a `SET` with `now = true`, or `CMND_EVT_SAVE_COMMIT`) is written at once. Global data is
//!   written at once (by the worker thread).
//! * Slot events (save, switch, copy, delete, new game) come from a loader build with a save-slots module through
//!   `game_state("save.*")` ([`codec`]); without them (the public VR-ModLoader does not publish them yet) the plugin
//!   watches the retail save file on disk ([`watch`]).
//! * Lua: `CMND_EVT_SAVE_GET/SET/DEL(mod, key, ...)`, `CMND_EVT_SAVE_GLOBAL_GET/SET/DEL`, `CMND_EVT_SAVE_COMMIT`,
//!   `CMND_EVT_SAVE_SLOT` (a Lua command cannot know which mod's script called it, so the mod id is the first argument;
//!   the mod's `lua\_all` helper `EvtSave.open()` fills it from `EVT_PATCH`).
//! * [`slots`]: pure port of the loader's slot naming / state, generalised to a configurable number of slots (design
//!   of the save_slots takeover; not active at run time yet).

use serde::{Deserialize, Serialize};

pub mod generic;
pub mod slots;
pub mod store;
pub mod watch;

pub use generic::{num_value, str_value, valid_key, valid_mod_id, MAX_KEY, MAX_KEYS, MAX_STR};

#[cfg(all(windows, target_arch = "x86_64"))]
mod rt;

/// Configuration (`mods\save_engine\config.toml`, overridden by `[mods.save_engine]` of `evt_loader\config.toml`).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct SaveEngineCfg {
    /// `"on_save"` (default): slot data is committed when the game saves the slot. `"immediate"`: every write is
    /// committed at once (by the worker thread, within [`SaveEngineCfg::flush_ms`]).
    pub commit: String,
    /// Mods whose slot data is always committed at once (the others follow `commit`).
    pub immediate_mods: Vec<String>,
    /// Worker period in ms (event polling, global / immediate writes), 50..=2000.
    pub flush_ms: u64,
    /// Trash folders kept in `saves\trash` (data of deleted / replaced slots), 1..=200.
    pub trash_keep: usize,
    /// Where the data lives ([`StorageCfg`]).
    pub storage: StorageCfg,
    /// Extra save slots (design, docs/game/engine/save-engine.md §6; not active: the loader's save_slots module
    /// still owns the slots).
    pub slots: slots::SlotsCfg,
}

impl Default for SaveEngineCfg {
    fn default() -> Self {
        SaveEngineCfg {
            commit: "on_save".into(),
            immediate_mods: Vec::new(),
            flush_ms: 250,
            trash_keep: 30,
            storage: StorageCfg::default(),
            slots: slots::SlotsCfg::default(),
        }
    }
}

/// `[storage]`: `location = "profile"` (default): next to the game's own user data in the Windows
/// profile, `%USERPROFILE%\AppData\LocalLow\LEVEL5 Inc_\INAZUMA ELEVEN Victory Road\modloader_saves\` (survives a
/// reinstall of the game). `"game"`: `<game>\evt_loader\saves\`. `path` (absolute) overrides both.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct StorageCfg {
    pub location: String,
    pub path: String,
}

impl Default for StorageCfg {
    fn default() -> Self {
        StorageCfg { location: "profile".into(), path: String::new() }
    }
}

/// The game's folder under LocalLow (where it keeps `users\<id>\save\...-DEVICELOCAL`; docs/formats/save.md §1).
pub const PROFILE_GAME_DIR: &str = r"LEVEL5 Inc_\INAZUMA ELEVEN Victory Road";
/// Our folder inside it.
pub const PROFILE_SAVES_DIR: &str = "modloader_saves";

/// Root of the store. `local_low` = `FOLDERID_LocalAppDataLow` (None = unknown: falls back to the game folder).
/// Returns the root and a note for the log when the configured location could not be used.
pub fn storage_root(c: &StorageCfg, game_dir: &std::path::Path, local_low: Option<&std::path::Path>) -> (std::path::PathBuf, Option<String>) {
    let game = game_dir.join("evt_loader").join("saves");
    if !c.path.trim().is_empty() {
        let p = std::path::PathBuf::from(c.path.trim());
        if p.is_absolute() {
            return (p, None);
        }
        return (game, Some(format!("storage.path {:?} is not absolute: using {}", c.path, "the game folder")));
    }
    match c.location.to_ascii_lowercase().as_str() {
        "game" => (game, None),
        "profile" | "" => match local_low {
            Some(ll) => (ll.join(PROFILE_GAME_DIR).join(PROFILE_SAVES_DIR), None),
            None => (game, Some("LocalLow folder unknown: storage falls back to the game folder".into())),
        },
        other => (game, Some(format!("storage.location {other:?} unknown (profile / game): using the game folder"))),
    }
}

impl SaveEngineCfg {
    pub fn parse(text: &str) -> Result<SaveEngineCfg, String> {
        if text.trim().is_empty() {
            return Ok(SaveEngineCfg::default());
        }
        toml::from_str(text).map_err(|e| e.to_string())
    }
    pub fn all_immediate(&self) -> bool {
        self.commit.eq_ignore_ascii_case("immediate")
    }
    pub fn is_immediate(&self, mod_id: &str) -> bool {
        self.all_immediate() || self.immediate_mods.iter().any(|m| m == mod_id)
    }
    pub fn flush_period_ms(&self) -> u64 {
        self.flush_ms.clamp(50, 2000)
    }
    pub fn trash_limit(&self) -> usize {
        self.trash_keep.clamp(1, 200)
    }
}

/// Save-slot events of the ModLoader (`game_state("save.event.<n>")`): same codec as
/// `crates/vr-loader/src/saveslots/events.rs` (`kind << 16 | a << 8 | b`).
pub mod codec {
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum Event {
        /// The game wrote slot `n`'s save.
        Saved(u8),
        /// The active slot is now `to` (was `from`).
        Switched { to: u8, from: u8 },
        /// Slot `src` was duplicated into slot `dst`.
        Copied { src: u8, dst: u8 },
        /// Slot `n`'s save was removed.
        Deleted(u8),
        /// A new game starts in the empty slot `n`.
        NewGame(u8),
    }

    pub fn decode(v: i64) -> Option<Event> {
        if !(0..=0xFF_FFFF).contains(&v) {
            return None;
        }
        let (k, a, b) = ((v >> 16) as u8, (v >> 8) as u8, v as u8);
        Some(match k {
            1 => Event::Saved(a),
            2 => Event::Switched { to: a, from: b },
            3 => Event::Copied { src: a, dst: b },
            4 => Event::Deleted(a),
            5 => Event::NewGame(a),
            _ => return None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_defaults_and_overrides() {
        let c = SaveEngineCfg::parse("").unwrap();
        assert_eq!(c, SaveEngineCfg::default());
        assert!(!c.all_immediate() && !c.is_immediate("x"));
        let c = SaveEngineCfg::parse("immediate_mods = [\"a\"]\nflush_ms = 5\n[slots]\ncount = 6\n").unwrap();
        assert!(c.is_immediate("a") && !c.is_immediate("b"));
        assert_eq!(c.flush_period_ms(), 50);
        assert_eq!(c.slots.count, 6);
        let c = SaveEngineCfg::parse("commit = \"Immediate\"").unwrap();
        assert!(c.is_immediate("anything"));
        assert!(SaveEngineCfg::parse("commit = 3").is_err());
    }

    #[test]
    fn storage_location() {
        use std::path::Path;
        let g = Path::new(r"D:\Games\VR");
        let ll = Path::new(r"C:\Users\n\AppData\LocalLow");
        let c = StorageCfg::default();
        assert_eq!(storage_root(&c, g, Some(ll)).0, ll.join(r"LEVEL5 Inc_\INAZUMA ELEVEN Victory Road\modloader_saves"));
        let (p, note) = storage_root(&c, g, None);
        assert_eq!(p, g.join(r"evt_loader\saves"));
        assert!(note.is_some());
        let c = StorageCfg { location: "game".into(), ..Default::default() };
        assert_eq!(storage_root(&c, g, Some(ll)), (g.join(r"evt_loader\saves"), None));
        let c = StorageCfg { path: r"E:\mine".into(), ..Default::default() };
        assert_eq!(storage_root(&c, g, Some(ll)).0, Path::new(r"E:\mine"));
        let c = StorageCfg { path: "rel".into(), ..Default::default() };
        assert!(storage_root(&c, g, Some(ll)).1.is_some());
        let c = StorageCfg { location: "cloud".into(), ..Default::default() };
        assert!(storage_root(&c, g, Some(ll)).1.is_some());
        let t: SaveEngineCfg = SaveEngineCfg::parse("[storage]\nlocation = \"game\"\n").unwrap();
        assert_eq!(t.storage.location, "game");
    }

    #[test]
    fn event_codec_matches_the_loader() {
        use codec::*;
        assert_eq!(decode(0x01_02_00), Some(Event::Saved(2)));
        assert_eq!(decode(0x02_03_02), Some(Event::Switched { to: 3, from: 2 }));
        assert_eq!(decode(0x03_01_02), Some(Event::Copied { src: 1, dst: 2 }));
        assert_eq!(decode(0x04_04_00), Some(Event::Deleted(4)));
        assert_eq!(decode(0x05_03_00), Some(Event::NewGame(3)));
        assert_eq!(decode(0), None);
        assert_eq!(decode(-5), None);
    }
}
