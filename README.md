# VR-ModLoader

**A mod loader, mod framework and mod manager for Inazuma Eleven Victory Road (PC, v7.1.2).**

VR-ModLoader lets the game run mods without patching `nie.exe` and without rewriting its archives. A mod is a folder
in `<game>\mods\<id>\`; enabling, disabling or removing it is a click (or deleting the folder), and several mods can be
installed side by side with a defined load order and clear conflict reports.

The project has three parts that work together:

| Part | What | Source |
|---|---|---|
| **VR-ModLoader** (core) | `winmm.dll` next to `nie.exe`: loads the mods, serves their files, runs their Lua patches, applies their data deltas and hosts their native plugins | `crates/vr-loader` |
| **VR-Framework** (engines) | plugin mods other mods build on: audio, text, match rules, per-mod save data | `crates/plugins/*`, `mods/*` |
| **VR-ModLoader.exe** (manager + Studio) | installs the ModLoader, installs / enables / orders mods from zips or 1-click links, and indexes the player's own game data for modders | `crates/vr-modloader-app`, `crates/vr-index` |

Plus the **SDK** for mod authors (`sdk/`: reference, C header, examples) and the `evt-mod` command-line tool.

> **Status: 1.0.0-pre.** Built and unit-tested; parts of it (see [CHANGELOG.md](CHANGELOG.md)) are not yet tested in
> game. Expect changes before 1.0.0.

## Features

* **Mod folders** with a `mod.toml` manifest: dependencies (`requires = ["id>=1.2"]`), `provides`, `conflicts`,
  `loader_min`, priorities, profiles, `enabled.toml` / `load_order.toml`.
* **File overlay**: a mod's `files\data\...` replaces or adds game files at run time. No CPK or `cpk_list` is modified,
  so uninstalling a mod leaves the game exactly as it was.
* **Data deltas**: cell-level changes to game tables (`data\<table>.toml`, `[[set]]` / `[[add]]`), merged across mods
  at boot, so two mods can edit the same table.
* **Lua patches**: plain-text Lua files run inside the game's own scripts right after they load (menus and more),
  matched by fingerprints so a patch never runs in the wrong script.
* **Native plugins** (C ABI, plugin API v1): chained inline / IAT hooks, byte signatures, SEH-guarded memory access,
  Lua commands, per-mod configuration, file serving, an early phase at the exe entry point. Rust SDK crate + C header.
* **Voice packs**: extra voice languages chosen from the game's own options.
* **Built-in fixes**: closing the window always ends the process, also during a match (`quit_fix`); characters added
  by mods are legal (no placeholder face, `chara_legal`).
* **Diagnostics**: `evt_loader\loader.log`, optional console window with live game event categories, Lua error
  capture, hang watchdog.
* **Safety net**: the loader only activates on the exact v7.1.2 executable (SHA-1 check); on anything else it only
  forwards `winmm.dll` and the game runs unmodded.

### VR-Framework engines

| Engine | Mod folder | What |
|---|---|---|
| `audio_engine` | `mods/audio_engine` | voices, sound effects and music from plain audio files (`audio.toml`), built at boot on the player's PC from their own banks (Rust HCA encoder, cached), merged per cue across mods; per-armour shouts |
| `text_engine` | `mods/text_engine` | game texts by readable keys in the game's 9 languages: replace retail texts, add new ones with stable ids, merged across mods |
| `match_engine` | `mods/match_engine` | rulesets for every offline match (length, halves, players, extra time, golden goal, penalty shootout), assigned by mode, match or rival |
| `save_engine` | `mods/save_engine` | per-mod persistent data kept apart from the game's save and tied to the player's save slot |
| `clean_hud` (plugin only) | - | the plugin of the Clean HUD mod: no team editor between the VS screen and the walk-in in offline matches |
| `quit_fix` (SDK test) | `mods/quit_fix` | the built-in quit fix as a stand-alone plugin; kept as a small, real example of the plugin API |

## Requirements

* **A legitimately owned copy of Inazuma Eleven Victory Road for PC (Steam), version 7.1.2.** Other versions are not
  supported (the loader stays inactive on them).
* Windows 10 / 11, x64.

## Install (players)

1. Download the latest release from the project's release page (GitHub Releases, GameBanana or Nexus Mods).
2. Run `VR-ModLoader.exe`: it finds the game, installs the ModLoader (`winmm.dll` + `evt_loader\`) and manages your
   mods (drop a mod `.zip` on it, or use a 1-click install link).
3. Start the game from Steam as usual. `<game>\evt_loader\loader.log` tells you what was loaded.

Manual install: copy `winmm.dll` and the `evt_loader\` folder next to `nie.exe`, and put mods in `<game>\mods\<id>\`.
To uninstall, remove `winmm.dll`, `evt_loader\` and `mods\` (the ModLoader never modifies the game's own files).

## Build from source

Requirements: Rust (stable, MSVC toolchain `x86_64-pc-windows-msvc`) and the Visual Studio C++ build tools.

```powershell
cargo build --release                         # everything
cargo build --release -p vr-loader            # target\release\vr_loader.dll -> install as <game>\winmm.dll
cargo build --release -p vr-modloader-app     # target\release\VR-ModLoader.exe
cargo build --release -p evt-plugin-audio-engine -p evt-plugin-text-engine -p evt-plugin-match-engine -p evt-plugin-save-engine
cargo test --release                          # unit tests (no game files needed)
```

A plugin mod is its folder under `mods/` plus the DLL built from `crates/plugins/<name>` (e.g. `audio_engine.dll`).

Tests that check signatures or formats against the real game read **your own** copy: put (or link) your extracted
v7.1.2 files at `assets\v7.1.2\` (`nie.exe`, `data\...`), or set the environment variables named in each test
(`EVT_NIE_EXE`, `EVT_DUMP`, `VR_DATA`, ...). They are skipped when the files are absent. **Never commit game files**:
`assets/` is in `.gitignore`.

## Making mods

Start with [`sdk/README.md`](sdk/README.md): mod format, load order, Lua patches, data deltas, configuration, the
plugin API v1 function by function, chained hooks, the console and the versioning policy. Working examples:

* `sdk/examples/example_data_only` - a mod that only ships files;
* `sdk/examples/example_lua_only` - a Lua patch;
* `sdk/examples/example_rust_plugin` - a native plugin in Rust (`crates/evt-plugin-sdk`);
* `sdk/examples/example_c_plugin` - the same plugin in C (`sdk/evt_plugin.h`).

`evt-mod` (`cargo run -p evt-mod -- ...`): `new`, `check`, `list`, `pack` for mod folders. The engines' own
READMEs (`mods/*/README.md`) describe their file formats (`audio.toml`, texts, rulesets, save data).

## Repository layout

| Path | What |
|---|---|
| `crates/vr-loader` | the ModLoader (`winmm.dll`) |
| `crates/evt-plugin-sdk`, `sdk/` | plugin API v1: Rust SDK crate, C header, reference, examples |
| `crates/evt-modfmt`, `crates/evt-mod` | mod folder format (shared by loader, manager and tools) and its CLI |
| `crates/plugins/*`, `mods/*` | VR-Framework engines (plugin crates) and their mod folders |
| `crates/vr-modloader-app`, `crates/vr-index`, `crates/evt-installer` | manager exe, local game index (built from the player's own data), install library |
| `crates/l5-core`, `crates/l5-cpk`, `crates/cri`, `crates/g4-texture` | format libraries: cfg.bin (T2B / RDBN), CPK + `cpk_list` encryption, CRI ADX2 (ACB / AWB / HCA), G4TX textures + DDS/BCn |
| `crates/vr-gamefiles` | game file access: read / write `cpk_list`, extract from the CPKs, exact install / restore of loose data files |
| `test-mods/` | small mods used by tests |

Some source comments cite design notes and scripts of the original development repository (paths such as `docs/...`
or `research/...`); those files are not part of this repository. The code and the SDK reference are the source of
truth.

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md).

## License

VR-ModLoader is free software under the **GNU General Public License v3.0** ([LICENSE](LICENSE)): if you distribute a
modified version, you must publish its source under the same license. Third-party crates: [NOTICE](NOTICE).

The plugin SDK (`evt-plugin-sdk`, `sdk/evt_plugin.h`) is under the same GPL-3.0 with no linking exception: a native
plugin built against it is a derivative work, so a distributed plugin must be GPL-3.0 compatible and ship its source.
Closed-source plugins are not allowed.

## Disclaimer

VR-ModLoader is an **unofficial fan project**. It is not affiliated with, endorsed by or sponsored by LEVEL-5 Inc. or
any of its partners. "Inazuma Eleven" and "Inazuma Eleven Victory Road" are trademarks of their respective owners.
This repository contains **no game files**, no extracted game data and no copyrighted game assets; all tools work on
the player's own, legitimately owned copy of the game. Use at your own risk and keep a backup of your saves.
