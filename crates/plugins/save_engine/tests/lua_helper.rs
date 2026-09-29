//! The mod's Lua side in a real Lua 5.2 VM: `mods/save_engine/lua/_all/00_save_engine.lua` (the EvtSave
//! helper) and `test-mods/test_save_mod/lua/title_menu_2/10_counter.lua`, with `funcLuaCommand` mocked by
//! the argument rules of the plugin's commands (rt.rs; plugin API v1: Lua booleans are unreadable) on top of the real
//! [`Store`] in a temp folder.

use mlua::{Lua, MultiValue, Value as LV};
use save_engine::store::{Scope, Store};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

fn crc(name: &str) -> u32 {
    // crc32 (IEEE), as the loader hashes command names
    let mut c = !0u32;
    for b in name.bytes() {
        c ^= b as u32;
        for _ in 0..8 {
            c = if c & 1 != 0 { (c >> 1) ^ 0xEDB8_8320 } else { c >> 1 };
        }
    }
    !c
}

fn repo() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("..").join("..").join("..")
}

fn to_json(v: &LV, as_bool: bool) -> Result<Option<serde_json::Value>, ()> {
    match v {
        LV::Integer(i) if as_bool => Ok(Some(serde_json::Value::Bool(*i != 0))),
        LV::Number(n) if as_bool => Ok(Some(serde_json::Value::Bool(*n != 0.0))),
        LV::Integer(i) => Ok(save_engine::num_value(*i as f64)),
        LV::Number(n) => Ok(save_engine::num_value(*n)),
        LV::String(s) if !as_bool => Ok(save_engine::str_value(&s.to_string_lossy())),
        LV::Nil => Ok(None),
        _ => Err(()), // booleans (unreadable in API v1), tables, ...
    }
}

fn to_lua<'l>(lua: &'l Lua, v: &serde_json::Value) -> mlua::Result<LV<'l>> {
    Ok(match v {
        serde_json::Value::Bool(b) => LV::Boolean(*b),
        serde_json::Value::Number(n) => LV::Number(n.as_f64().unwrap_or(0.0)),
        serde_json::Value::String(s) => LV::String(lua.create_string(s)?),
        other => LV::String(lua.create_string(other.to_string())?),
    })
}

/// A VM with `funcLuaCommand` routed to `store` (slot 2, writable).
fn vm(store: Arc<Mutex<Store>>) -> Lua {
    let lua = Lua::new();
    let hs: Vec<(u32, &str)> = [
        "CMND_EVT_SAVE_GET",
        "CMND_EVT_SAVE_SET",
        "CMND_EVT_SAVE_SET_BOOL",
        "CMND_EVT_SAVE_DEL",
        "CMND_EVT_SAVE_GLOBAL_GET",
        "CMND_EVT_SAVE_GLOBAL_SET",
        "CMND_EVT_SAVE_GLOBAL_SET_BOOL",
        "CMND_EVT_SAVE_GLOBAL_DEL",
        "CMND_EVT_SAVE_COMMIT",
        "CMND_EVT_SAVE_SLOT",
    ]
    .into_iter()
    .map(|n| (crc(n), n))
    .collect();
    let f = lua
        .create_function(move |lua, args: MultiValue| {
            let a: Vec<LV> = args.into_iter().collect();
            let h = match a.first() {
                Some(LV::Integer(i)) => *i as u32,
                Some(LV::Number(n)) => *n as i64 as u32,
                _ => return Ok(MultiValue::new()),
            };
            let name = hs.iter().find(|(x, _)| *x == h).map(|(_, n)| *n).unwrap_or("?");
            let arg = |i: usize| a.get(i + 1).cloned().unwrap_or(LV::Nil);
            let mut s = store.lock().unwrap();
            let scope = if name.contains("GLOBAL") { Scope::Global } else { Scope::Slot };
            let text = |v: LV| match v {
                LV::String(x) => Some(x.to_string_lossy().into_owned()),
                _ => None,
            };
            let out: Vec<LV> = match name {
                "CMND_EVT_SAVE_SLOT" => vec![LV::Number(s.slot() as f64), LV::Boolean(s.writable())],
                "CMND_EVT_SAVE_COMMIT" => vec![LV::Boolean(s.writable())],
                _ => {
                    let (Some(m), Some(k)) = (text(arg(0)).filter(|m| save_engine::valid_mod_id(m)), text(arg(1))) else {
                        return Ok(MultiValue::from_vec(vec![LV::Boolean(false)]));
                    };
                    if name.ends_with("_GET") {
                        match s.get(scope, &m, &k) {
                            Some(v) => vec![to_lua(lua, &v)?],
                            None => match arg(2) {
                                LV::Nil => vec![],
                                d => vec![d],
                            },
                        }
                    } else if name.ends_with("_DEL") {
                        vec![LV::Boolean(s.del(scope, &m, &k))]
                    } else {
                        match to_json(&arg(2), name.ends_with("_BOOL")) {
                            Ok(Some(v)) => vec![LV::Boolean(s.set(scope, &m, &k, v).is_ok())],
                            Ok(None) => {
                                s.del(scope, &m, &k);
                                vec![LV::Boolean(true)]
                            }
                            Err(()) => vec![LV::Boolean(false)],
                        }
                    }
                }
            };
            Ok(MultiValue::from_vec(out))
        })
        .unwrap();
    lua.globals().set("funcLuaCommand", f).unwrap();
    lua
}

/// Run a patch file as the loader does: EVT_PATCH set, then the chunk.
fn run_patch(lua: &Lua, rel: &str, patch_file: &str) {
    let src = std::fs::read_to_string(repo().join(rel)).unwrap();
    let t = lua.create_table().unwrap();
    t.set("file", patch_file).unwrap();
    lua.globals().set("EVT_PATCH", t).unwrap();
    lua.load(&src).set_name(rel).exec().unwrap();
}

