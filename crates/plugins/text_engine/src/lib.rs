//! Plugin `text_engine` (mod `mods\text_engine\`, `text_engine.dll`): the **text engine** of the ModLoader, the first
//! of the engines (docs/app/modloader-roadmap.md «Engines»; design: docs/game/engine/text-engine.md; modders:
//! mods/text_engine/README.md).
//!
//! Mods write texts by readable keys in 9 languages (`text.toml` / `text\<lang>.toml`): replace a game text
//! (`chara.c01000010.name`, `menu_text:sysmes_foo`, `system_text:1389146809`) or add a new one (`[new] greeting =
//! "Hello"` → key `<mod id>.greeting`, stable id). At the early phase (exe entry point, before any game code) the
//! plugin merges every active mod over the tables the game would read (load order, conflicts → WARN, later wins;
//! language fallback: the language → `all` → the mod's default language → the game's text), writes the merged tables
//! to `evt_loader\cache\text_engine\` and serves them with the ModLoader's `file_serve` (shown at the first start;
//! a ModLoader without it: the [`fw::slots`] fallback); the result is cached ([`fw::cache`]). Lua: `CMND_EVT_TEXT_ID(key)`
//! → id, `CMND_EVT_TEXT_GET(key | id [, lang])` → string. Other plugins: the DLL exports `evt_text_id` /
//! `evt_text_get` (see `rt`).
//!
//! Layout: [`fw`] is the generic part, from the shared **VR-Framework** crate (`crates/vr-framework`: discovery of
//! per-mod declaration files, layered cross-mod merge, base files, cache, slots, ids, host helpers); [`lang`],
//! [`table`], [`decl`], [`keys`], [`build`], [`index`] are the text-specific parts; [`boot`] wires them (its flow is
//! generic, its types are text ones).

pub mod boot;
pub mod build;
pub mod decl;
pub mod fw;
pub mod index;
pub mod keys;
pub mod lang;
pub mod table;

use serde::{Deserialize, Serialize};

/// Configuration: `mods\text_engine\config.toml`, overridden by `[mods.text_engine]` of `evt_loader\config.toml`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Cfg {
    /// Merge and serve the mods' texts (false = nothing served, the game's texts; Lua commands still answer the new
    /// keys from the last build).
    pub enabled: bool,
    /// Language of `CMND_EVT_TEXT_GET` when the call gives none (a folder name: en, es, ja, zh_hans…).
    pub lang: String,
    /// Slots fallback only (a ModLoader without `file_serve`): minimum free room of a new slot, KiB (texts added
    /// later must fit without a restart).
    pub headroom_kib: u32,
    /// Slots fallback only: prepare slots for installed mods that are not active, so enabling one needs one restart
    /// only.
    pub prepare_inactive: bool,
}

impl Default for Cfg {
    fn default() -> Self {
        Cfg { enabled: true, lang: "en".into(), headroom_kib: 64, prepare_inactive: true }
    }
}

impl Cfg {
    /// Parse the merged configuration text (defaults on error, with the message).
    pub fn from_text(t: &str) -> (Cfg, Option<String>) {
        match toml::from_str::<Cfg>(t) {
            Ok(c) => (c, None),
            Err(e) => (Cfg::default(), Some(e.message().to_string())),
        }
    }

    pub fn policy(&self, live: bool) -> fw::slots::SlotPolicy {
        fw::slots::SlotPolicy { min_headroom: (self.headroom_kib.clamp(4, 64 * 1024) as usize) * 1024, align: 4096, live }
    }

    /// The default language folder (`en` when the setting is not a language).
    pub fn default_lang(&self) -> &'static str {
        lang::norm(&self.lang).unwrap_or("en")
    }
}

#[cfg(all(windows, target_arch = "x86_64"))]
mod rt;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_defaults() {
        let (c, e) = Cfg::from_text("");
        assert!(e.is_none() && c.enabled && c.prepare_inactive);
        assert_eq!((c.default_lang(), c.policy(true).min_headroom), ("en", 64 * 1024));
        let (c, _) = Cfg::from_text("lang = \"ES\"\nheadroom_kib = 1\n");
        assert_eq!((c.default_lang(), c.policy(false).min_headroom), ("es", 4 * 1024));
        let (c, e) = Cfg::from_text("enabled = 3");
        assert!(e.is_some() && c.enabled);
    }
}
