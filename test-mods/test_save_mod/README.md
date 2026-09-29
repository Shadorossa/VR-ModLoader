# test_save_mod - save engine test

Test mod for `save_engine` (see [mods/save_engine/README.md](../../mods/save_engine/README.md)). Every time the title
menu (`title_menu_2`) is opened it adds one visit:

* `title_visits` in the **slot** data (written when the game saves that slot);
* `title_visits_total` in the **global** data (written immediately);
* plus `last_slot` (number) and `seen` (boolean, via `SET_BOOL`) in the slot.

Tested outside the game: `cargo test -p evt-plugin-save-engine` runs the `EvtSave` helper and this `10_counter.lua` in
a real Lua 5.2 VM against the store (`tests/lua_helper.rs`). **Not tested in the game.**

## Setup (does not touch save data)

1. ModLoader built from HEAD (`cargo build -p vr-loader --release`, install `winmm.dll` as usual). For the "events"
   mode (slot copies and deletions are followed) the ModLoader has to publish save events (`game_state("save.*")`, see
   the save_engine README); without them the plugin works in "state.json + Steam save file" mode (copies are not
   followed).
2. `cargo build -p evt-plugin-save-engine --release` -> copy `target\release\save_engine.dll` into `mods\save_engine\`
   of this repository and copy that folder to `<game>\mods\save_engine\`.
3. Copy `test-mods\test_save_mod\` to `<game>\mods\test_save_mod\`.
4. `evt_loader\config.toml`: `[modules] mods = true`, `lua_bridge = true`, `debug = true` (to see the `print` output).
5. **Before testing copies / deletions**: back up your saves (`<Steam>\userdata\<account>\2799860\remote\002AB8F4-USERDATALIVE*`).

Data folder (default): `%USERPROFILE%\AppData\LocalLow\LEVEL5 Inc_\INAZUMA ELEVEN Victory Road\modloader_saves\`.

## Steps in the game

| # | Do | Expected |
|---|---|---|
| 1 | Start the game | `loader.log`: `save_engine: on: data in ...modloader_saves (storage profile)` and `save_engine: slot source: ...` |
| 2 | Reach the title screen | `LUAPRINT ... [test_save_mod] slot N (writable ...): visits 1 (set true), total 1 (set true)`. Within 1 s `global\test_save_mod.json` appears with `title_visits_total: 1`. There is **no** `slotN\test_save_mod.json` yet |
| 3 | With slot 1 (locked) active | `writable false`; `slot1\` is never created |
| 4 | START -> slot 2 -> play and save (Save or autosave) | log `save_engine: game saved slot 2: wrote test_save_mod`; `slot2\test_save_mod.json` with `"bound": true` and `title_visits` = the visits counted in slot 2 |
| 5 | Go back to the title 2 times and close the game **without saving**; start it again | the 2 unsaved visits are lost: the counter continues from what was saved in step 4 (like the game's own progress). The global total does keep them |
| 6 | Switch to slot 3 on the slot screen | log `slot 2 -> 3` (and `changes not saved by the game dropped ...` if there were unsaved visits); slot 3's counter starts at 1 |
| 7 | (events mode) Delete slot 3 on the slot screen | log `slot 3 deleted: old data moved to ...\trash\..._deleted_slot3` |
| 8 | (events mode) Duplicate slot 2 into slot 3 | log `slot 2 copied into slot 3: 1 file(s) copied`; `slot3\test_save_mod.json` = what was saved for slot 2 |
| 9 | (optional) `[mods.save_engine] immediate_mods = ["test_save_mod"]` | `slotN\test_save_mod.json` is written at the title without waiting for a save (`"bound": false`) |

If the counter does not go up: look in the log for `lua_patch: title_menu_2...: ran N files [..., save_engine 1, test_save_mod 1]`
(the helper has to run first: `requires = ["save_engine"]` orders it) and for `[test_save_mod] EvtSave missing`.
