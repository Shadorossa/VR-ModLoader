//! `example_engine`: the smallest VR-Framework engine. Every active mod that `requires = ["example_engine"]` ships
//! `<mod>\example\*.toml`; at the early phase the engine merges them into ONE generated file and serves it to the game
//! with `file_serve` (the game path is new, so the game itself never reads it: nothing changes in play). Lua reads
//! the result: `CMND_EVT_EXAMPLE_GREETING("<mod>.<name>")` → text, id.
//!
//! ```toml
//! schema = "example_engine.greetings/1"   # every data file names its schema
//! [[greeting]]
//! name = "hello"                          # own id: "<this mod>.hello"
//! text = "Hello from my mod"
//! [[greeting]]
//! name = "example_engine.welcome"         # a full id of another mod: replaces its text (the later mod wins)
//! text = "Welcome, modder"
//! ```

use evt_plugin_sdk::{declare_plugin, Host, Level, LuaCall, EVT_LUA_STRING};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::sync::OnceLock;
use vr_framework::host::{self as fw, EarlyMode};
use vr_framework::{cache, data, discover, fsx, ids, layer, lua, Notes};

const ENGINE: &str = "example_engine";
/// Bump when the same inputs give another output (invalidates the cache).
const VERSION: u32 = 1;
const KIND: &str = "example_engine.greetings";
const GAME_PATH: &str = "data/common/evt_example/greetings.json";

#[derive(Deserialize)]
#[serde(deny_unknown_fields)] // strict: an unknown key is an error with file, line and key
struct GreetingsFile {
    #[serde(default)]
    greeting: Vec<Greeting>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Greeting {
    name: String,
    text: String,
}

/// The generated file: own id → (stable game id, text).
type Table = BTreeMap<String, Entry>;
#[derive(Serialize, Deserialize, Clone)]
struct Entry {
    id: u32,
    text: String,
}

static TABLE: OnceLock<Table> = OnceLock::new();

fn build(h: &Host, users: &[vr_framework::ModDir], notes: &mut Notes) -> Table {
    let mod_ids: Vec<&str> = users.iter().map(|m| m.id.as_str()).collect();
    let mut items = Vec::new();
    for f in data::load_dir::<GreetingsFile>(users, "example", KIND, 1, notes) {
        for g in f.value.greeting {
            // "<other mod>.<name>" replaces another mod's entry; a plain name is this mod's own id
            let key = match ids::split_own(&g.name, &mod_ids) {
                Some(_) => Ok(g.name.clone()),
                None => ids::own_id(&f.mod_id, &g.name),
            };
            match key {
                Ok(k) => items.push((f.mod_id.clone(), k, g.text)),
                Err(e) => notes.error(format!("{}: {e}", f.file)),
            }
        }
    }
    let (merged, conflicts) = layer::merge_by_key(&items);
    for c in conflicts {
        notes.warn(format!("conflict: {} also set by {}; {} wins (loads later)", c.key, c.losers.join(", "), c.winner));
    }
    // stable numbers: remembered in evt_loader\ids\example_engine.json (never renumbered between starts)
    let (_, loader) = fw::game_and_loader_dirs(h);
    let (mut reg, err) = ids::IdRegistry::load(&ids::IdRegistry::default_path(&loader, ENGINE));
    notes.0.extend(err.map(|e| (vr_framework::Lvl::Warn, e)));
    let table: Table = merged.into_iter().map(|(k, (_, text))| (k.clone(), Entry { id: reg.assign(&k, |_| false), text })).collect();
    if let Err(e) = reg.save() {
        notes.error(format!("ids not saved: {e}"));
    }
    table
}

fn early(h: &'static Host) -> Result<(), String> {
    let mut notes = Notes::default();
    let users: Vec<_> = fw::active_mods(h).into_iter().filter(|m| discover::mod_uses_engine(ENGINE, &h.mod_id, m)).collect();
    // cache: hash of the engine version + every data file; a hit reuses the generated file
    let mut ih = cache::InputHash::new(&format!("{ENGINE} v{VERSION}"));
    for m in &users {
        for p in discover::toml_files(&m.dir.join("example")) {
            ih.str(&m.id).file(&p);
        }
    }
    let inputs = ih.finish();
    let (out, manifest) = (fsx::key_path(&fw::cache_dir(h), GAME_PATH), fw::cache_dir(h).join("manifest.json"));
    let hit = cache::read_json::<String>(&manifest).as_deref() == Some(inputs.as_str());
    let table = match cache::read_json::<Table>(&out).filter(|_| hit) {
        Some(t) => t,
        None => {
            let t = build(h, &users, &mut notes);
            let written = cache::write_json(&out, &t).and_then(|_| cache::write_json(&manifest, &inputs));
            notes.0.extend(written.err().map(|e| (vr_framework::Lvl::Error, e)));
            t
        }
    };
    fw::log_notes(h, &notes);
    match EarlyMode::of(h) {
        EarlyMode::Serve => match fw::serve_files(h, [(GAME_PATH, out.as_path())]).1.first() {
            Some((k, c)) => h.log(Level::Error, &format!("{k}: file_serve error {c}")),
            None => h.log(Level::Info, &format!("{} greeting(s) from {} mod(s){}, served as {GAME_PATH}", table.len(), users.len(), if hit { " (cache)" } else { "" })),
        },
        EarlyMode::Late => h.log(Level::Warn, "early phase ran late: served from the next start"),
        EarlyMode::Legacy => h.log(Level::Warn, "this ModLoader has no file_serve: update it"),
    }
    let _ = TABLE.set(table);
    Ok(())
}

/// `CMND_EVT_EXAMPLE_GREETING("<mod>.<name>")` → text, id ("" and 0 when unknown).
fn cmd_greeting(c: &mut LuaCall) {
    let key = if c.arg_type(0) == EVT_LUA_STRING { c.string(0).unwrap_or_default() } else { String::new() };
    let e = TABLE.get().and_then(|t| t.get(&key)).cloned();
    c.push_str(e.as_ref().map_or("", |e| e.text.as_str()));
    c.push_int(e.map_or(0, |e| e.id as i64));
}

fn init(h: &'static Host) -> Result<(), String> {
    for (n, code) in lua::register_all(h, &[("CMND_EVT_EXAMPLE_GREETING", cmd_greeting)]) {
        h.log(Level::Warn, &format!("{n} not registered (code {code}): is lua_bridge on?"));
    }
    Ok(())
}

declare_plugin!(init = init, early = early);
