# VR-ModLoader SDK - making mods for Inazuma Eleven Victory Road (PC)

This folder is everything you need to write a mod for **VR-ModLoader**, the `winmm.dll` that is loaded next to
`nie.exe` and lets the game run mods without patching the executable or rewriting its archives.

| File | What |
|---|---|
| `README.md` | this reference (mod format, load order, plugin API v1, console, versioning) |
| `evt_plugin.h` | the plugin API for **C / C++** (single header, hand-written mirror of the Rust ABI) |
| `examples/example_data_only` | a mod that only ships files |
| `examples/example_lua_only` | a mod that only ships a Lua patch (logs a line on the title menu) |
| `examples/example_rust_plugin` | a native plugin in Rust (`crates/evt-plugin-sdk`): log, config, Lua command, chained hook |
| `examples/example_c_plugin` | the same plugin in C with `evt_plugin.h` (`build.bat` / CMake) |
| `examples/example_engine` | an **engine** on the VR-Framework (`crates/vr-framework`): reads `<mod>\example\*.toml` of the mods that require it, merges them, serves one generated file (guide: `crates/vr-framework/README.md`) |

The Rust SDK crate is `crates/evt-plugin-sdk` in the repository (zero dependencies).

> **Supported game build: Inazuma Eleven Victory Road PC v7.1.2 only** (`nie.exe` SHA-1
> `d27e76217730783fec8df4a3b0541cb15fe8f100`). On any other build the ModLoader logs an error and stays inactive; plugins are
> not started.

