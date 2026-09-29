# VR-Framework

The shared base of the **engines** of VR-ModLoader (Inazuma Eleven Victory Road PC v7.1.2). License: GPL-3.0-only.

An engine is a native plugin (a DLL made with `evt-plugin-sdk`) that takes over one system of the game: texts, audio,
saves, match rules, characters… Mods ship small, readable data files (TOML, wav, png) and the engine turns them into
what the game reads, merged across every active mod. Every engine needs the same plumbing, and this crate is where it
lives. The engines of the repository all use it: `crates/plugins/text_engine`, `audio_engine`, `save_engine`,
`match_engine`.

Layers:

```
VR-ModLoader (winmm.dll)   loads mods and plugins, serves files, hooks, console          (does NOT use this crate)
VR-Framework               this crate + the engines on top of it, shipped as the package vr_framework
Mods                       data files for the engines: requires = ["vr_framework>=1.0"] or just the engines they use
```

## What it gives

| Module | What it gives |
|---|---|
| `discover` | the data of an engine in the mod folders: `<mod>\<name>.toml` + `<mod>\<name>\*.toml` (`read`), `<dir>\*.toml` sorted without `_*.toml` (`toml_files`), mods that have a file (`mods_with`), installed mods (`installed`), mods that use an engine through `requires` / `provides` (`uses_engine`, `mod_uses_engine`) |
| `host` | the ModLoader side: active mods in load order and installed-but-inactive mods as `ModDir`, the game / `evt_loader` folders, the engine's cache folder (`cache_dir` = `evt_loader\cache\<engine>`), the serving mode of the early phase (`EarlyMode`), `file_serve` of a set of files (`serve_files`), logging of notes (`log_notes`), the game's own files through `game_file_path` (`HostGame`) |
| `diag` | `Notes`: the error / warn / info / debug lines a build returns (the plugin logs them, a tool prints them, a cache keeps them); `Diagnostic`: a problem in a data file with file, line and key |
| `data` | strict TOML data files: `schema = "<kind>/<version>"` checked, unknown keys / missing keys / values outside an enum reported with file, line and key (`parse_strict`, `load_dir`) |
| `ids` | mod ids (`valid_mod_id`), own ids `<mod_id>.<name>` (`own_id`, `split_own`, `valid_name`), game ids = standard CRC-32 of the name (`crc32`, `parse_id`), stable numbers for new entries (`probe_id`, `IdRegistry` persisted in `evt_loader\ids\<engine>.json`) |
| `layer` | cross-mod merge rules: claims with precedence tiers where the mod that loads later wins at equal tier and two explicit claims are a conflict (`Layer`), and the plain "last one wins" (`merge_by_key`) |
| `cache` | build cache: input hash (`InputHash`), size + mtime of every file read (`Dep`, `stats`, `unchanged`), JSON manifest (`read_json`, `write_json`, `write_atomic`); per-output stamps (`StampCache`, `hash_inputs`, `file_stamp`) |
| `game` | where a base file comes from: another mod's whole file, else the game (`Source`, `base_file`, `latest_versioned`); the overlay winner (`overlay_winner`); the game's files on disk (`GameFiles`, `DumpFiles`); the installed game read from `cpk_list` + CPKs (`GameSource`, feature `gamefiles`) |
| `slots` | serving generated files on a ModLoader without `file_serve`: fixed-size files in the engine's own `files\`, rewritten in place |
| `lua` | `CMND_EVT_*` commands: `register_all`, naming rule (`valid_command`), Lua ↔ JSON (`num_value`, `str_value`, `push_json`), keys (`valid_key`), `WarnOnce`, `cstr` for DLL exports |
| `options` | a mod's settings: `options.toml` (`toggle` / `list` / `number`, label and help as text keys, default, storage key) parsed strictly (`parse`, `declared`), current values with their defaults (`ModOptions::load`, `get`, `set`) |
| `state` | the loader's numbered event rings read through `game_state` (`ring_pending`) |
| `fsx` | `write_atomic` (tmp + flush + rename), `unix_now`, `key_path` (a game key under a folder) |

