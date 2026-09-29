//! The generic engine plumbing (nothing here knows about texts) lives in the shared **VR-Framework** crate
//! (`crates/vr-framework`); this module keeps the paths the text engine, its tool and its tests use.
//!
//! * [`discover`]: per-mod declaration files (`<name>.toml` + `<name>\*.toml` in every mod folder).
//! * [`layer`]: cross-mod merge of keyed values with precedence tiers, load order and conflict notes.
//! * [`game`]: where a base file comes from (a whole-file override of another mod, else the game: cpk_list → loose
//!   file or CPK), with the files read as cache dependencies.
//! * [`cache`]: input hash + dependency stats + JSON manifest (same rules as the ModLoader's data-delta cache).
//! * [`slots`]: serving generated files through the mods overlay without a loader API.

pub use vr_framework::{cache, discover, game, layer, slots, Lvl, ModDir, Notes};