fn fresh_store(name: &str) -> (Arc<Mutex<Store>>, PathBuf) {
    let d = std::env::temp_dir().join(format!("save-engine-lua-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    let mut s = Store::new(d.clone(), 5);
    s.set_slot(2, true);
    (Arc::new(Mutex::new(s)), d)
}

const HELPER: &str = "mods/save_engine/lua/_all/00_save_engine.lua";
const COUNTER: &str = "test-mods/test_save_mod/lua/title_menu_2/10_counter.lua";

#[test]
fn helper_commands_and_hashes() {
    let (store, dir) = fresh_store("helper");
    let lua = vm(store.clone());
    // the helper's hashes are the crc32 of the command names
    run_patch(&lua, HELPER, "mods/save_engine/_all/00_save_engine.lua");
    for (k, n) in [("GET", "CMND_EVT_SAVE_GET"), ("SET_BOOL", "CMND_EVT_SAVE_SET_BOOL"), ("GSET_BOOL", "CMND_EVT_SAVE_GLOBAL_SET_BOOL"), ("SLOT", "CMND_EVT_SAVE_SLOT")] {
        let v: f64 = lua.load(format!("return EvtSave.H.{k}")).eval().unwrap();
        assert_eq!(v as u32, crc(n), "{k}");
    }
    // a second run (another script VM loads _all again) keeps the table
    run_patch(&lua, HELPER, "mods/save_engine/_all/00_save_engine.lua");
    lua.load(
        r#"
        local S = EvtSave.open("my_mod")
        assert(S.get("k") == nil)
        assert(S.get("k", 5) == 5)
        assert(S.set("k", 7) == true)
        assert(S.get("k", 5) == 7)
        assert(S.set("flag", true) == true and S.get("flag") == true)
        assert(S.set("flag", false) == true and S.get("flag") == false)
        assert(S.set("name", "Raimon") and S.get("name") == "Raimon")
        assert(S.set("name", nil) and S.get("name") == nil)
        assert(S.del("k") == true and S.del("k") == false)
        assert(S.gset("g", 1) and S.gget("g") == 1 and S.get("g") == nil)
        local slot, w = S.slot()
        assert(slot == 2 and w == true)
        assert(S.commit() == true)
        assert(EvtSave.open(nil) == nil or true)
        "#,
    )
    .exec()
    .unwrap();
    // a raw call with a Lua boolean is refused (plugin API v1 cannot read it); SET_BOOL is the way
    let ok: bool = lua.load(r#"return funcLuaCommand(EvtSave.H.SET, "my_mod", "b", true)"#).eval().unwrap();
    assert!(!ok);
    // bad mod id refused
    let ok: bool = lua.load(r#"return funcLuaCommand(EvtSave.H.SET, "My Mod", "b", 1)"#).eval().unwrap();
    assert!(!ok);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn open_takes_the_mod_id_from_evt_patch() {
    let (store, dir) = fresh_store("open");
    let lua = vm(store.clone());
    run_patch(&lua, HELPER, "mods/save_engine/_all/00_save_engine.lua");
    let t = lua.create_table().unwrap();
    t.set("file", "mods/other_mod/title_menu_2/10_x.lua").unwrap();
    lua.globals().set("EVT_PATCH", t).unwrap();
    let id: String = lua.load("return EvtSave.open().id").eval().unwrap();
    assert_eq!(id, "other_mod");
    lua.globals().set("EVT_PATCH", LV::Nil).unwrap();
    let none: LV = lua.load("return EvtSave.open()").eval().unwrap();
    assert!(matches!(none, LV::Nil));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn test_mod_counts_title_visits_per_slot_and_in_total() {
    let (store, dir) = fresh_store("counter");
    // three title visits in slot 2 (each visit = the title script loads again in a new VM and OnInit runs)
    for visit in 1..=3 {
        let lua = vm(store.clone());
        lua.load("function OnInit() RETAIL_INIT = (RETAIL_INIT or 0) + 1 end").exec().unwrap();
        run_patch(&lua, HELPER, "mods/save_engine/_all/00_save_engine.lua");
        run_patch(&lua, COUNTER, "mods/test_save_mod/title_menu_2/10_counter.lua");
        lua.load("OnInit()").exec().unwrap();
        let retail: i64 = lua.load("return RETAIL_INIT").eval().unwrap();
        assert_eq!(retail, 1, "the retail OnInit still runs");
        let n = store.lock().unwrap().get(Scope::Slot, "test_save_mod", "title_visits");
        assert_eq!(n, Some(serde_json::json!(visit)));
    }
    let mut s = store.lock().unwrap();
    assert_eq!(s.get(Scope::Slot, "test_save_mod", "seen"), Some(serde_json::json!(true)));
    assert_eq!(s.get(Scope::Slot, "test_save_mod", "last_slot"), Some(serde_json::json!(2)));
    assert_eq!(s.get(Scope::Global, "test_save_mod", "title_visits_total"), Some(serde_json::json!(3)));
    // the game saves slot 2: the counter reaches the disk
    assert_eq!(s.commit_slot("game save").written, vec!["test_save_mod"]);
    let f: serde_json::Value = serde_json::from_slice(&std::fs::read(dir.join("slot2").join("test_save_mod.json")).unwrap()).unwrap();
    assert_eq!(f["data"]["title_visits"], serde_json::json!(3));
    // another slot starts from zero; the total goes on
    s.set_slot(3, true);
    assert_eq!(s.get(Scope::Slot, "test_save_mod", "title_visits"), None);
    drop(s);
    let _ = std::fs::remove_dir_all(&dir);
}
