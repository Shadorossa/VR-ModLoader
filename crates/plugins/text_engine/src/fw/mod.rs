//! **Generic engine plumbing** (nothing here knows about texts). Candidates for the shared `vr-framework` crate
//! (docs/app/modloader-roadmap.md «VR-Framework»); the text engine only plugs its format into them.
//!
//! * [`discover`]: per-mod declaration files (`<name>.toml` + `<name>\*.toml` in every mod folder).
//! * [`layer`]: cross-mod merge of keyed values with precedence tiers, load order and conflict notes.
//! * [`game`]: where a base file comes from (a whole-file override of another mod, else the game: cpk_list → loose
//!   file or CPK), with the files read as cache dependencies.
//! * [`cache`]: input hash + dependency stats + JSON manifest (same rules as the ModLoader's data-delta cache).
//! * [`slots`]: serving generated files through the mods overlay without a loader API: fixed-size files in the
//!   engine's own `files\` folder, rewritten in place at the early phase (padding supplied by the format).

pub mod cache;
pub mod discover;
pub mod game;
pub mod layer;
pub mod slots;

use serde::{Deserialize, Serialize};

/// Level of a build note (logged by the plugin, kept in the cache manifest and repeated on a cache hit).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Lvl {
    Error,
    Warn,
    Info,
    Debug,
}

/// Log lines of a build.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Notes(pub Vec<(Lvl, String)>);

impl Notes {
    pub fn push(&mut self, l: Lvl, s: impl Into<String>) {
        self.0.push((l, s.into()));
    }
    pub fn error(&mut self, s: impl Into<String>) {
        self.push(Lvl::Error, s)
    }
    pub fn warn(&mut self, s: impl Into<String>) {
        self.push(Lvl::Warn, s)
    }
    pub fn info(&mut self, s: impl Into<String>) {
        self.push(Lvl::Info, s)
    }
    pub fn debug(&mut self, s: impl Into<String>) {
        self.push(Lvl::Debug, s)
    }
    pub fn extend(&mut self, o: Notes) {
        self.0.extend(o.0)
    }
    /// Lines at `l` or more severe.
    pub fn at(&self, l: Lvl) -> impl Iterator<Item = &str> {
        self.0.iter().filter(move |(x, _)| *x <= l).map(|(_, s)| s.as_str())
    }
}

/// One active mod as the engines need it (from the ModLoader's mod list or an `evt_modfmt` load plan).
#[derive(Debug, Clone, PartialEq)]
pub struct ModDir {
    pub id: String,
    pub dir: std::path::PathBuf,
    /// Position in the load order (later = wins conflicts).
    pub load_index: u32,
}
