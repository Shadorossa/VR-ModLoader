# vr-loader (VR-ModLoader core)

`winmm.dll` proxy mod loader for **Inazuma Eleven Victory Road PC v7.1.2** (`nie.exe` SHA-1
`d27e76217730783fec8df4a3b0541cb15fe8f100`). The DLL is placed next to `nie.exe` as `winmm.dll`; it forwards every
winmm export to the system DLL and, inside the game process only, loads the mods of `<game>\mods\`.

The public reference for mod authors (mod format, load order, plugin API, console) is [`sdk/README.md`](../../sdk/README.md).

## Build

```powershell
cargo build -p vr-loader --release              # target\release\vr_loader.dll -> copy as <game>\winmm.dll
cargo build -p vr-loader --profile loader-fast  # faster rebuilds while developing (no LTO)
powershell -ExecutionPolicy Bypass -File crates\vr-loader\build-winmm.ps1   # build + copy to target\release\winmm.dll
```

Needs the MSVC toolchain (the SEH shim `src/seh.c` is compiled with `cc`). The DLL links the dynamic VC runtime
(`VCRUNTIME140.dll`), which the game already requires.

## Tests

```powershell
cargo test -p vr-loader --release
```

Unit tests need nothing. The static checks against the game executable (`tests/*.rs`: every signature unique at its
expected RVA, hook prologues relocatable, command hashes) read **your own** extracted `nie.exe` v7.1.2 from
`<repo>\assets\v7.1.2\nie.exe` (or `$env:EVT_NIE_EXE`) and are skipped when it is absent. No game file is part of this
repository.

## Built-in modules (`[modules]` in `evt_loader\config.toml`)

| Module | Default | What |
|---|---|---|
| `mods` | on | mod folders `<game>\mods\<id>\`: load plan (dependencies, `provides`, `loader_min`), whole-file overlay without touching `cpk_list`, data deltas, voice packs, the in-game Mods menu commands, native plugins |
| `lua_bridge` | on | `CMND_EVT_*` Lua commands (mods' Lua, plugins' `lua_register`) |
| `lua_patch` | off | global Lua patch folder `evt_loader\lua_patches\` (mods' `lua\` folders use the same runner anyway) |
| `chara_legal` | on | every character is legal: characters added by mods get no yellow placeholder face |
| `quit_fix` | on | closing the window always ends `nie.exe`, also in a match; never during a save job |
| `debug` | off | `LUAERR` / `LUAPRINT` lines, local debug server, hang watchdog |
| `console` | off | console window mirroring `loader.log` with a command line and game event categories |

## Layout

| File | What |
|---|---|
| `exports.txt` + `build.rs` | the exports of the system `winmm.dll` (names + ordinals) -> asm stubs + `/EXPORT` args; the T2B table index of the data deltas (`table_index.tsv`: table layouts only) |
| `src/proxy.rs` | lazy forwarding to the real winmm (no work in DllMain) |
| `src/runtime.rs` | DllMain (quick gate, config, early hooks) + init thread (log, SHA-1 gate, plugins, modules) |
| `src/sigs.rs`, `src/scan.rs`, `src/game.rs`, `src/registry.rs` | signatures, unique-match scanner, resolution on the clean `.text` before any patch, SEH-guarded calls/reads/writes |
| `src/hook.rs`, `src/pe.rs` | chained inline hooks with verified stolen bytes, pointer / IAT patches, PE helpers |
| `src/lua.rs` | `lua.CommandDispatch` hook, command registry, argument / return helpers |
| `src/mods/` | mod plan, file overlay (`ResolveOverlayPath` hook), `cpk_list` records, data-delta merge, voice packs, Mods menu commands |
| `src/lua_patch/` | Lua patch runner (file open + chunk load hooks, fingerprints) |
| `src/plugins/` | native plugin host (API v1, `sdk/evt_plugin.h`) |
| `src/console/`, `src/debug/` | console window and taps; Lua error capture, debug server, hang watchdog |
| `src/quit_fix/`, `src/chara_legal/`, `src/match_state.rs` | built-in fixes and the «real match?» state for plugins |
| `src/config.rs`, `src/log.rs`, `src/gate.rs` | `evt_loader\config.toml`, `loader.log`, version gate |

Adding a command: `lua::register("CMND_EVT_<NAME>", handler)` during init; `handler(&mut Call)` reads arguments with
`c.num(i)` / `c.u32(i)` / `c.string(i)` and returns values with `c.push_*`.
