//! The plugin glue (Windows x64): early phase = boot (merge + slots), init = Lua commands; DLL exports for other
//! plugins.
//!
//! **Lua** (module `lua_bridge`, turned on by `loader_modules` of the mod):
//! * `CMND_EVT_TEXT_ID(key)` → the text id (number, unsigned 32-bit), 0 when the key is unknown. A number argument is
//!   returned as it is.
//! * `CMND_EVT_TEXT_GET(key | id [, lang])` → the string ("" when unknown). `lang` = a folder name (`"es"`,
//!   `"zh_hans"`) or the game's language code (`funcLuaCommand(CMND_GET_LANGUAGE_CODE)`); none = `lang` of the
//!   configuration.
//!
//! **Other plugins** (`GetModuleHandleW` on the provider's DLL + `GetProcAddress`; find it with
//! `provider_find("text_engine")`):
//! * `int32_t evt_text_id(const char* key, uint32_t* out)` → `EVT_OK`, `EVT_E_NOT_FOUND`, `EVT_E_ARG`, `EVT_E_STATE`;
//! * `intptr_t evt_text_get(const char* key, uint32_t id, const char* lang, char* buf, size_t cap)` → the UTF-8
//!   length (writes at most `cap - 1` bytes + NUL; call again with a bigger buffer when `>= cap`), negative
//!   `EVT_E_*` on error. `key` NULL = by `id`; `lang` NULL = the configured language.

use crate::boot::{self, BootIn, OwnedTables};
use crate::fw::game::GameSource;
use crate::fw::{self, Lvl, ModDir, Notes};
use crate::index::{FileLazy, Query, Runtime};
use crate::{lang, Cfg};
use evt_plugin_sdk::{declare_plugin, host, try_host, Host, Level, LuaCall, EVT_E_ARG, EVT_E_NOT_FOUND, EVT_E_STATE, EVT_LUA_NUMBER, EVT_LUA_STRING, EVT_OK};
use std::collections::HashSet;
use std::ffi::{c_char, CStr};
use std::sync::{Mutex, OnceLock};
use std::time::Instant;

static RT: OnceLock<Runtime> = OnceLock::new();
static CFG: OnceLock<Cfg> = OnceLock::new();
static WARNED: Mutex<Option<HashSet<String>>> = Mutex::new(None);

fn log(level: Level, msg: &str) {
    host().log(level, msg);
}

fn cfg(h: &Host) -> &'static Cfg {
    CFG.get_or_init(|| {
        let (c, e) = Cfg::from_text(&h.config_text());
        if let Some(e) = e {
            log(Level::Warn, &format!("configuration: {e}: defaults used"));
        }
        c
    })
}

fn log_notes(n: &Notes) {
    for (l, s) in &n.0 {
        let lvl = match l {
            Lvl::Error => Level::Error,
            Lvl::Warn => Level::Warn,
            Lvl::Info => Level::Info,
            Lvl::Debug => Level::Debug,
        };
        log(lvl, s);
    }
}

fn active_mods(h: &Host) -> Vec<ModDir> {
    h.mods().into_iter().map(|m| ModDir { id: m.id, dir: m.dir, load_index: m.load_index }).collect()
}

fn inactive_mods(h: &Host, active: &[ModDir]) -> Vec<ModDir> {
    let Some(md) = h.path("mods_dir") else { return Vec::new() };
    fw::discover::installed(&md)
        .into_iter()
        .filter(|(id, _)| !active.iter().any(|a| a.id == *id))
        .map(|(id, dir)| ModDir { id, dir, load_index: 0 })
        .collect()
}

/// Build (or reuse) the merge and publish the run-time index. `write` = the slots may be rewritten (early phase only).
fn boot(h: &'static Host, write: bool) {
    let t0 = Instant::now();
    let c = cfg(h);
    let game = h.path("game_dir").unwrap_or_default();
    let loader = h.path("loader_dir").unwrap_or_else(|| game.join("evt_loader"));
    let mods = active_mods(h);
    let out = if c.enabled && write {
        let inactive = if c.prepare_inactive { inactive_mods(h, &mods) } else { Vec::new() };
        let inp = BootIn { self_id: &h.mod_id, self_dir: &h.mod_dir, loader_dir: &loader, mods: &mods, inactive: &inactive, policy: c.policy(true) };
        let out = boot::run(&inp, &mut GameSource::new(&game));
        log_notes(&out.notes);
        log(
            Level::Info,
            &format!(
                "{} mod(s) with texts{}; {} new text key(s); {} in {} ms",
                out.text_mods.len(),
                if out.text_mods.is_empty() { String::new() } else { format!(" ({})", out.text_mods.join(", ")) },
                out.index.keys.len(),
                if out.from_cache { "merge from the cache" } else { "merge built" },
                t0.elapsed().as_millis()
            ),
        );
        out
    } else {
        if !c.enabled {
            log(Level::Info, "off (enabled = false): served text files left as they are");
        }
        let out = boot::cached(&loader).unwrap_or_default();
        log(Level::Info, &format!("lookups from the last build ({} new text key(s))", out.index.keys.len()));
        out
    };
    let lazy = FileLazy { served: out.served(&h.mod_dir), base: OwnedTables { src: GameSource::new(&game), mods, self_id: h.mod_id.clone(), loader_dir: loader } };
    let _ = RT.set(Runtime::new(out.index, Box::new(lazy)));
}

