//! The «Pack de voces» row Lua of the audio_engine mod (`mods/audio_engine/lua/setting_menu/110_voice_pack.lua`) in a
//! real Lua 5.2 VM on top of the **retail** `setting_menu_1.03.73.00.lua.bin` bytecode (from your own v7.1.2 dump in
//! `assets/v7.1.2`; the tests are skipped without it), like the ModLoader's lua_patch runner does: the retail main chunk
//! runs, then the patch file runs as a separate chunk in the same VM, then the engine callbacks are faked (list the
//! rows, left / right / A, the save check, «Sí» / «No»).

use mlua::{ChunkMode, Function, Lua, Table, Value};
use std::path::PathBuf;

fn repo() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("..").join("..").join("..")
}

const STUB: &str = r#"
CALLS = {}
TEXT = {}
FOCUS = 9
LOADER = true
VOICE_IDX = 0
PACKS = {{"Español", "es"}}
VOICE_SETS = {}
TEXTS = {[249340083] = "Pack de voces", [581565135] = "Ninguno", [2052939705] = "Necesita el VR ModLoader"}
local function any() return setmetatable({}, {__index = function() return function() end end}) end
MENU_DEF, LISTVIEW, MAIN_MENU, GENERAL_WINDOW, SAVEDATA_MANAGEMENT_MENU = any(), any(), any(), any(), any()
function INCLUDE() end
function waitFalse() end
function funcLuaMenuCommand(cmd, ...)
  local a = {...}
  if cmd == 1727549139 then return FOCUS end
  if cmd == 1083631230 and a[2] == 569341911 then TEXT[a[4]] = a[3] end
  return true
