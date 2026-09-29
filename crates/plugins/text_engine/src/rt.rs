//! The plugin glue (Windows x64): early phase = boot (merge into the cache + `file_serve`, shown at this start; the
//! slots fallback on a ModLoader without `file_serve`), init = Lua commands; DLL exports for other plugins.
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

use crate::boot::{self, BootIn, OwnedTables, Serve};
use crate::fw::game::GameSource;
use crate::fw::Notes;
use crate::index::{FileLazy, Query, Runtime};
use crate::{lang, Cfg};
use evt_plugin_sdk::{declare_plugin, host, try_host, Host, Level, LuaCall, EVT_E_ARG, EVT_E_NOT_FOUND, EVT_E_STATE, EVT_LUA_NUMBER, EVT_LUA_STRING, EVT_OK};
use std::collections::HashSet;
use std::ffi::c_char;
use std::sync::OnceLock;
use std::time::Instant;
use vr_framework::host::{self as fwhost, EarlyMode};
use vr_framework::lua::{self as fwlua, cstr, WarnOnce};

static RT: OnceLock<Runtime> = OnceLock::new();
static CFG: OnceLock<Cfg> = OnceLock::new();
static WARNED: WarnOnce = WarnOnce::new(200);

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
    fwhost::log_notes(host(), n);
}

/// What the boot may do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mode {
    /// Early phase at the entry point with `file_serve`: build into the cache and serve (shown at this start).
    Serve,
    /// Early phase run late (the overlay is sealed): build into the cache, serve nothing (shown from the next start).
    Late,
    /// A ModLoader without `file_serve`: the slots fallback.
    Slots,
    /// Init without an early phase: lookups from the last build, nothing written.
    Lookup,
}

/// `file_serve` every file of `out` (early phase at the entry point), then delete the legacy slots it covers (the
/// overlay no longer points at them). Returns the keys served.
fn serve_files(h: &'static Host, out: &boot::BootOut) -> HashSet<String> {
    let files = out.to_serve();
    let (served, refused) = fwhost::serve_files(h, files.iter().map(|(k, p)| (*k, p.as_path())));
    for (key, c) in refused {
        log(Level::Error, &format!("{key}: file_serve error {c}: NOT served (the game shows its own texts of that table)"));
    }
    let ok: HashSet<String> = served.into_iter().collect();
    let legacy: Vec<String> = boot::legacy_slots(&h.mod_dir).into_iter().filter(|k| ok.contains(k)).collect();
    if !legacy.is_empty() {
        let (n, errs) = boot::retire_legacy(&h.mod_dir, &legacy);
        for e in errs {
            log(Level::Warn, &format!("old slot not deleted ({e}): delete mods\\{}\\files with the game closed", h.mod_id));
        }
        if n > 0 {
            log(Level::Info, &format!("{n} old slot file(s) of mods\\{}\\files deleted (the texts are served from the cache now)", h.mod_id));
        }
    }
    ok
}

/// Build (or reuse) the merge, serve it and publish the run-time index.
fn boot(h: &'static Host, mode: Mode) {
    let t0 = Instant::now();
    let c = cfg(h);
    let (game, loader) = fwhost::game_and_loader_dirs(h);
    let mods = fwhost::active_mods(h);
    let out = if c.enabled && mode != Mode::Lookup {
        let (serve, inactive) = match mode {
            Mode::Slots => (Serve::Slots(c.policy(true)), if c.prepare_inactive { fwhost::inactive_mods(h, &mods) } else { Vec::new() }),
            _ => (Serve::Cache, Vec::new()),
        };
        let inp = BootIn { self_id: &h.mod_id, self_dir: &h.mod_dir, loader_dir: &loader, mods: &mods, inactive: &inactive, serve };
        let mut out = boot::run(&inp, &mut GameSource::new(&game));
        log_notes(&out.notes);
        let served = match mode {
            Mode::Serve => {
                let ok = serve_files(h, &out);
                // lookups read what the game reads
                out.files.retain(|f| f.file.is_none() || ok.contains(&f.key));
                format!("; {} text file(s) served", ok.len())
            }
            Mode::Late => {
                out.files.retain(|f| f.file.is_none());
                log(Level::Warn, "the early phase ran late (the game already runs): the merged texts are NOT served at this start; they show from the next start");
                String::new()
            }
            _ => String::new(),
        };
        log(
            Level::Info,
            &format!(
                "{} mod(s) with texts{}; {} new text key(s); {}{served} in {} ms",
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
            log(Level::Info, "off (enabled = false): no merged text file served (old slots, if any, are left as they are)");
        }
        let mut out = boot::cached(&loader).unwrap_or_default();
        // cache files are served only by a boot that serves them
        out.files.retain(|f| f.file.is_none());
        log(Level::Info, &format!("lookups from the last build ({} new text key(s))", out.index.keys.len()));
        out
    };
    let lazy = FileLazy { served: out.served(&h.mod_dir), base: OwnedTables { src: GameSource::new(&game), mods, self_id: h.mod_id.clone(), loader_dir: loader } };
    let _ = RT.set(Runtime::new(out.index, Box::new(lazy)));
}

fn warn_once(what: &str) {
    if WARNED.first(what) {
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
    let mode = match EarlyMode::of(h) {
        EarlyMode::Serve => Mode::Serve,
        // a ModLoader without phase / file_serve
        EarlyMode::Legacy => Mode::Slots,
        EarlyMode::Late => Mode::Late,
    };
    boot(h, mode);
    Ok(())
}

fn init(h: &'static Host) -> Result<(), String> {
    if RT.get().is_none() {
        log(
            Level::Warn,
            "the early phase did not run: the merged text files are NOT served / updated at this start (the game may already be reading them); lookups use the last build",
        );
        boot(h, Mode::Lookup);
    }
    for (n, e) in fwlua::register_all(h, &[("CMND_EVT_TEXT_ID", cmd_text_id), ("CMND_EVT_TEXT_GET", cmd_text_get)]) {
        log(Level::Warn, &format!("{n} not registered (code {e}: module lua_bridge off, or the name is taken): texts still merge"));
    }
    Ok(())
}

declare_plugin!(init = init, early = early);

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