Feature `gamefiles` (off by default) adds `game::GameSource`, which pulls in the game-file crates; a plugin that only
reads the game through the ModLoader (`game_file_path`) does not need it.

## Format rules (every engine)

The framework enforces what it can, the engine's types do the rest:

1. **References use the game's internal ids**: `c01020010`, `whs01980`, `menu_text:sysmes_foo`. The game names things
   by the CRC-32 of the name (`ids::crc32`); `ids::parse_id` reads a number, `0x…`, a negative number or a name.
   Readable names are only a help of the tools (the Studio lists them).
2. **Own ids are namespaced by the mod and stable**: a modder writes `name = "kaito"`, the engine makes it
   `my_mod.kaito` (`ids::own_id`); another mod refers to it by the full id. The number the game needs comes from
   `ids::IdRegistry::assign` (`crc32("my_mod.kaito")`, probing `#1`, `#2`… on collision) and is **remembered** in
   `evt_loader\ids\<engine>.json`, so it never changes between starts, even if a later mod or game file takes the
   number it was derived from. Keep that file outside the cache: wiping the cache must not renumber anything.
3. **Keys and values are English**: `element = "fire"`, never `elemento = "fuego"`. Use Rust enums with
   `#[serde(rename_all = "snake_case")]`: a value outside the enum is an error with its line.
4. **Every data file has a schema version**: `schema = "<kind>/<version>"` as a top-level line
   (`"vr_framework.character/1"`, `"example_engine.greetings/1"`). The engine reads versions `1..=N` of its kind; a newer
   file asks the player to update the engine instead of being misread.
5. **Validation is strict**: an unknown key is an error that names the file, the line and the key
   (`mods/my_mod/character/kaito.toml:12: unknown key `elemnt` (expected one of …)`). The file is left out, the other
   files still load, the game keeps running.

6. **Every setting a mod adds lives in the Opciones tab «Opciones de mods»** (to the right of the graphics tab),
   declared in the mod's `options.toml`; no mod builds its own settings menu. The tab is the `mod_options` component
   (planned); engines and mods read the values through `options`:

   ```toml
   schema = "vr_framework.options/1"

   [[option]]
   key = "difficulty"                    # storage key (lower-case English, unique in the mod)
   type = "list"                         # toggle | list | number
   label = "my_mod.options.difficulty"    # text key (text engine) of the label
   help = "my_mod.options.difficulty_help"   # optional text key of the help line
   values = ["easy", "normal", "hard"]   # list: stored values; labels = [...] optional text keys per value
   default = "normal"                    # number: min, max, step (default 1)
   ```

   ```rust
   let opts = vr_framework::options::ModOptions::load(&loader_dir, &h.mod_id, &h.mod_dir, &mut notes);
   let hard = opts.get("difficulty").and_then(|v| v.as_str()) == Some("hard");
   ```

   Values are kept in `evt_loader\mod_options\<mod id>.json` (`{"<key>": value}`; the location is reserved here and
   aligned with `mod_options` when it lands); a missing or no-longer-valid value reads as the default.

`data::parse_strict` / `data::load_dir` implement 4 and 5. Serde is where unknown keys are found, so **every struct of
a data file needs `#[serde(deny_unknown_fields)]`**; the type does not declare `schema` (the framework checks it and
hides it from serde, keeping the line numbers).

## Writing an engine

Start from `sdk/examples/example_engine` (about 100 lines). The shape of every engine:

