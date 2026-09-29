# example_c_plugin - a native plugin in C

The same minimal plugin as `example_rust_plugin`, written against the header `sdk/evt_plugin.h` only (no SDK crate, no
C runtime tricks): C11, MSVC, x64.

| What | Where in `plugin.c` |
|---|---|
| logs to `evt_loader\loader.log` (prefix `example_c_plugin: `) | `logf_` |
| reads its configuration (`log_first_menus` from the merged TOML text) | `read_config` |
| registers the Lua command `CMND_EVT_EXAMPLE_C_MENU_COUNT([reset])` -> `count` | `menu_count_cmd` |
| installs a **chained inline hook** on `CMenuController::OpenMenu` (counts menus, always calls on) | `evt_plugin_init`, `open_menu_detour` |

It also checks the API layout at compile time (`_Static_assert(sizeof(EvtApi) == 248, ...)`): if the header and the
loader ever disagree, the build breaks instead of the game crashing.

```
example_c_plugin\
    plugin.c            the plugin (exports evt_plugin_api_version, evt_plugin_early, evt_plugin_init)
    mod\mod.toml        plugin = "example_c_plugin.dll", loader_min = "1.0.0", loader_modules = ["lua_bridge"]
    mod\config.toml     default configuration
    build.bat           MSVC build + assembles dist\example_c_plugin\
    CMakeLists.txt      the same with CMake (not tested here; build.bat is)
```

## Build

Requirements: Visual Studio 2019+ Build Tools (C compiler, x64).

**build.bat** (open an *x64 Native Tools Command Prompt for VS*):

```bat
cd sdk\examples\example_c_plugin
build.bat
```

It runs `cl /std:c11 /O2 /W4 /LD /I ..\.. plugin.c` (the include path `..\..` is the folder holding `evt_plugin.h`) and
copies `mod\*` plus the DLL into `dist\example_c_plugin\`.

**CMake** (64-bit generator):

```bat
cmake -S . -B build -A x64
cmake --build build --config Release
:: the installable folder is build\dist\example_c_plugin\
```

The DLL uses the static C runtime (`cl` default `/MT`), so players need no Visual C++ redistributable.

## Install and try

Same as the Rust example: copy `dist\example_c_plugin\` to `<game>\mods\example_c_plugin\`, set `[modules] mods = true`
in `<game>\evt_loader\config.toml`, start the game (v7.1.2) and read `evt_loader\loader.log`. From Lua (inside a
callback, not at the top level of a patch):

```lua
-- 3488838267 = crc32("CMND_EVT_EXAMPLE_C_MENU_COUNT")
local count = funcLuaCommand(3488838267)
```

## Rules a C plugin must follow (all visible in `plugin.c`)

* Export `evt_plugin_api_version` and `evt_plugin_init` (and optionally `evt_plugin_early`, `evt_plugin_shutdown`) with
  C linkage and the Microsoft x64 convention (`EVT_PLUGIN_EXPORT` = `__declspec(dllexport)`).
* In `init`/`early`, check `api->api_version >= EVT_PLUGIN_API_VERSION` and `api->size >= sizeof(EvtApi)` before
  touching the table. Never read a table field at an offset `>= api->size` (newer loaders only ever append fields).
* Return 0 from `init` to stay enabled; any other value disables just this plugin. Don't start threads before you know
  you will return 0: the DLL is never unloaded.
* The cell you pass as `next` to `hook_inline` must live forever (static storage, 8-byte aligned); the detour reads it
  with one aligned load and calls it to continue.
* A crash inside a detour crashes the game: Rust plugins get panic-catching from the SDK, C plugins get nothing. Keep
  detours tiny and use `mem_read` for game pointers.

Full API reference: `sdk/README.md`.