Contents: [1 What a mod is](#1-what-a-mod-is) - [2 Install](#2-install) - [3 Folder layout](#3-folder-layout) -
[4 mod.toml](#4-modtoml-reference) - [5 Load order](#5-load-order-and-conflicts) - [6 Mod content](#6-mod-content) -
[7 Configuration](#7-configuration) - [8 Plugins](#8-plugins-native-code) - [9 API v1](#9-plugin-api-v1-reference) -
[10 Hooks](#10-chained-hooks) - [11 Console and logs](#11-console-and-logs) - [12 Tools](#12-tools) -
[13 Safety](#13-safety) - [14 Versioning](#14-versioning-and-compatibility-policy) - [15 Not there yet](#15-not-there-yet)

---

## 1. What a mod is

A mod is **one folder** in `<game>\mods\<id>\` with a `mod.toml` and any mix of:

* **files** that replace or add game files (textures, tables, text, audio banks) - no game archive is touched;
* **Lua patches** that run inside the game's own Lua scripts (menus and other scripts), right after each script loads;
* **data deltas** (`data\*.toml`, cell-level changes to game tables so several mods can edit the same table);
* a **plugin**: a native DLL (`plugin = "x.dll"`) with hooks, Lua commands and full access to the game's code.

The loader stacks the mods in a defined order (dependencies first), reports conflicts in a log, and removing a mod is
deleting or disabling its folder. The ModLoader itself contains nothing of any particular mod.

## 2. Install

The ModLoader consists of `winmm.dll` (placed in the folder that contains `nie.exe`) and an `evt_loader\` folder next to
it (`config.toml`, `loader.log`, global Lua patches). A release archive / installer for players is still to be published (see
[15](#15-not-there-yet)); developers build it from the repository: `cargo build -p vr-loader --release` produces
`target\release\vr_loader.dll`, which is installed as `winmm.dll`.

After the first start `evt_loader\config.toml` exists. **The mod system is a loader module and is on by default**:

```toml
[modules]
mods = true          # <game>\mods\<id>\ folders (files, Lua patches, plugins)
```

`lua_bridge` (the Lua command bridge that plugins register commands on) is on by default; `debug = true` makes Lua
`print(...)` write `LUAPRINT` lines to the log; `console = true` opens the console window ([11](#11-console-and-logs)).
Everything the loader does is written to **`<game>\evt_loader\loader.log`**: that file is the first place to look when
a mod does not work.

Mods that name built-in loader modules in `loader_modules` switch them on automatically while they are active.

### 2.1 The mod manager: `VR-ModLoader.exe`

A single exe (GPL-3.0, source `crates/vr-modloader-app`, no installer) for players:

* finds the game (Steam libraries), checks it is v7.1.2, and installs / updates / removes the ModLoader (what it
  replaces is backed up in `<game>\evt_backup\componentes\_modloader\` and put back on removal);
* lists `<game>\mods`: enable with the check box, set the priority by **dragging a row** (top = loads last = wins),
  profiles, per-mod status (missing / disabled / too old requirements, `conflicts`, `loader_min` newer than the
  installed ModLoader) and a Conflicts panel (files, table cells, new rows, `audio.toml` cues) with the winner;
  **Save** writes `mods\enabled.toml` + `mods\load_order.toml` (the files the ModLoader and its in-game Mods menu
  use), **Launch game** saves first and starts the game through Steam;
* installs a mod from a `.zip` (button, or drop it on the window): every mod folder in it is checked with the same
  rules as `evt-mod check`, then goes to `mods\<id>\`; the copy it replaces, and an uninstalled mod, go to
  `mods\_trash\` (nothing is deleted unless you empty the trash); missing `requires` are offered afterwards;
* **1-click install**: after you press *Register 1-click links* in Settings, links such as
  `vrmodloader:https://gamebanana.com/mmdl/<file>,Mod,<id>` or `vrmodloader:https://example.org/my_mod.zip` open the
  manager, which asks, downloads over HTTPS and installs. Registration is per user
  (`HKCU\Software\Classes\vrmodloader`) and *Unregister* removes it.

For mod authors: ship your mod as a zip whose folder holds `mod.toml` (`my_mod\mod.toml`, or `My Mod 1.0\my_mod\mod.toml`);
several mods in one zip are fine (a mod plus the library it requires). English and Spanish UI.

## 3. Folder layout

```
<game>\
    nie.exe
    winmm.dll                          the ModLoader
    evt_loader\
        config.toml                    loader configuration (+ per-mod overrides, section 7)
        loader.log                     the log
        lua_patches\                   optional global Lua patches (same layout as a mod's lua\)
    mods\
        enabled.toml                   enabled = ["my_mod", ...]   (missing file = every installed mod is enabled)
        load_order.toml                order = ["winner", "other", "base"]   (top of the list loads last and wins)
        profiles\<name>.toml           saved enabled/order sets, profile.toml says which one is active
        my_mod\                        one folder per mod (folder name = id)
            mod.toml                   manifest (required)
            config.toml                default configuration for a plugin (optional)
            preview.png                picture for the in-game Mods menu (optional, cropped to 16:9)
            my_mod.dll                 plugin (optional)
            files\data\...             full files that replace / add game files
            lua\<script>\NN_name.lua   Lua patches; lua\_all\ = every script; lua\_fingerprints.json
            data\<table>.toml          data deltas
```

Folders in `mods\` whose name starts with `.` or `_` are ignored silently (handy for parking a mod: `_my_mod`).
A folder without `mod.toml` is ignored with a warning; a folder whose name differs from the `id` gives a warning.

## 4. mod.toml reference

```toml
id = "my_mod"                          # required
name = "My mod"
version = "1.0.0"                      # required
author = "me"
description = "What it does"
priority = 0
requires = ["base_lib", "db>=3.0", "match_engine_api>=1"]
conflicts = ["other_mod"]
loader_min = "1.0.0"
loader_modules = ["lua_bridge"]
plugin = "my_mod.dll"
provides = ["my_api=2"]
tags = ["Graphics", "Players"]
updated = "2026-09-29"
voice_language = { code = "es", name = "Español" }
```

Unknown keys only produce a warning (`unknown key ...`), so a mod written for a newer loader still loads on an older one
(the new fields are simply ignored: use `loader_min` to refuse instead).

| Field | Type | Meaning |
|---|---|---|
| `id` | string, **required** | 1-64 characters of `a-z 0-9 _ . -`, first one `a-z` or `0-9`. Must equal the folder name. Unique: with two folders of the same id the first (alphabetical) wins. |
| `version` | string, **required** | Free text, compared part by part (see below). Shown in the mods menu and in logs. |
| `name` | string | Display name (empty gives a warning). |
| `author`, `description` | string | Shown in the mods menu / tools. |
| `priority` | integer, default 0 | Base load order: **lower loads first**; among mods with no other constraint the later one wins conflicts. Ties break by `id`. |
| `requires` | list of strings | Mods (or `provides` names) that must be active. Entry = `name` or `name<op>version` with `>=`, `>`, `=` (or `==`), `<=`, `<`, e.g. `"db>=3.0"`. A mod that requires something missing or too old is **skipped** (reason in the log). The mod also loads **after** everything it requires. |
| `conflicts` | list of ids | If any of these is active, **this** mod is skipped. A mod cannot require and conflict with the same id, nor with itself. |
| `loader_min` | version | Lowest ModLoader version the mod runs with (`1.0.0` = plugin API v1). An older loader skips the mod (`needs ModLoader >= X (this one: Y)`); a much older loader that does not know the key only warns, so mods with a plugin must set it. Empty = any. |
| `loader_modules` | list of strings | Built-in `[modules]` switches of the loader the mod needs (e.g. `lua_bridge`). Since ModLoader 1.0.0 they are **turned on automatically** while the mod is active (`mods: loader module x turned on by mod(s) a, b`); an unknown name gives a warning. |
| `plugin` | string | File name of the native plugin in the mod folder: plain name of `a-z 0-9 _ . -`, ends in `.dll`, at most 64 characters, no path. A missing DLL is an error in `evt-mod check` and the in-game menu. |
| `provides` | list of strings | Extra names other mods can put in `requires`: `"name"` or `"name=version"` (no version = the mod's own). A mod always provides its own `id` with its `version`. `requires = ["x>=2"]` is satisfied by a mod with id `x` **or** by one that provides `x=2` (or higher). A plugin that provides the name of a *built-in* loader module replaces that module (the built-in stays off when the plugin loads fine). |
| `tags` | list of strings | Chips / filters in the in-game Mods menu (1-24 characters each, more than 6 warns; the game shows the first 3). |
| `updated` | `YYYY-MM-DD` | Date of the last version (empty = the modification date of `mod.toml`). |
| `voice_language` | `{ code, name }` | Only for a **voice pack**: `code` = 2-3 lowercase letters with an optional variant (`es`, `fr`, `pt-br`; never the retail `ja` / `en`); `name` = what the in-game "voice language" row shows. Its banks go in `files\data\common\sound_asset\<code>\` and only play while the player picks that language. |

**Versions.** `1.2` = `1.2.0` < `1.10`. Parts are split on `.`, `-` and `+`; numeric parts compare as numbers, other
parts as text, a missing part counts as 0, a leading `v` is ignored.

`evt-mod new <id> <parent folder>` writes a commented template.

## 5. Load order and conflicts

1. **Duplicates and enabled list.** Duplicate ids: the first folder alphabetically wins. Mods not in `enabled.toml` are
   off (no file = all on).
2. **Base order.** From `load_order.toml` if it exists (managed by the in-game Mods menu and `VR-ModLoader.exe`: enabling a mod puts it on
   top; ids not listed load *before* every listed one, by priority and id); otherwise by `priority` ascending, then `id`.
3. **Skipping, repeated until nothing changes:** a `loader_min` higher than the loader, a `requires` without a provider
   or with an unacceptable version, a `conflicts` with an active mod. (Skipping one mod can break another's requirement,
   hence the repetition.)
4. **Dependency order (stable topological sort):** each mod loads after the mods that provide its `requires`. If nothing
   forces a change the base order is kept; a mod held back by a dependency goes right after the last dependency.
5. **Cycles** (`a -> b -> c -> a`): logged as an error and every mod in the cycle is skipped; mods depending on them are
   skipped in the next pass.
6. **Conflicts.** Two active mods providing the same file (`files\data\...`), the same table cell or the same new row are
   **not** skipped: it is a WARN in the log and **the mod that loads later wins** (so a mod always wins over its
   dependency). Lua patches all run, in load order (later wraps earlier); the loader warns when two mods redefine the
   same global function without chaining onto it.

The plan is printed at start-up:

```
INFO  mods: 2 enabled: base_lib@1.0 my_mod@1.0.0
WARN  mods: other skipped: conflicts with enabled mod `my_mod`
WARN  mods: conflict: file data/common/x.cfg.bin: base_lib, my_mod (winner: my_mod)
```

`evt-mod list <game>` prints the same plan offline.

## 6. Mod content

### 6.1 `files\data\...` - replace or add files

Put the file at its logical game path starting with `data\`, e.g. `files\data\common\text\es\chara_text.cfg.bin`. A file
that exists in the game (loose or inside a CPK archive) is **replaced**; a new path is **added**. Nothing is copied into the
game and no archive list is rewritten: the loader hooks the engine's file lookup (`ResolveOverlayPath`) and registers the
files in the in-memory file list.

* Names are matched case-insensitively with `/` or `\`; versioned names must be exact.
* Files are served byte for byte: they must be in the format the engine expects (uncompressed, unencrypted game
  tables/textures; CRI audio banks follow the voice-pack rules).
* `data\cpk_list.cfg.bin` cannot be replaced. Files outside `files\data\` and junk (`Thumbs.db`, `desktop.ini`) are ignored
  with a warning.
* The absolute path of the mod folder must be **ASCII and shorter than 256 bytes** (the engine's path buffer);
  otherwise that file is not served (ERROR in the log).
* Changing files requires restarting the game.
* Log: `mods: data/... served from my_mod (...)` once per file per session, the first time the game opens it.

### 6.2 `lua\<script>\*.lua` - Lua patches

`<script>` is the retail script name without `.lua.bin`, with or without its trailing version (`title_menu_2` or
`title_menu_2_7.01.12.00`); `lua\_all\` runs for every script. Files in a folder run in lexicographic order (use `10_`,
`20_` prefixes). A patch runs inside the script's own Lua VM immediately after the script's main chunk, every time the game
loads the script (so while developing you can edit the file and reopen the menu, no restart).

Order of execution after a script's main chunk: `evt_loader\lua_patches\` first (only when `[modules] lua_patch = true`),
then every active mod in load order.

* The global `EVT_PATCH = { script = "...", file = "mods/<id>/<folder>/<file>" }` is set before each file runs.
* Only the script's **globals** are reachable (retail bytecode is stripped of local names). To change a callback, wrap the
  global: `local orig = OnDecideFocus; function OnDecideFocus(...) ... end`.
* **Never call `funcLuaCommand` / `funcLuaMenuCommand` at the top level** of a patch: they do not exist while the chunk
  loads. Call them from inside the game's callbacks (`OnInit`, `Step`, ...).
* Libraries available: `base`, `coroutine`, `table`, `string`, `bit32`, `math` (no `io`, `os`, `debug`, `require`).
* `lua\_fingerprints.json` maps a folder name to the globals that identify the script in a running VM (so a patch never
  runs in the wrong menu when several load at once). Fingerprints of a mod are merged after the global ones. Format: see
  `examples/example_lua_only` (its `_fingerprints.json` and README).
* Errors in a patch are logged (`lua_patch: <script>: <file>: runtime error: ...`) and never break the script.

Working example: `examples/example_lua_only`.

### 6.3 `data\<table>.toml` - data deltas

```toml
[[set]]
table = "character/chara_param"
key = "pc_para_c01000010"
column = "skill_1"            # column name from the schema, or a numeric index
value = 1234

[[add]]
table = "character/chara_param"
key = "pc_para_c09000010"
from = "pc_para_c01000010"    # optional: clone this row
values = { skill_1 = 5, skill_2 = 7 }
```

`evt-mod check` validates the table and columns against the table index. At boot the ModLoader rebuilds every game table
file an active mod touches **once**, with the cells of every mod (all `[[add]]` in load order, then all `[[set]]`; the mod
that loads later wins a cell both change, with a warning), caches the result in `evt_loader\cache\merged\` and serves it
through the overlay like a whole-file override. Only T2B tables that live in one file are supported (per-map / per-language
tables and RDBN tables are refused with a warning).

### 6.4 `preview.png`

Next to `mod.toml`, any size (cropped to 16:9). The mod manager converts it to the texture the in-game Mods menu shows; without it the
menu shows a generic picture.

## 7. Configuration

`<game>\evt_loader\config.toml` configures the loader itself (`[modules]`, `[loader] log_level`, `[console]`, ...). A mod's
own settings are merged **key by key** (tables deep-merge), weakest first, and handed to the plugin as one TOML text
(`config_get`):

1. `<mod>\config.toml` - defaults the mod ships with (optional);
2. `[<id>]` in `evt_loader\config.toml` - legacy section of a built-in module that became a plugin;
3. `[mods.<id>]` in `evt_loader\config.toml` - **the user's own settings**. They survive mod updates, which may replace the
   mod's whole folder.

Recommendation: defaults in code, `config.toml` in the mod only as an example, and document `[mods.<id>]` in your README.
The loader ignores sections it does not know, so `[mods.x]` never bothers it. An invalid mod `config.toml` is skipped with a warning.

## 8. Plugins (native code)

A plugin is a Windows **x64 DLL** in the mod folder, named by `plugin = "..."`. It gets the whole game process: it can read
and write memory, hook functions, and register Lua commands. Exports (C ABI, Microsoft x64 convention):

| Export | Signature | |
|---|---|---|
| `evt_plugin_api_version` | `uint32_t (void)` | **required.** The API version the plugin was built for (`EVT_PLUGIN_API_VERSION`, 1). |
| `evt_plugin_init` | `int32_t (const EvtApi*, const EvtPluginInfo*)` | **required.** `0` = OK, anything else = this plugin is disabled. |
| `evt_plugin_early` | same | optional: the early phase (below). |
| `evt_plugin_shutdown` | `void (void)` | optional: called when `init` (or `early`) failed, for cleanup. Unload / hot reload is not implemented; the DLL stays loaded. |

`EvtPluginInfo` (48 bytes): `size`, `load_index` (position in the mod load order), `handle` (the opaque `EvtPlugin*` you pass as
first argument to the per-plugin functions), `mod_id`, `mod_version`, `mod_dir` (UTF-8 absolute path), `loader_version`.
The strings belong to the loader and are valid forever.

**Language support.** Rust: use `crates/evt-plugin-sdk` (`declare_plugin!`, safe `Host` wrapper, `LuaCall`, `evt_info!` etc.
macros; panics are caught at the plugin boundary). C / C++: `sdk/evt_plugin.h`. Any language that can produce a C-ABI x64 DLL works.

### 8.1 Phases

Plugins load in mod load order (dependencies first), and each phase runs plugin by plugin in that order.

| Phase | Export | Thread and moment | For |
|---|---|---|---|
| **early** | `evt_plugin_early` | The game's **main thread**, at the executable's CRT entry point: loader lock released, before any game code and before C++ static initialisers. The game's globals do not exist yet; the log is buffered in memory until the file opens; do not wait for other threads. `sig_find`, hooks, IAT patches, `config_get`, `mod_*`, `path_get` work. | Hooks that must exist before the game starts (IAT of Steam calls, file loaders...). |
| **init** | `evt_plugin_init` | The loader's **init thread**, after the `nie.exe` v7.1.2 SHA-1 check and before the Lua commands are published. | Everything else: hooks, Lua command registration, starting threads. |

If the early phase cannot be armed (unexpected entry-point shape) or does not run within 2 s, the loader falls back to the init
thread and calls `evt_plugin_early` *late* (warning in the log). A plugin that fails in `early` is disabled and its `init` is
never called. Which path ran is in the log: `plugins: path: early phase at the entry point ...` or `FALLBACK`.

Requirements for plugins to run: `[modules] mods = true` and `nie.exe` v7.1.2.

### 8.2 Failure isolation

A missing DLL, a failing `LoadLibrary`, missing exports, an API version newer than the loader's, an `init`/`early` that returns
non-zero or raises an exception (SEH-guarded) - the plugin is **disabled and the game continues**: its hooks leave the chains,
its IAT / pointer patches are restored (if the slot still points at its detour), its pending Lua commands are dropped and
`evt_plugin_shutdown` is called. The mod's Lua patches and files stay active; `EvtModInfo.plugin_state` becomes `EVT_PLUGIN_FAILED`.

```
ERROR plugins: x@1.0 (x.dll): <reason>: plugin DISABLED, the game continues without it (removed N hook(s), ...)
```

The DLL is never unloaded (it may have started threads). Do not start any work in `init` until you know it will return 0.

### 8.3 Log lines

`log(level, msg)` writes to `loader.log` with the prefix `<mod id>: `. Levels: 0 error, 1 warn, 2 info, 3 debug, 4 trace
(`[loader] log_level` decides what reaches the file). At every start the loader logs the plugin list and
`WARN plugins: a plugin is native code with full access to the game and the PC: install only mods you trust`.

## 9. Plugin API v1 reference

`const EvtApi *api` is a table of function pointers (v1 = **248 bytes**; offsets are pinned by a test in the Rust SDK and by the
`_Static_assert`s in `examples/example_c_plugin/plugin.c`). It starts with `api_version`, `size` (= `sizeof(EvtApi)` *of the
loader*) and `loader_version`. **Check `api->api_version >= N` and `api->size >= sizeof(EvtApi)` before using the table, and never
read a field at an offset >= `api->size`.**

**Results** (`int32_t`): `EVT_OK 0`, `EVT_E_ARG -1` (bad argument: NULL, bad UTF-8/pattern, too many args), `EVT_E_NOT_FOUND -2`
(signature / import / mod / key not found), `EVT_E_STATE -3` (wrong moment, or the needed loader module is off), `EVT_E_FAULT -4` (an
exception in a memory access or guarded call), `EVT_E_CONFLICT -5` (already registered / already hooked with this cell), `EVT_E_HOOK -6`
(hook could not be installed). Details always go to `loader.log` with your prefix.

**Threads.** Every function is safe to call from any thread. Rules that matter: Lua handlers run on the **game's Lua thread**
(keep them short); detours run on whichever thread the game calls the hooked function from; game code is generally *not*
thread-safe, so call it (`call_guarded`) only from contexts where the game itself would. `lua_register*` only works during init.

Signatures below are C (`evt_plugin.h`); the Rust `Host` method is in parentheses.

### Identity, log, config, paths

| Function | Notes |
|---|---|
| `void log(h, int32_t level, const char *msg)` (`Host::log`, `evt_info!` ...) | Any thread, any phase (buffered in the early phase). NULL `msg` = empty line; an unknown level counts as info. |
| `void log_flush(void)` (`flush`) | Writes the queued lines now. Call before a hard exit (`TerminateProcess`). |
| `size_t config_get(h, char *buf, size_t cap)` (`config_text`) | The merged TOML text ([7](#7-configuration)). Copies at most `cap-1` bytes + NUL and **returns the full length**; call with `buf = NULL, cap = 0` to get the size first. Empty (0) when there is no config. It is re-serialised TOML, not the raw file. Available in both phases. |
| `size_t path_get(const char *key, char *buf, size_t cap)` (`path`) | `"game_dir"`, `"loader_dir"` (`evt_loader`), `"mods_dir"`; UTF-8, same buffer convention as `config_get`; returns 0 for an unknown key. |
| `uintptr_t exe_base(void)` (`exe_base`) | Base address of `nie.exe`. RVA = address - base. |

### Finding code

| Function | Notes |
|---|---|
| `int32_t sig_find(h, const char *name, const char *pattern, uint32_t expected_rva, uintptr_t *out)` (`sig`) | Finds a **unique** match of an IDA-style pattern (`48 89 5C 24 10 ?? ?? 48 8B`; `??` or `?` wildcards; at least one fixed byte) in a copy of `nie.exe`'s `.text` that the loader took in `DllMain` **before any patch**, so a prologue already hooked by another mod does not hide the pattern. Results are cached per pattern string. `name` and `expected_rva` are only for the log (`sig <name> -> RVA 0x...`, a WARN if the RVA differs from `expected_rva`; 0 = don't care). Errors: `E_ARG` (NULL / invalid pattern), `E_STATE` (`.text` information unavailable), `E_NOT_FOUND` (no match **or more than one**, logged). Make patterns long enough to be unique and wildcard everything relative (rel32 / RIP displacements). |
| `int32_t rip_target(uintptr_t insn, uint32_t disp_off, uint32_t next_ip_off, uintptr_t *out)` (`rip`) | Address a RIP-relative operand points to: `insn + next_ip_off + disp32`, where the disp32 is read at `insn + disp_off` (from the clean copy when inside `.text`). `next_ip_off` = length of the instruction. `E_ARG`, `E_FAULT`. |
| `int32_t code_read_clean(uintptr_t addr, void *dst, size_t n)` (`code_clean`) | Bytes of `.text` as they were before any patch. `E_NOT_FOUND` outside `.text`. |

### Memory and calls

| Function | Notes |
|---|---|
| `int32_t mem_read(uintptr_t addr, void *dst, size_t n)` (`read::<T>`, `read_ptr`) | SEH-guarded copy. `E_FAULT` for an unreadable address or one below 0x10000, `E_ARG` for NULL `dst`. Always use it (never dereference game pointers directly) unless you are sure. |
| `int32_t mem_write(uintptr_t addr, const void *src, size_t n)` (`write::<T>`) | SEH-guarded write of **data**. Not for code: patch code only through the hook functions. |
| `int32_t call_guarded(uintptr_t fn, const uint64_t *args, uint32_t nargs, uint64_t *ret)` (`call`) | Calls a game function with 0-6 integer / pointer arguments (Microsoft x64: rcx, rdx, r8, r9 then stack) under SEH; `*ret` = rax. No float arguments. `E_ARG` (fn < 0x10000, `nargs > 6`, NULL args), `E_FAULT` (exception inside). |

### Hooks

| Function | Notes |
|---|---|
| `int32_t hook_inline(h, uintptr_t target, const uint8_t *prologue, size_t len, const void *detour, uintptr_t *next, int32_t priority)` (`hook_inline`, unsafe) | **Chained** inline hook, see [10](#10-chained-hooks). `E_ARG`, `E_CONFLICT` (this `next` cell or detour is already in the chain), `E_HOOK` (prologue mismatch, too short, target not patchable). |
| `int32_t hook_iat(h, const char *module, const char *dll, const char *func, const void *detour, uintptr_t *orig)` (`hook_iat`, unsafe) | Patches the import-table slot of `dll!func` in `module` (NULL = `nie.exe`, else a loaded module's name). `*orig` receives the previous slot value (call it to continue). Several patches of the same slot stack: the last one installed runs first. Restored automatically if your init fails (only if the slot still holds your detour). `E_NOT_FOUND` (module not loaded / function not imported by name), `E_HOOK`. |
| `int32_t hook_ptr(h, uintptr_t slot, const void *detour, uintptr_t *orig)` (`hook_ptr`, unsafe) | Same for any pointer-sized cell the game calls through (a vtable entry). `E_FAULT` unreadable slot, `E_HOOK` not writable. Stacks like the IAT and is undone if your init fails. |

### Lua commands

A plugin can add engine commands that Lua scripts call with `funcLuaCommand(hash, args...)`, exactly like retail commands.

| Function | Notes |
|---|---|
| `int32_t lua_register(h, const char *name, EvtLuaHandler fn, void *user)` (`lua_register`) | Registers a command; its hash is `crc32(name)` like every engine command. **Only during `evt_plugin_init`** (before the loader publishes the commands) and with `[modules] lua_bridge = true`: otherwise `E_STATE`. A name/hash already taken by the loader or another plugin: `E_CONFLICT`. Convention: `CMND_EVT_<MOD>_<WHAT>`. |
| `int32_t lua_register_hash(h, uint32_t hash, const char *label, EvtLuaHandler fn, void *user)` | Same with an explicit hash; `label` is only for the log. |
| `void (*EvtLuaHandler)(EvtLuaCall *call, void *user)` | Runs on the game's Lua thread each time a script calls the command. Keep it short; do not call blocking code. |
| `int32_t lua_nargs(c)` | Number of arguments (not counting the hash). |
| `int32_t lua_arg_type(c, int32_t i)` | `i` is 0-based (0 = first argument after the hash). `EVT_LUA_NONE -1` (no such argument), `EVT_LUA_NIL 0`, `EVT_LUA_BOOLEAN 1`, `EVT_LUA_NUMBER 3`, `EVT_LUA_STRING 4` (Lua 5.2 type codes; no tables / other types in v1). |
| `int32_t lua_arg_num(c, i, double *out)` | `EVT_OK`, `E_NOT_FOUND` if the argument is missing / not a number, `E_ARG` for NULL `out`. |
| `ptrdiff_t lua_arg_str(c, i, char *buf, size_t cap)` | Copies at most `cap-1` bytes + NUL, returns the **full length**, `-1` if not a string. |
| `void lua_push_num(c, double)`, `lua_push_bool(c, int32_t)`, `lua_push_str(c, const char*)` | Push return values, in order; **the values you push are the values `funcLuaCommand` returns** (number and strings only; no `nil`, tables or 64-bit integers in v1). |

`EvtLuaCall` is valid only during the handler.

### Mods and game state

| Function | Notes |
|---|---|
| `uint32_t mod_count(void)`, `int32_t mod_get(uint32_t index, EvtModInfo *out)`, `int32_t mod_find(const char *id, EvtModInfo *out)` (`mods`, `find_mod`) | The active mods in load order. `EvtModInfo` (40 bytes): `id`, `version`, `dir`, `plugin` ("" = none), `load_index`, `plugin_state` (`EVT_PLUGIN_NONE 0`, `LOADED 1`, `FAILED 2`, `PENDING 3` = later in the order and not loaded yet). Strings are valid forever. `E_NOT_FOUND`. |
| `int32_t provider_find(const char *name, EvtModInfo *out, const char **version)` (`provider`) | The mod that provides `name` (its id or a `provides` entry) and the provided version; if several provide it, the one loaded last. `E_NOT_FOUND`. |
| `int32_t game_state(const char *key, int64_t *out)` (`state`) | `"match.soccer_mode"` (1/0; `E_NOT_FOUND` until known), `"match.in_match"` (1/0), `"loader.modules_mask"` (bit mask of the built-in modules that are on). Unknown key: `E_NOT_FOUND`. More keys will be added. |
| `int32_t thread_spawn(h, const char *name, EvtThreadFn fn, void *user)` (`spawn`) | Starts a thread named `plugin-<mod id>-<name>` running `fn(user)`. `E_STATE` if it cannot be created. The Rust wrapper catches panics. |

## 10. Chained hooks

Several plugins - and the loader's own modules - can hook **the same function**. The loader patches the function once; each
hook is a link in a chain.

* `hook_inline(h, target, prologue, len, detour, &next, priority)`: `target` is the function address (from `sig_find`),
  `prologue` the target's real first `len` bytes (**`len >= 14`**, whole instructions, **no relative operands** such as RIP-relative
  loads, `call rel32`, `jcc`: take them from the fixed prefix of your signature; the loader compares them with the live bytes
  and refuses a mismatch). If the target is already hooked by someone else, `prologue` is ignored and you just join the chain (still pass the right one).
* `detour` must have **exactly the signature and calling convention of the hooked function** and always continues through
  `next`: `((Fn)next)(args...)` to continue (run the rest of the chain, then the original) or *not* calling it to **replace**
  the function (everything after you in the chain is skipped, including the original). Call `next` on every path unless
  you mean to replace.
* `next` is a variable of yours (`static` storage, 8-byte aligned, alive forever, never on the stack). The loader **rewrites it
  whenever the chain changes** (another plugin hooking, a plugin failing and being removed), with atomic 8-byte writes from the
  inside out; read it with one aligned load each call. Never cache its value.
* **Order.** Runs first = outermost. Key `(priority, rank)` descending: higher `priority` first; at equal priority the mod that
  **loaded later** runs first (consistent with "the later mod wins"). The loader's built-in hooks sit at `(0, 0)`: by default plugins
  wrap them; `priority < 0` puts you inside the built-ins, closer to the original. Use `priority = 0` unless you have a reason.
* The same `next` cell or the same detour twice in one chain is refused (`E_CONFLICT`: it would loop).
* A failing plugin is removed from all chains at once; the patched code stays (with no hooks left it jumps straight to the original).
* Log: `<mod>: hook RVA 0x... installed (priority 0; chain, first runs first: mod_b > mod_a > <loader>)`.
* Limits: no hooks in the middle of a region another hook already stole; loader modules that copy instructions with relative operands by hand cannot join *after* a plugin on the same address (they fail and switch themselves off with their usual message).

## 11. Console and logs

`[modules] console = true` opens a separate **"ModLoader"** console window (behind the game) that shows `loader.log` live with
colours, plus a command line. Closing the window never closes the game (`block_close`; use the `close` command).

```toml
[modules]
console = true
[console]
categories = ["loader"]   # on at start-up
colors = true
input = true
block_close = true
```

| Command | What |
|---|---|
| `help` / `help <command>` | list / details |
| `log` / `log <category> on\|off` / `log level <error\|warn\|info\|debug\|trace>` | categories and the running log level |
| `config keys` / `config get <section.key>` / `config set <section.key> <value>` | inspect / change the few settings that can change at run time (not saved) |
| `status` | version, active modules, active mods and plugins, match state, categories, installed hooks |
| `lua reload` | explains that patches are read from disk on every script load (nothing to reload) |
| `quit` | asks the game to quit with its own request (`g_quitRequest`) |
| `close` | closes the console only |
| `clear` | clears the screen |

Optional categories add game events to the log (`states`, `files`, `menus`, `sound`, `match`; only `loader` is on by
default). Command and category names are English only (`log states on`, `log all off`, `close`); the Spanish names of the
first versions (`estados`, `ficheros`, `sonido`, `partido`, `todas`, `cerrar`) still work as hidden aliases for now, also in
`[console] categories`.
**Console commands and categories for plugins are not part of API v1 yet** (they will be appended to the table, see [15](#15-not-there-yet)).

When something does not work, in this order: `loader.log` (the mod list `mods: N enabled`, `skipped`/`conflict` lines, `plugins:`
lines, `lua_patch:` lines), then the console `status`, then `evt-mod check <game>\mods`.

## 12. Tools

`evt-mod` (source: `crates/evt-mod`, run from the repository with `cargo run -p evt-mod -- ...`):

```
evt-mod new <id> [<parent folder>]      creates a skeleton (mod.toml, lua\, files\data\, data\)
evt-mod check <mod folder | mods>       validates, prints load order and conflicts (exit code 1 on errors)
evt-mod list <game | mods>              the load plan of <game>\mods (active, skipped, conflicts)
evt-mod pack <mod folder> <out folder>  validates and copies the clean mod to <out>\<id>\ (zip it yourself)
```

`check` reports a missing plugin DLL, unknown keys, conflicting requirements, invalid delta tables, and so on.
`VR-ModLoader.exe` ([2.1](#21-the-mod-manager-vr-modloaderexe)) installs / removes individual mods and the ModLoader.

## 13. Safety

A **plugin is native code** running inside `nie.exe` with the player's permissions: it can do anything a program can do. Only
install plugin mods you trust; publish source or hashes if you distribute one. The ModLoader logs every plugin at each start
and, on failure, keeps the game running without that plugin; it cannot sandbox one. Mods without a plugin (files, Lua) cannot run
native code, but a Lua patch can call every engine command the game itself can.

## 14. Versioning and compatibility policy

* **Two independent versions.** `MODLOADER_VERSION` (semantic, `1.0.0` now; what `loader_min` compares, what plugins get
  as `loader_version`, and what the console `status` shows) and the **plugin API version** (`EVT_PLUGIN_API_VERSION`, `1`, what
  `evt_plugin_api_version` returns and `EvtApi.api_version` carries).
* **The API is append-only.** A new API version only **adds** fields at the end of `EvtApi` (and of `EvtPluginInfo`, `EvtModInfo`,
  each with its own `size` / by the same rule). Existing fields never move, disappear or change type or meaning. `EvtApi.size` is
  the size the *loader* has. New `game_state` keys and new `path_get` keys are additions too.
* **A plugin built for API vN loads on every ModLoader with `api_version >= N`.** A loader whose API is older than the plugin's
  refuses it: `needs plugin API vN, this ModLoader has vM (update the ModLoader)`. To use a field added in a later version while
  still supporting older loaders, check first that `api->size >= offsetof(EvtApi, field) + sizeof(field)`.
* Use `loader_min` in `mod.toml` so players with an older loader see a clear reason instead of a broken mod.
* The game side is fixed (nie.exe v7.1.2): signatures and RVAs will not change under you. Prefer `sig_find` patterns over
  hard-coded addresses anyway, so a mistake is a log line and not a crash.
* Breaking the ABI would mean a new major API generation with a new entry point; none is planned. *(This policy is the proposal for
  the public release; the numbering of the SDK crate follows the API version: `1.x` targets API v1.)*

## 15. Not there yet

Planned, not in API v1 / the current loader:

* `data\*.toml` deltas for RDBN tables and for tables split over several files (per map / per language).
* Console commands and log categories registered by plugins (`console_register`, `category_register`, `category_log`: new fields appended to `EvtApi`).
* Before/after filters on retail Lua commands, more Lua value types (integers, `nil`, tables), silent commands, core events (`game_reset`, match start / end), a file-overlay query API, executable-memory helpers, shared game-state readers (scene, actors, ball).
* Unloading / hot reload of plugins (`evt_plugin_shutdown` is only called after a failed init today).
* Extension points: a mod registers a content type and the loader hands it the files of the mods that depend on it.
* An SDK release with a prebuilt `evt-mod`; public English documentation of the game formats and the documented game
  function signatures (today they live in the source: `crates/*/src/**/sigs.rs`, the format crates `l5-core`, `l5-cpk`,
  `cri`, `g4-texture`).
