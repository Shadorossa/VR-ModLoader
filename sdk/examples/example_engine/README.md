# example_engine - the smallest VR-Framework engine

An **engine** takes over one system of the game: mods ship small, readable data files and the engine turns them into
what the game reads, merged across every active mod. This one is a toy that changes nothing in the game, so it can be
read end to end in `src/lib.rs` (about 100 lines of code):

1. at the **early phase** (exe entry point, before any game code) it finds the active mods that
   `requires = ["example_engine"]` (`discover::mod_uses_engine`);
2. loads their `example\*.toml` **strictly** (`data::load_dir`): every file starts with
   `schema = "example_engine.greetings/1"`, an unknown key is an error naming the file, the line and the key;
3. merges them (`layer::merge_by_key`: the mod that loads later wins, with a warning);
4. gives every new entry a **stable number** (`ids::IdRegistry`, kept in `evt_loader\ids\example_engine.json`);
5. writes ONE generated file to its **cache** (`evt_loader\cache\example_engine\data\common\evt_example\greetings.json`,
   rebuilt only when a data file or the engine version changes: `cache::InputHash` + a manifest) and **serves** it with
   `file_serve` (`host::EarlyMode`, `host::serve_files`);
6. answers the Lua command `CMND_EVT_EXAMPLE_GREETING("<mod>.<name>")` -> text, id (`lua::register_all`).

```
example_engine\
    Cargo.toml              cdylib example_engine.dll; deps evt-plugin-sdk + vr-framework; its own [workspace]
    src\lib.rs              the engine
    mod\mod.toml            plugin = "example_engine.dll", provides = ["example_engine"], loader_modules = ["lua_bridge"]
    mod\example\welcome.toml  the engine's own entry (example_engine.welcome)
    example_data_mod\       a data-only mod that uses the engine: requires = ["example_engine>=1.0"]
        example\greetings.toml  a new entry (example_engine_data.hello) + a replacement of example_engine.welcome
    build.ps1               cargo build --release + dist\example_engine\ and dist\example_engine_data\
```

## Data format

```toml
schema = "example_engine.greetings/1"   # first line of every data file

[[greeting]]
name = "hello"                          # own id: "<this mod>.hello" (lower-case English: a-z 0-9 _)
text = "Hello from a data mod"

[[greeting]]
name = "example_engine.welcome"         # the full id of another mod's entry: replaces its text
text = "Welcome, modder"
```

A typo such as `txet = "..."` on line 6 stops that file with this ERROR in `evt_loader\loader.log` (the other files
still load):

```
example_engine: mods/example_engine_data/example/greetings.toml:6: unknown key `txet` (expected `name` or `text`)
```

## Build and try

```powershell
cd sdk\examples\example_engine
powershell -ExecutionPolicy Bypass -File build.ps1
```

Copy `dist\example_engine\` and `dist\example_engine_data\` to `<game>\mods\` and start the game. `loader.log`:

```
example_engine: 2 greeting(s) from 2 mod(s), served as data/common/evt_example/greetings.json
```

From Lua (a mod's `lua\` patch): `local text, id = funcLuaCommand("CMND_EVT_EXAMPLE_GREETING", "example_engine_data.hello")`.

## Writing your own engine

Start from this folder: rename the crate, the `ENGINE` / `KIND` / `GAME_PATH` constants and the data types, and put the
real work in `build`. The framework guide is `crates/vr-framework/README.md`.
