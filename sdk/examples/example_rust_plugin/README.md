# example_rust_plugin - a native plugin in Rust

A mod with its own DLL, built on the `evt-plugin-sdk` crate (`crates/evt-plugin-sdk`). It does four things and changes
nothing in the game:

| What | Where in `src/lib.rs` |
|---|---|
| logs to `evt_loader\loader.log` (prefix `example_rust_plugin: `) | `early`, `init`, `open_menu_detour` |
| reads its configuration (`config.toml` of the mod + `[mods.example_rust_plugin]` overrides) | `Config::parse` |
| registers the Lua command `CMND_EVT_EXAMPLE_MENU_COUNT([reset])` -> `count, greeting` | `menu_count_cmd` |
| installs a **chained inline hook** on `CMenuController::OpenMenu` (it counts menus and always calls on) | `install_hook`, `open_menu_detour` |

`OpenMenu` (nie.exe v7.1.2, RVA `0x10DEC90`, `bool OpenMenu(this, const u32 *nameHash, const OpenMenuParam *p)`) is
called for every menu the game opens, so it is a harmless, human-visible place to hook (the same signature the
ModLoader's console uses: `crates/vr-loader/src/console/sigs.rs`).

```
example_rust_plugin\
    Cargo.toml          cdylib, [lib] name = "example_rust_plugin"; its own [workspace] (see below)
    src\lib.rs          the plugin
    mod\mod.toml        the manifest: plugin = "example_rust_plugin.dll", loader_min, loader_modules = ["lua_bridge"]
    mod\config.toml     default configuration shipped with the mod
    build.ps1           cargo build --release + assembles dist\example_rust_plugin\ (the installable mod folder)
```

## Build

Requirements: Rust (stable, MSVC toolchain, `x86_64-pc-windows-msvc`).

```powershell
cd sdk\examples\example_rust_plugin
powershell -ExecutionPolicy Bypass -File build.ps1
# or by hand:  cargo build --release   ->   target\release\example_rust_plugin.dll
```

`build.ps1` leaves the ready-to-install folder in `dist\example_rust_plugin\` (`mod.toml`, `config.toml`, the DLL).
Set `$env:CARGO_TARGET_DIR` first if you want the build output somewhere else.

The example is **not** part of the main repository workspace: `Cargo.toml` declares its own empty `[workspace]`, and the
root `Cargo.toml` only lists the crates under `crates/`, so `cargo build` at the repository root never sees it.
In a mod of your own, point the `evt-plugin-sdk` dependency at wherever you keep the SDK (a path, a git URL, or the
registry once it is published) and keep `crate-type = ["cdylib"]` and `[lib] name = "<dll name>"`.

## Install and try

1. Copy `dist\example_rust_plugin\` to `<game>\mods\example_rust_plugin\`.
2. `<game>\evt_loader\config.toml`:

   ```toml
   [modules]
   mods = true                         # the mod system (plugins need it)
   # optional user overrides for this plugin (survive mod updates):
   [mods.example_rust_plugin]
   log_first_menus = 20
   ```
3. Start the game (Inazuma Eleven Victory Road PC **v7.1.2**). `loader.log` should contain, in this order:

   ```
   INFO  plugins: 1 plugin(s) in load order: example_rust_plugin@1.0.0 (example_rust_plugin.dll)
   WARN  plugins: a plugin is native code with full access to the game and the PC: install only mods you trust
   INFO  plugins: early phase armed (...)
   INFO  example_rust_plugin: early phase on the game's main thread (loader 1.0.0)
   ...
   INFO  example_rust_plugin: sig OpenMenu -> RVA 0x10DEC90
   INFO  example_rust_plugin: hook RVA 0x10DEC90 installed (priority 0; chain, first runs first: example_rust_plugin; a `> <loader>` follows when a built-in module, e.g. the console, hooks it too)
   INFO  example_rust_plugin: OpenMenu hooked (chained, priority 0)
   INFO  plugins: example_rust_plugin@1.0.0 (example_rust_plugin.dll): init OK in <n> ms
   INFO  example_rust_plugin: menu #1 opened (crc32 of its name 0x........)
   ```

   (The exact lines were written from the loader's source; the plugin itself is verified to build and to export the
   three entry points, but has not been run inside the game yet.)

## Call the Lua command

From a Lua patch (`lua\<script>\*.lua`, see `example_lua_only`), **inside a game callback, never at the top level of
the file** (`funcLuaCommand` does not exist yet while the script's chunk loads):

```lua
-- 615781408 = crc32("CMND_EVT_EXAMPLE_MENU_COUNT")
local count, greeting = funcLuaCommand(615781408)
print(greeting, count)            -- print needs [modules] debug = true to reach loader.log
local n2 = funcLuaCommand(615781408, 1)   -- argument 1 = reset the counter, returns the old count
```

Command names are hashed with CRC-32 (like every engine command); by convention plugin commands are called
`CMND_EVT_<MOD>_<WHAT>` so they cannot collide with retail commands or other mods (a duplicate name is refused with
`EVT_E_CONFLICT`).

## Things this example shows on purpose

* **Defaults live in the code**; `config.toml` is only an example and `[mods.<id>]` in the user's config wins.
* **Hook failure is not fatal here**: `init` logs a warning and keeps the Lua command. Returning `Err` from `init`
  instead disables the whole plugin (hooks removed, IAT patches restored, Lua commands dropped, game continues).
* **The detour never panics** and reads game memory only through `Host::read` (SEH-guarded).
* **`NEXT` is a `static AtomicUsize`**: the loader rewrites it whenever the chain of hooks on that function changes
  (another plugin hooking `OpenMenu` too, a plugin failing and being removed). The detour must always continue through
  it.
* **Early phase**: `declare_plugin!(init = init, early = early)` also exports `evt_plugin_early`, which runs on the game's
  main thread at the executable's entry point, before any game code. Use it only for hooks that must exist before the
  game starts; everything else belongs in `init`.

Full API reference: `sdk/README.md`.
