//! The generic pieces (no audio in here) come from the shared **VR-Framework** crate (`crates/vr-framework`); this
//! module keeps the names the audio engine, its `audio_build` tool and its tests use:
//!
//! * [`ModDir`] / [`mods_with`] — per-mod folder discovery (active mods in load order that have a given file);
//! * [`overlay_winner`] — which mod's `files\` serves a game path (the overlay rule: the one that loads last);
//! * [`merge_by_key`] — cross-mod merge rule: same key from several mods → the one that loads last wins, with a
//!   conflict note;
//! * [`Cache`] — hash-keyed build cache (`<evt_loader>\cache\<plugin>\…` + a `.stamp` per output);
//! * [`GameFiles`] — where the game's own files come from (plugin: `game_file_path`; tools: a dump folder).

pub use vr_framework::cache::{file_stamp, hash_inputs, StampCache as Cache};
pub use vr_framework::discover::mods_with;
pub use vr_framework::game::{overlay_winner, DumpFiles, GameFiles};
pub use vr_framework::layer::{merge_by_key, MergeConflict as Conflict};
pub use vr_framework::ModDir;