fn warn_once(what: &str) {
    let mut g = WARNED.lock().unwrap_or_else(|e| e.into_inner());
    let set = g.get_or_insert_with(HashSet::new);
    if set.len() < 200 && set.insert(what.to_string()) {
        log(Level::Warn, what);
    }
}

fn lang_arg(c: &LuaCall, i: i32) -> &'static str {
    let dflt = try_host().map(|h| cfg(h).default_lang()).unwrap_or("en");
    match c.arg_type(i) {
        EVT_LUA_NUMBER => c.int(i).and_then(lang::from_game_code).unwrap_or(dflt),
        EVT_LUA_STRING => c.string(i).as_deref().and_then(lang::norm).unwrap_or(dflt),
        _ => dflt,
    }
}

/// `CMND_EVT_TEXT_ID(key)` → id (0 = unknown).
fn cmd_text_id(c: &mut LuaCall) {
    match c.arg_type(0) {
        EVT_LUA_NUMBER => {
            let n = c.num(0).unwrap_or(0.0);
            c.push_num(n);
        }
        EVT_LUA_STRING => {
            let key = c.string(0).unwrap_or_default();
            let id = RT.get().and_then(|rt| rt.text_id(&key));
            if id.is_none() {
                warn_once(&format!("CMND_EVT_TEXT_ID: unknown key `{key}` (0 returned)"));
            }
            c.push_num(id.unwrap_or(0) as f64);
        }
        _ => c.push_num(0.0),
    }
}

/// `CMND_EVT_TEXT_GET(key | id [, lang])` → string ("" = unknown).
fn cmd_text_get(c: &mut LuaCall) {
    let lang = lang_arg(c, 1);
    let s = match (RT.get(), c.arg_type(0)) {
        (Some(rt), EVT_LUA_STRING) => {
            let key = c.string(0).unwrap_or_default();
            let r = rt.text_get(Query::Key(&key), lang);
            if r.is_none() {
                warn_once(&format!("CMND_EVT_TEXT_GET: no text for `{key}` in {lang} (\"\" returned)"));
            }
            r
        }
        (Some(rt), EVT_LUA_NUMBER) => {
            let id = c.int(0).unwrap_or(0) as u32;
            rt.text_get(Query::Id(id), lang)
        }
        _ => None,
    };
    c.push_str(&s.unwrap_or_default());
}

fn early(h: &'static Host) -> Result<(), String> {
    boot(h, true);
    Ok(())
}

fn init(h: &'static Host) -> Result<(), String> {
    if RT.get().is_none() {
        log(
            Level::Warn,
            "the early phase did not run: the served text files are NOT updated at this start (the game may already be reading them); lookups use the last build",
        );
        boot(h, false);
    }
    for (n, f) in [("CMND_EVT_TEXT_ID", cmd_text_id as fn(&mut LuaCall)), ("CMND_EVT_TEXT_GET", cmd_text_get)] {
        if let Err(e) = h.lua_register(n, f) {
            log(Level::Warn, &format!("{n} not registered (code {e}: module lua_bridge off, or the name is taken): texts still merge"));
        }
    }
    Ok(())
}

declare_plugin!(init = init, early = early);

unsafe fn cstr<'a>(p: *const c_char) -> Option<&'a str> {
    if p.is_null() {
        None
    } else {
        CStr::from_ptr(p).to_str().ok()
    }
}

/// Export for other plugins: id of a text key.
///
/// # Safety
/// `key`: NUL-terminated UTF-8; `out`: writable.
#[no_mangle]
pub unsafe extern "C" fn evt_text_id(key: *const c_char, out: *mut u32) -> i32 {
    std::panic::catch_unwind(|| {
        let (Some(k), false) = (cstr(key), out.is_null()) else { return EVT_E_ARG };
        let Some(rt) = RT.get() else { return EVT_E_STATE };
        match rt.text_id(k) {
            Some(id) => {
                *out = id;
                EVT_OK
            }
            None => EVT_E_NOT_FOUND,
        }
    })
    .unwrap_or(EVT_E_STATE)
}

/// Export for other plugins: the text of a key (or of `id` when `key` is NULL) in `lang` (NULL = configured).
///
/// # Safety
/// `key` / `lang`: NULL or NUL-terminated UTF-8; `buf`: `cap` writable bytes (or NULL with `cap` 0).
#[no_mangle]
pub unsafe extern "C" fn evt_text_get(key: *const c_char, id: u32, lang: *const c_char, buf: *mut c_char, cap: usize) -> isize {
    std::panic::catch_unwind(|| {
        let Some(rt) = RT.get() else { return EVT_E_STATE as isize };
        let l = match cstr(lang) {
            Some(s) => match lang::norm(s) {
                Some(l) => l,
                None => return EVT_E_ARG as isize,
            },
            None => try_host().map(|h| cfg(h).default_lang()).unwrap_or("en"),
        };
        let q = match cstr(key) {
            Some(k) => Query::Key(k),
            None if key.is_null() => Query::Id(id),
            None => return EVT_E_ARG as isize,
        };
        let Some(s) = rt.text_get(q, l) else { return EVT_E_NOT_FOUND as isize };
        let b = s.as_bytes();
        if !buf.is_null() && cap > 0 {
            let n = b.len().min(cap - 1);
            std::ptr::copy_nonoverlapping(b.as_ptr(), buf as *mut u8, n);
            *buf.add(n) = 0;
        }
        b.len() as isize
    })
    .unwrap_or(EVT_E_STATE as isize)
}