```rust
use evt_plugin_sdk::{declare_plugin, Host};
use vr_framework::host::{self as fw, EarlyMode};
use vr_framework::{cache, data, discover, fsx, Notes};

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct MyFile { /* the data format, English keys */ }

fn early(h: &'static Host) -> Result<(), String> {
    let mut notes = Notes::default();
    // 1. the mods that use the engine, in load order
    let users: Vec<_> = fw::active_mods(h).into_iter().filter(|m| discover::mod_uses_engine("my_engine", &h.mod_id, m)).collect();
    // 2. cache: skip the build when nothing changed (engine version + every data file + the game files read)
    let mut ih = cache::InputHash::new("my_engine v1");
    for m in &users {
        for p in discover::toml_files(&m.dir.join("my")) { ih.str(&m.id).file(&p); }
    }
    // 3. build: strict data, merge (layer), ids (IdRegistry), base files of the game (game / HostGame)
    let files = data::load_dir::<MyFile>(&users, "my", "my_engine.thing", 1, &mut notes);
    let out = fsx::key_path(&fw::cache_dir(h), "data/common/…/generated.cfg.bin");
    // … write `out` with cache::write_atomic, keep the hash in a manifest …
    // 4. serve at the early phase
    match EarlyMode::of(h) {
        EarlyMode::Serve => { fw::serve_files(h, [("data/common/…/generated.cfg.bin", out.as_path())]); }
        EarlyMode::Late => { /* shown from the next start */ }
        EarlyMode::Legacy => { /* old ModLoader: crate::slots fallback, or ask to update */ }
    }
    fw::log_notes(h, &notes);
    Ok(())
}

fn init(h: &'static Host) -> Result<(), String> {
    // Lua commands (module lua_bridge): CMND_EVT_MY_ENGINE_*
    vr_framework::lua::register_all(h, &[/* ("CMND_EVT_MY_ENGINE_GET", cmd_get) */]);
    Ok(())
}

declare_plugin!(init = init, early = early);
```

Rules of thumb:

* **Build at the early phase, serve with `file_serve`.** The early phase runs at the exe entry point, before any game
  code: a file served there is read by the game at its real size from the first start. `file_serve` only accepts
  files below the game folder: write outputs to `host::cache_dir`. On a ModLoader without `file_serve`
  (`EarlyMode::Legacy`) either use `slots` (the text engine does) or tell the player to update.
* **Base files**: start from what the game would read without your engine (`game::base_file` with the whole file of
  another mod when one ships it, else the game), never from a copy shipped with your mod. In the plugin,
  `host::HostGame` asks the ModLoader (`game_file_path`: loose-first, else extracted from the CPK); offline tools use
  `game::GameSource` (feature `gamefiles`) or `game::DumpFiles`.
* **Merge across mods**: load order decides; the mod that loads later wins a conflict and the conflict is a WARN in
  the log (`layer`). Report, never fail the game: a broken file is an ERROR note and is skipped.
* **Cache**: hash every input that changes the output (engine version, the mods' data files in load order, the game
  files read with `cache::stats`); bump the engine version when the same inputs give another output.
* **Lua commands** start with `CMND_EVT_<ENGINE>_` (`lua::valid_command`); the mod needs
  `loader_modules = ["lua_bridge"]` in its `mod.toml`. A Lua command cannot know which mod's script called it: take
  the mod id as an argument when it matters (the save engine does).
* **Per-slot save data** is the save engine's job (`save_engine`: Lua `CMND_EVT_SAVE_*`, C exports
  `save_engine_get/set/commit`); an engine that needs to remember something per save slot calls it instead of writing
  its own files.

## Where each engine stands

| Engine | Uses |
|---|---|
| `text_engine` | `discover`, `layer`, `game` (+ `GameSource`), `cache`, `slots`, `ids` (`probe_id`, `parse_id`), `host`, `lua` (its `fw` module re-exports them) |
| `audio_engine` | `discover::mods_with`, `game` (`overlay_winner`, `GameFiles`, `DumpFiles`), `layer::merge_by_key`, `cache::StampCache`, `host` (its `framework` module re-exports them) |
| `save_engine` | `fsx`, `ids::valid_mod_id`, `lua` (values, `push_json`, `cstr`), `state::ring_pending` |
| `match_engine` | `discover` (`toml_files`, `uses_engine`), `host::active_mods`, `lua::register_all`, `ids::crc32` |

Not in the framework (yet): the cell-level merge of cfg.bin tables (`[[set]]` / `[[add]]` data deltas) lives in the
ModLoader (`crates/vr-loader/src/mods/merge.rs`) and no engine shares it today; a Character / Team engine will need it,
and then it moves to a crate both can use. The strict `data` loader is for new formats: the text engine's `text.toml`
and the match engine's rulesets keep their own parsers (they already have users and their own lenient rules for
deprecated keys).