end
function funcLuaCommand(cmd, ...)
  local a = {...}
  CALLS[#CALLS + 1] = cmd
  if cmd == 2325395018 then            -- CMND_EVT_VOICE_GET
    if not LOADER then error("unknown command") end
    return VOICE_IDX, 1 + #PACKS
  end
  if cmd == 2738235108 then            -- CMND_EVT_VOICE_NAME
    if not LOADER then error("unknown command") end
    if a[1] == 0 then return "Ninguno", "" end
    local p = PACKS[a[1]]
    if p == nil then return "", "" end
    return p[1], p[2]
  end
  if cmd == 2444606950 then            -- CMND_EVT_VOICE_SET
    if not LOADER then error("unknown command") end
    VOICE_SETS[#VOICE_SETS + 1] = a[1]
    VOICE_IDX = a[1]
    return true
  end
  if cmd == 4074371074 then return TEXTS[a[1]] end                        -- CMND_TEXT_GET
  if cmd == 682363669 then return true end
  if cmd == 3284067519 then return 60 end                                 -- CMND_GET_TARGET_FPS (unchanged)
  return nil
end
"#;

/// The mod's Lua.
fn ours() -> PathBuf {
    repo().join("mods/audio_engine/lua/setting_menu/110_voice_pack.lua")
}

fn retail_chunk() -> Option<Vec<u8>> {
    std::fs::read(repo().join("assets/v7.1.2/data/common/script/lua/menu/setting_menu_1.03.73.00.lua.bin")).ok()
}

fn run_file(lua: &Lua, p: &PathBuf) {
    let src = std::fs::read_to_string(p).unwrap();
    lua.load(&src).set_name(p.file_name().unwrap().to_string_lossy()).exec().unwrap_or_else(|e| panic!("{}: {e}", p.display()));
}

/// None when the retail chunk is not there.
fn vm() -> Option<Lua> {
    let Some(chunk) = retail_chunk() else {
        eprintln!("SKIPPED: no retail setting_menu chunk in assets/v7.1.2");
        return None;
    };
    let lua = unsafe { Lua::unsafe_new() };
    lua.load(STUB).set_name("stub").exec().unwrap();
    lua.load(&chunk[..]).set_mode(ChunkMode::Binary).set_name("setting_menu_1.03.73.00").exec().unwrap();
    run_file(&lua, &ours());
    // the engine lists the rows: cell 3 = the retail «FPS máximos», 9 = ours
    {
        let uli: Function = lua.globals().get("UpdateListItem").unwrap();
        uli.call::<_, ()>((3, "FPS máximos")).unwrap();
        uli.call::<_, ()>((9, "Pack de voces")).unwrap();
    }
    Some(lua)
}

fn g(lua: &Lua) -> Table<'_> {
    lua.globals()
}

fn row(lua: &Lua, cell: i64, left: bool, right: bool) -> String {
    g(lua).get::<_, Function>("UpdateListContentDynamicText").unwrap().call::<_, ()>((cell, 40, left, right)).unwrap();
    g(lua).get::<_, Table>("TEXT").unwrap().get::<_, String>(cell).unwrap_or_default()
}

fn changed(lua: &Lua) -> bool {
    g(lua).get::<_, Function>("IsChangedGraphicSetting").unwrap().call(40).unwrap()
}

fn save(lua: &Lua, answer: i64) {
    // the engine runs the callback in a coroutine
    let f: Function = g(lua).get("callBackSaveCheckDialog").unwrap();
    let co = lua.create_thread(f).unwrap();
    co.resume::<_, mlua::MultiValue>(answer).unwrap();
}

fn sets(lua: &Lua, name: &str) -> Vec<i64> {
    let t: Table = g(lua).get(name).unwrap();
    t.sequence_values::<i64>().map(|v| v.unwrap()).collect()
}

fn enter(lua: &Lua) {
    g(lua).get::<_, Function>("OnEnter").unwrap().call::<_, ()>((2081113831u32, 0)).unwrap();
}

#[test]
fn retail_game_with_the_loader_only() {
    let Some(lua) = vm() else { return };
    assert!(g(&lua).get::<_, Table>("EVT_SET_ROWS").unwrap().contains_key("Pack de voces").unwrap());
    assert_eq!(row(&lua, 9, false, false), "Ninguno");
    assert!(!changed(&lua));
    assert_eq!(row(&lua, 9, false, true), "Español");
    assert!(changed(&lua), "a pack draft raises the retail save window");
    assert!(sets(&lua, "VOICE_SETS").is_empty(), "nothing stored before «Sí»");
    save(&lua, 0);
    assert_eq!(sets(&lua, "VOICE_SETS"), vec![1]);
    assert!(!changed(&lua));
    assert_eq!(row(&lua, 9, false, false), "Español");
    // «No» drops the draft
    assert_eq!(row(&lua, 9, true, false), "Ninguno");
    save(&lua, 1);
    assert_eq!(sets(&lua, "VOICE_SETS"), vec![1]);
    assert_eq!(row(&lua, 9, false, false), "Español");
    // A = right; the retail «FPS máximos» row stays retail (the FPS list is not ours)
    lua.load("PACKS = {{'Español','es'},{'Français','fr'}} FOCUS = 9").exec().unwrap();
    enter(&lua);
    assert_eq!(g(&lua).get::<_, Table>("TEXT").unwrap().get::<_, String>(9).unwrap(), "Français");
    assert!(g(&lua).get::<_, Function>("EVT_SET_Label").unwrap().call::<_, Value>(3).unwrap().is_nil());
}

#[test]
fn retail_game_other_language_and_loader_off() {
    let Some(lua) = vm() else { return };
    // English game: the engine lists the label of the English menu_text
    lua.load("TEXTS[249340083] = 'Voice pack' TEXTS[581565135] = 'None' AE_VP_LABEL = nil").exec().unwrap();
    g(&lua).get::<_, Function>("UpdateListItem").unwrap().call::<_, ()>((10, "Voice pack")).unwrap();
    assert_eq!(row(&lua, 10, false, false), "None");
    assert_eq!(row(&lua, 10, false, true), "Español");
    // texts missing (no text_engine and no served menu_text): English fallbacks, never an error
    let Some(lua) = vm() else { return };
    lua.load("TEXTS[581565135] = nil LOADER = false").exec().unwrap();
    assert_eq!(row(&lua, 9, false, false), "Necesita el VR ModLoader");
    assert_eq!(row(&lua, 9, false, true), "Necesita el VR ModLoader");
    assert!(!changed(&lua));
    lua.load("TEXTS[2052939705] = nil").exec().unwrap();
    assert_eq!(row(&lua, 9, false, false), "Needs the VR ModLoader");
    lua.load("LOADER = true").exec().unwrap();
    assert_eq!(row(&lua, 9, false, false), "None");
}
