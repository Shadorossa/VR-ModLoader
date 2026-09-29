# Changelog

All notable changes to this project are documented here. Versions follow [Semantic Versioning](https://semver.org/);
the plugin API has its own version number (`EVT_PLUGIN_API_VERSION`, see `sdk/README.md` §14).

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
