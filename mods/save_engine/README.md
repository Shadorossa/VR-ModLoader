# Save engine (`save_engine`)

Persistent data for mods, **kept apart from the game's save** and **tied to the player's save slot**.

A mod stores numbers, strings and booleans under its own keys. Slot data is saved **when the game saves** (like the
game's own progress), is dropped if the player loads another slot without saving, follows slot copies
and deletions reported by the ModLoader, and never touches the retail save file. Global data (not tied to a slot) is written at once.

Status: 1.0.0, built and unit-tested; **not yet tested in game**.

## Requirements

* ModLoader 1.0.0+ with the mod system on (`[modules] mods = true`) and `[modules] lua_bridge = true` (the Lua
  commands).
* Recommended: a ModLoader that publishes save events (`game_state("save.*")`). Without them the
  engine watches the game's save file in Steam's cloud folder instead (saves and removals of the active slot are seen;
  slot copies are not).

## Install

Copy this folder (with `save_engine.dll`) to `<game>\mods\save_engine\`. Mods that use it declare:

```toml
requires = ["save_engine"]
```

so they load after it (its Lua helper is then defined before their patches run).

## Where the data lives

| `[storage] location` | Folder |
|---|---|
| `"profile"` (default) | `%USERPROFILE%\AppData\LocalLow\LEVEL5 Inc_\INAZUMA ELEVEN Victory Road\modloader_saves\` (next to the game's own user data; survives a reinstall of the game) |
| `"game"` | `<game>\evt_loader\saves\` |
| `path = "D:\\anything"` | that folder |

```
modloader_saves\
    slot2\my_mod.json          slot data of mod my_mod for save slot 2
    global\my_mod.json         global data of my_mod
    trash\<stamp>_deleted_slot3\   data of a deleted / replaced slot (the newest 30 folders are kept)
```

The game's own saves are elsewhere: Steam's cloud folder
`<Steam>\userdata\<account>\2799860\remote\002AB8F4-USERDATALIVE[_<slot>]` (the save slots) and
`...\LocalLow\LEVEL5 Inc_\INAZUMA ELEVEN Victory Road\users\<id>\save\` (graphics settings). The engine never writes
there. Mod data is **not** synced by Steam Cloud: copy the folder yourself to move it to another PC.

A file looks like this (safe to read; edit only with the game closed):

```json
{
  "format": 1,
  "mod": "my_mod",
  "bound": true,
  "written_unix": 1759150000,
  "reason": "game save",
  "data": { "title_visits": 4, "seen_intro": true, "route": "raimon" }
}
```

Writes are atomic (`.tmp` + rename): a crash leaves the old or the new file, never a broken one. A file that does not
parse is kept as `<name>.json.bad-<stamp>` and the mod starts empty (logged).

## Lua: the `EvtSave` helper

The engine ships `lua\_all\00_save_engine.lua`, which defines the global `EvtSave` in every script. Open a handle **at
the top level** of your patch file (the helper reads your mod id from `EVT_PATCH`), use it **inside callbacks**:

```lua
-- mods\my_mod\lua\title_menu_2\10_visits.lua
local S = EvtSave and EvtSave.open()      -- top level: binds the handle to "my_mod"

local prev_OnInit = OnInit
function OnInit(...)
  if S then
    local n = S.get("title_visits", 0) + 1  -- (key, default)
    S.set("title_visits", n)                -- saved with the next game save of this slot
    S.set("seen_title", true)               -- booleans are fine
    S.set("old_key", nil)                   -- nil deletes (same as S.del("old_key"))
    S.gset("boots", S.gget("boots", 0) + 1) -- global: not tied to a slot, written at once
  end
  return prev_OnInit(...)
end
```

| Handle function | Does |
|---|---|
| `S.get(key [, default])` | slot value, or `default`, or `nil` |
| `S.set(key, value [, now])` | `value`: number, string, boolean, `nil` (= delete). `now = true`: written within ~0.25 s instead of at the next game save. Returns `true` / `false` |
| `S.del(key)` | `true` if the key existed |
| `S.gget` / `S.gset` / `S.gdel` | the same for global data (always written at once) |
| `S.commit()` | write this mod's slot data now (`false` on a locked slot) |
| `S.slot()` | `slot, writable`: the save slot the data follows (0 = not known yet) and whether it can be saved (the locked vanilla slot cannot) |

Tables: store them as strings you encode yourself (e.g. JSON text or `"a,b,c"`); the Lua sandbox has no JSON library.

Limits: key 1..128 bytes, string value 64 KiB, 4096 keys per mod and scope. A refused call returns `false` and logs one
warning.

## Lua: the raw commands

A Lua command cannot know which mod's script called it, so the **first argument is always your mod id**. Hashes =
crc32 of the names:

| Command | Hash | Arguments → results |
|---|---:|---|
| `CMND_EVT_SAVE_GET` | 3760054781 | `(mod, key [, default])` → value, or default, or nothing |
| `CMND_EVT_SAVE_SET` | 4214418001 | `(mod, key, value [, now])` → ok. `value` number / string; `nil` deletes; `now` = 1 writes it at once |
| `CMND_EVT_SAVE_SET_BOOL` | 1378346386 | `(mod, key, 1/0 [, now])` → ok (stores a boolean; plugin API v1 cannot read Lua booleans) |
| `CMND_EVT_SAVE_DEL` | 4046964722 | `(mod, key)` → existed |
| `CMND_EVT_SAVE_GLOBAL_GET` | 192964512 | as GET, global data |
| `CMND_EVT_SAVE_GLOBAL_SET` | 279931916 | as SET, global data |
| `CMND_EVT_SAVE_GLOBAL_SET_BOOL` | 316081803 | as SET_BOOL, global data |
| `CMND_EVT_SAVE_GLOBAL_DEL` | 447365551 | as DEL, global data |
| `CMND_EVT_SAVE_COMMIT` | 2951850645 | `(mod)` → ok: write the mod's slot data now |
| `CMND_EVT_SAVE_SLOT` | 74766845 | `()` → slot, writable |

```lua
local MOD = "my_mod"
local n = funcLuaCommand(3760054781, MOD, "title_visits", 0) + 1
funcLuaCommand(4214418001, MOD, "title_visits", n)
```

## When data is written

| Event | Slot data of the mods | Global data |
|---|---|---|
| a `set` | kept in memory (your `get` sees it at once) | written within ~0.25 s |
| the game saves the active slot (save / autosave) | **written** (`bound: true`) | — |
| the player switches to another slot (the START slot screen, or a mode that owns a slot) | unsaved changes **dropped**, the new slot's data is read | — |
| a slot is duplicated (slot screen) | the target gets a copy of the source's **saved** data | — |
| a slot is deleted / the game deletes its save / a new game starts in an empty slot | that slot's data goes to `trash\` | — |
| the game quits without saving | unsaved changes lost (like the game's own) | already written |
| the active slot is the locked vanilla slot | never written (the game's saves there are discarded too) | written |

`commit = "immediate"` (all mods) or `immediate_mods = ["my_mod"]` in `[mods.save_engine]` write slot data at once
instead of with the game's saves.

## Configuration

`<game>\evt_loader\config.toml`:

```toml
[mods.save_engine]
commit = "on_save"          # or "immediate"
immediate_mods = []         # mods written at once
flush_ms = 250              # worker period
trash_keep = 30             # trash folders kept

[mods.save_engine.storage]
location = "profile"        # or "game"
path = ""                   # absolute folder, overrides location
```

`[mods.save_engine.slots]` (count, lock, locked_slot, reserved, takeover) describes the save-slot layout the engine
will manage in a later version; in 1.0.0 the engine does not take the slots over and `takeover` must stay `false`.

## From a native plugin

`save_engine.dll` exports C functions (find the DLL with `provider("save_engine")` → its folder, then
`GetModuleHandleW` + `GetProcAddress`):

```c
uint32_t save_engine_api_version(void);                        // 1
intptr_t save_engine_get(const char *mod, int32_t global, const char *key, char *buf, size_t cap);  // JSON text; -1 none, -2 bad args
int32_t  save_engine_set(const char *mod, int32_t global, const char *key, const char *json);       // any JSON value; NULL = delete
int32_t  save_engine_commit(const char *mod);                   // 0 queued, -1 slot not writable
int32_t  save_engine_active_slot(void);                         // 0 = none
```

A shared service table in the ModLoader API (so plugins do not need `GetProcAddress`) is a possible future addition.

## Log lines

```
INFO  save_engine: on: data in C:\Users\...\modloader_saves (storage profile); slot data committed when the game saves ...
INFO  save_engine: slot source: ModLoader save events (save / switch / copy / delete / new game)
INFO  save_engine: game saved slot 2: wrote my_mod
INFO  save_engine: slot 2 -> 3: changes not saved by the game dropped for my_mod
INFO  save_engine: slot 3 deleted: old data moved to ...\trash\..._deleted_slot3
```

Test mod: `test-mods/test_save_mod/` (repository).
