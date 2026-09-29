# Changelog

All notable changes to this project are documented here. Versions follow [Semantic Versioning](https://semver.org/);
the plugin API has its own version number (`EVT_PLUGIN_API_VERSION`, see `sdk/README.md` §14).

## [1.0.0] - 2026-09-29

First release with binaries: `VR-ModLoader-1.0.0.zip` (unzip into the game folder, see README.md «Install»).

### Release layout

* The zip mirrors the game folder: `winmm.dll` (the ModLoader), `VR-ModLoader.exe` (the manager, with the same
  ModLoader built in), `mods\` with the four VR-Framework engines (`audio_engine`, `text_engine`, `match_engine`,
  `save_engine`: mod folder + DLL), `README.txt`, `LICENSE`, `NOTICE`, `CHANGELOG.md`. The ModLoader creates
  `evt_loader\config.toml` with its defaults at the first start.
* `tools/release.ps1` builds and assembles it and prints the zip's SHA-256.

### ModLoader

* Version marker: `winmm.dll` embeds `EVT_MODLOADER_VERSION=<version>` (the package version of `crates/vr-loader`,
  also `MODLOADER_VERSION` and the version logged at start), so tools can read the version of an installed DLL.

### VR-ModLoader.exe (manager)

* Reads the installed ModLoader's version from the marker: "ModLoader 1.0.0" / "update available" instead of
  "unknown version".
* The game folder is the folder of `VR-ModLoader.exe` when `nie.exe` is next to it; elsewhere it falls back to the
  saved folder / Steam detection and warns that the exe should be in the game folder.
* «Install» / «Update» / **«Repair»** of the ModLoader from the built-in copy: an unzipped release is repaired or
  updated file by file (the player's `evt_loader\config.toml` is kept), anything else is installed with a backup of
  what it replaces; a newer installed ModLoader is never downgraded.
* «Remove» also removes an unzipped release (`winmm.dll`, `evt_loader\`), keeping the mods folder;
  «Remove and delete VR-ModLoader.exe» removes the manager too.
* New look: dark "newspaper" theme with bundled OFL fonts (IBM Plex, Playfair Display, Source Serif 4), masthead
  status line, two-column mods view, secondary «More» section.

### VR-Framework

* New shared crate `crates/vr-framework` (mod discovery, strict TOML data files with schema versions, namespaced stable
  ids, cross-mod merge layers, build cache, game files, Lua helpers, mod options); `audio_engine`, `text_engine`,
  `save_engine` and `match_engine` are built on it.
* `sdk/examples/example_engine`: the smallest engine on the framework.
* `text_engine`: the merged text tables are written to `evt_loader\cache\text_engine\` and served with `file_serve`
  at the early phase, so texts show from the first start (fixed-size slots only as the fallback of an old ModLoader;
  their leftovers are deleted).
* `audio_engine`: the **"Voice pack"** row in Options › Game settings (None / each installed voice pack), built at
  startup from the player's own settings list, texts in 9 languages (`text.toml`), Lua in
  `lua/setting_menu/110_voice_pack.lua`; `[voice_row] enabled = false` turns it off.

### Not yet tested in game

* The public build of the loader (`winmm.dll` from this repository) and the engines with it, the manager's
  repair / remove of an unzipped release, the "Voice pack" row. Report problems with `evt_loader\loader.log`.

## [1.0.0-pre] - 2026-09-29

First public source release (pre-release). Supported game: Inazuma Eleven Victory Road PC **v7.1.2** only.

### ModLoader (`winmm.dll`, `MODLOADER_VERSION` 1.0.0, plugin API v1)

* Mod folders `<game>\mods\<id>\` with `mod.toml`: `requires` / `provides` / `conflicts` / `loader_min` /
  `loader_modules`, priorities, `enabled.toml`, `load_order.toml`, profiles; conflict reports in `loader.log`.
* File overlay of `files\data\...` (no CPK / `cpk_list` modification), preview pictures for the in-game Mods menu.
* Data deltas (`data\<table>.toml`, `[[set]]` / `[[add]]`) merged across mods at boot and cached.
* Lua patch runner with script fingerprints (mods' `lua\` folders and `evt_loader\lua_patches\`).
* Voice packs (extra voice languages).
* Native plugins, plugin API v1: log, config, signatures and RIP resolution, guarded memory access, chained inline and
  IAT hooks with priorities, Lua commands, mod list and providers, game state, threads, early phase at the exe entry
  point, `file_serve`, `game_file_path`.
* Built-in modules: `mods` (on), `lua_bridge` (on), `chara_legal` (on), `quit_fix` (on), `lua_patch`, `debug`,
  `console`. The mod system is on by default.
* Console window (English commands and categories), Lua error capture, hang watchdog.
* The loader resolves `g_sceneSoccer` itself for the `match.*` game-state keys.

### VR-Framework

* `audio_engine` 1.1.0, `text_engine` 1.0.0, `match_engine` 1.0.0, `save_engine` 1.0.0, `clean_hud` plugin 1.0.0,
  `quit_fix` plugin 1.1.0 (SDK test).

### Tools

* `VR-ModLoader.exe` (manager + Studio index search), `vr-index` (local game index built from the player's own
  files), `evt-mod` (new / check / list / pack), `audio_build`, `evt-text-engine`.

### Known limitations

* Not yet tested in game: the SDK examples, the console outside the author's setup, the plugins other than
  `quit_fix`, and this repository's split build of the loader (the private development build carries more modules).
* `audio_engine`'s name index (`index\audio_index.json`) must be generated from the player's own game data; it is not
  shipped.
* See `sdk/README.md` §15 for API features that are planned but not in v1.
