//! Run time of the plugin (nie.exe, through the ModLoader API v1): configuration, storage root, the Lua commands,
//! the worker thread (slot events, global / immediate writes) and the C exports other plugins can call.
//!
//! Slot source, chosen at every sync (the built-in modules start after the plugins' init):
//! * **events**: the loader answers `game_state("save.active_slot")` → its `save.event.*` ring (save, switch, copy,
//!   delete, new game) and `save.locked_slot`;
//! * **state file**: the loader's save_slots module is on (`loader.modules_mask` bit 1) but publishes no events (an
//!   older ModLoader): active slot from `evt_loader\state.json`, slot 1 locked, saves / removals seen on the Steam cloud
//!   file ([`crate::watch`]); copies are not followed;
//! * **single**: no save_slots module: one slot (1, the retail file), saves seen on the Steam cloud file.
//!
//! Without the Steam cloud folder (not found) the non-event sources cannot see saves: slot data is then written at
//! once (logged).

use crate::codec::{self, Event};
use crate::store::{Outcome, Scope, SetError, Store};
use crate::watch::{self, FileWatch, Seen};
use crate::{slots, SaveEngineCfg};
use evt_plugin_sdk::{declare_plugin, evt_error, evt_info, evt_warn, Host, LuaCall, EVT_LUA_BOOLEAN, EVT_LUA_NIL, EVT_LUA_NONE, EVT_LUA_NUMBER, EVT_LUA_STRING};
use serde_json::Value;
use std::collections::BTreeSet;
use std::ffi::c_char;
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};

/// `save.event.*` ring size of the loader (`saveslots::events::RING`).
const RING: u64 = 64;
/// `loader.modules_mask` bit of the built-in save_slots module.
const MOD_SAVE_SLOTS: i64 = 2;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mode {
    Unknown,
    Events,
    StateFile,
    Single,
}

struct Src {
    mode: Mode,
    seen: u64,
    watch: FileWatch,
    /// Steam cloud folder (None = not looked up yet; Some(None) = not found).
    remote: Option<Option<PathBuf>>,
    state_json: PathBuf,
    state_stamp: Option<watch::Stamp>,
    state_slot: u8,
    warned_no_remote: bool,
    /// Mods whose slot data must be written at the next worker tick (`now` / COMMIT).
    commit_now: BTreeSet<String>,
}

struct Inner {
    store: Store,
    src: Src,
}

struct Engine {
    cfg: SaveEngineCfg,
    inner: Mutex<Inner>,
    warned: Mutex<BTreeSet<String>>,
}

static ENGINE: OnceLock<Engine> = OnceLock::new();

fn eng() -> Option<&'static Engine> {
    ENGINE.get()
}

fn lock(e: &Engine) -> std::sync::MutexGuard<'_, Inner> {
    e.inner.lock().unwrap_or_else(|p| p.into_inner())
}

fn warn_once(key: String, msg: impl FnOnce() -> String) {
    if let Some(e) = eng() {
        if e.warned.lock().unwrap_or_else(|p| p.into_inner()).insert(key) {
            evt_warn!("{}", msg());
        }
    }
}

fn report(what: &str, o: &Outcome) {
    if !o.written.is_empty() {
        evt_info!("{what}: wrote {}", o.written.join(", "));
    }
    if !o.dropped.is_empty() {
        evt_info!("{what}: changes not saved by the game dropped for {}", o.dropped.join(", "));
    }
    if let Some(t) = &o.trashed {
        evt_info!("{what}: old data moved to {}", t.display());
    }
    if o.copied > 0 {
        evt_info!("{what}: {} file(s) copied", o.copied);
    }
    for e in &o.errors {
        evt_error!("{what}: {e}");
    }
}

// ---------------------------------------------------------------- init

fn local_low() -> Option<PathBuf> {
    use windows_sys::Win32::System::Com::CoTaskMemFree;
    use windows_sys::Win32::UI::Shell::{FOLDERID_LocalAppDataLow, SHGetKnownFolderPath};
    unsafe {
        let mut p: windows_sys::core::PWSTR = std::ptr::null_mut();
        let hr = SHGetKnownFolderPath(&FOLDERID_LocalAppDataLow, 0, std::ptr::null_mut(), &mut p);
        let r = if hr >= 0 && !p.is_null() {
            let mut n = 0;
            while *p.add(n) != 0 {
                n += 1;
            }
            Some(PathBuf::from(String::from_utf16_lossy(std::slice::from_raw_parts(p, n))))
        } else {
            None
        };
        if !p.is_null() {
            CoTaskMemFree(p as *const core::ffi::c_void);
        }
        r
    }
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(Some(0)).collect()
}

/// `HKCU\Software\Valve\Steam` `SteamPath` and `ActiveProcess\ActiveUser`.
fn steam_registry() -> (Option<PathBuf>, Option<u32>) {
    use windows_sys::Win32::System::Registry::{RegGetValueW, HKEY_CURRENT_USER, RRF_RT_REG_DWORD, RRF_RT_REG_SZ};
    unsafe {
        let mut buf = [0u16; 520];
        let mut size = (buf.len() * 2) as u32;
        let (k, v) = (wide(r"Software\Valve\Steam"), wide("SteamPath"));
        let path = (RegGetValueW(HKEY_CURRENT_USER, k.as_ptr(), v.as_ptr(), RRF_RT_REG_SZ, std::ptr::null_mut(), buf.as_mut_ptr() as _, &mut size) == 0)
            .then(|| {
                let n = buf.iter().position(|&c| c == 0).unwrap_or(0);
                PathBuf::from(String::from_utf16_lossy(&buf[..n]).replace('/', "\\"))
            });
        let mut user = 0u32;
        let mut size = 4u32;
        let (k, v) = (wide(r"Software\Valve\Steam\ActiveProcess"), wide("ActiveUser"));
        let ok = RegGetValueW(HKEY_CURRENT_USER, k.as_ptr(), v.as_ptr(), RRF_RT_REG_DWORD, std::ptr::null_mut(), &mut user as *mut u32 as _, &mut size) == 0;
        (path, ok.then_some(user))
    }
}

fn init(host: &'static Host) -> Result<(), String> {
    let cfg = match SaveEngineCfg::parse(&host.config_text()) {
        Ok(c) => c,
        Err(e) => {
            evt_warn!("config: {e}; using the defaults");
            SaveEngineCfg::default()
        }
    };
    let game_dir = host.path("game_dir").ok_or("game_dir unknown")?;
    let loader_dir = host.path("loader_dir").unwrap_or_else(|| game_dir.join("evt_loader"));
    let (root, note) = crate::storage_root(&cfg.storage, &game_dir, local_low().as_deref());
    if let Some(n) = note {
        evt_warn!("storage: {n}");
    }
    std::fs::create_dir_all(&root).map_err(|e| format!("cannot create {}: {e}", root.display()))?;
    if cfg.slots.takeover {
        evt_warn!(
            "[slots] takeover = true is not available in this version: the ModLoader's save_slots module keeps the \
             save slots (docs/game/engine/save-engine.md §6); ignored"
        );
    }
    let layout = slots::Layout::new(&cfg.slots);
    let commands: [(&str, vr_framework::lua::Command); 10] = [
        ("CMND_EVT_SAVE_GET", cmd_get),
        ("CMND_EVT_SAVE_SET", cmd_set),
        ("CMND_EVT_SAVE_SET_BOOL", cmd_set_bool),
        ("CMND_EVT_SAVE_DEL", cmd_del),
        ("CMND_EVT_SAVE_GLOBAL_GET", cmd_gget),
        ("CMND_EVT_SAVE_GLOBAL_SET", cmd_gset),
        ("CMND_EVT_SAVE_GLOBAL_SET_BOOL", cmd_gset_bool),
        ("CMND_EVT_SAVE_GLOBAL_DEL", cmd_gdel),
        ("CMND_EVT_SAVE_COMMIT", cmd_commit),
        ("CMND_EVT_SAVE_SLOT", cmd_slot),
    ];
    for (n, f) in commands {
        host.lua_register(n, f).map_err(|r| format!("Lua command {n}: error {r} (needs [modules] lua_bridge = true)"))?;
    }
    let period = cfg.flush_period_ms();
    evt_info!(
        "on: data in {} (storage {}); slot data committed {}; {} Lua commands; slots layout (not active) {:?} locked {:?}",
        root.display(),
        if cfg.storage.path.trim().is_empty() { cfg.storage.location.as_str() } else { "path" },
        if cfg.all_immediate() { "at once (commit = immediate)".to_string() } else { format!("when the game saves (at once for {:?})", cfg.immediate_mods) },
        commands.len(),
        layout.player,
        layout.locked
    );
    let src = Src {
        mode: Mode::Unknown,
        seen: 0,
        watch: FileWatch::default(),
        remote: None,
        state_json: loader_dir.join("state.json"),
        state_stamp: None,
        state_slot: 1,
        warned_no_remote: false,
        commit_now: BTreeSet::new(),
    };
    let trash = cfg.trash_limit();
    let _ = ENGINE.set(Engine { cfg, inner: Mutex::new(Inner { store: Store::new(root, trash), src }), warned: Mutex::new(BTreeSet::new()) });
    // last: nothing may run before init is sure to return OK (docs/app/modloader-plugins.md §7)
    if !host.spawn("worker", move || worker(period)) {
        return Err("cannot start the worker thread".into());
    }
    Ok(())
}

// ---------------------------------------------------------------- slot sync

fn state_i64(key: &str) -> Option<i64> {
    evt_plugin_sdk::try_host()?.state(key)
}

fn set_mode(s: &mut Src, m: Mode) {
    if s.mode != m {
        evt_info!(
            "slot source: {}",
            match m {
                Mode::Events => "ModLoader save events (save / switch / copy / delete / new game)",
                Mode::StateFile => "evt_loader\\state.json + the Steam cloud file (this ModLoader publishes no save events: slot copies are not followed)",
                Mode::Single => "one slot (no save_slots module): the Steam cloud file",
                Mode::Unknown => "unknown",
            }
        );
        s.mode = m;
    }
}

fn apply(store: &mut Store, locked: u8, ev: Event) {
    let writable = |n: u8| n != slots::NO_SLOT && n != locked;
    match ev {
        Event::Saved(n) if n == store.slot() => report(&format!("game saved slot {n}"), &store.commit_slot("game save")),
        Event::Saved(_) => {}
        Event::Switched { to, from } => report(&format!("slot {from} -> {to}"), &store.set_slot(to, writable(to))),
        Event::Copied { src, dst } => report(&format!("slot {src} copied into slot {dst}"), &store.on_slot_copied(src, dst)),
        Event::Deleted(n) => report(&format!("slot {n} deleted"), &store.on_slot_gone(n, "deleted")),
        Event::NewGame(n) => report(&format!("new game in slot {n}"), &store.on_slot_gone(n, "new_game")),
    }
}

/// Bring the store in step with the game (worker and every Lua command, so a write never lands in the old slot).
fn sync(inner: &mut Inner, from_worker: bool) {
    let Inner { store, src } = inner;
    if let Some(active) = state_i64("save.active_slot") {
        set_mode(src, Mode::Events);
        let locked = state_i64("save.locked_slot").unwrap_or(1) as u8;
        let seq = state_i64("save.event_seq").unwrap_or(0).max(0) as u64;
        let (range, lost) = vr_framework::state::ring_pending(src.seen, seq, RING);
        if lost {
            evt_warn!("save events {}..{} were missed (ring of {RING}): resynchronised on the active slot", src.seen + 1, range.start() - 1);
        }
        for n in range {
            match state_i64(&format!("save.event.{n}")).and_then(codec::decode) {
                Some(ev) => apply(store, locked, ev),
                None => evt_warn!("save event #{n} unreadable"),
            }
        }
        src.seen = seq.max(src.seen);
        let a = active.clamp(0, 255) as u8;
        report(&format!("active slot {a}"), &store.set_slot(a, a != slots::NO_SLOT && a != locked));
        return;
    }
    let mask = state_i64("loader.modules_mask").unwrap_or(0);
    if mask & MOD_SAVE_SLOTS != 0 {
        set_mode(src, Mode::StateFile);
        let st = watch::stamp(&src.state_json);
        if st != src.state_stamp || store.slot() == 0 {
            src.state_stamp = st;
            let layout = slots::Layout::new(&slots::SlotsCfg { count: 3, ..Default::default() });
            src.state_slot = slots::State::load(&src.state_json, &layout).active_slot;
        }
        let a = src.state_slot;
        report(&format!("active slot {a}"), &store.set_slot(a, a != 1));
    } else if from_worker || store.slot() == 0 {
        // before the built-in modules started the mask is 0 too: a later sync corrects the mode
        set_mode(src, Mode::Single);
        report("active slot 1", &store.set_slot(1, true));
    }
}

fn remote_dir(src: &mut Src) -> Option<PathBuf> {
    if src.remote.is_none() {
        let (steam, user) = steam_registry();
        let d = steam.as_deref().and_then(|s| watch::find_remote_dir(s, user));
        match &d {
            Some(d) => evt_info!("retail saves (Steam cloud folder): {}", d.display()),
            None => evt_warn!("Steam cloud folder of the game not found (SteamPath {:?}, ActiveUser {:?})", steam, user),
        }
        src.remote = Some(d);
    }
    src.remote.clone().flatten()
}

fn worker(period: u64) {
    let Some(e) = eng() else { return };
    loop {
        {
            let mut g = lock(e);
            sync(&mut g, true);
            let Inner { store, src } = &mut *g;
            if matches!(src.mode, Mode::StateFile | Mode::Single) && store.slot() != 0 {
                match remote_dir(src) {
                    Some(dir) => {
                        let f = dir.join(slots::slot_file(slots::CANON_USERDATA, store.slot()));
                        match src.watch.step(store.slot(), watch::stamp(&f)) {
                            Seen::Saved => report(&format!("game saved slot {}", store.slot()), &store.commit_slot("game save")),
                            Seen::Gone => {
                                let n = store.slot();
                                report(&format!("slot {n} save removed"), &store.on_slot_gone(n, "deleted"));
                            }
                            Seen::Nothing => {}
                        }
                    }
                    None if !src.warned_no_remote => {
                        src.warned_no_remote = true;
                        evt_warn!("the game's saves cannot be seen: slot data is written at once instead of with the game's saves");
                    }
                    None => {}
                }
            }
            let all_now = src.warned_no_remote && src.mode != Mode::Events;
            let now = std::mem::take(&mut src.commit_now);
            let o = store.flush(|m| all_now || now.contains(m) || e.cfg.is_immediate(m));
            if !o.written.is_empty() || !o.errors.is_empty() {
                report("write", &o);
            }
            for n in std::mem::take(&mut store.notes) {
                evt_warn!("{n}");
            }
        }
        std::thread::sleep(std::time::Duration::from_millis(period));
    }
}

// ---------------------------------------------------------------- Lua commands

fn mod_arg(c: &LuaCall, what: &str) -> Option<String> {
    match c.string(0) {
        Some(m) if crate::valid_mod_id(&m) => Some(m),
        other => {
            let shown = other.unwrap_or_else(|| "<not a string>".into());
            warn_once(format!("id:{what}:{shown}"), || {
                format!("{what}: first argument must be the calling mod's id (a-z 0-9 _ . -), got {shown:?}; call refused")
            });
            None
        }
    }
}

fn key_arg(c: &LuaCall, what: &str, m: &str) -> Option<String> {
    match c.string(1) {
        Some(k) if crate::valid_key(&k) => Some(k),
        _ => {
            warn_once(format!("key:{what}:{m}"), || format!("{what} ({m}): second argument must be a key string of 1..=128 bytes"));
            None
        }
    }
}

use vr_framework::lua::push_json as push_value;

/// Echo argument `i` back (the default of GET).
fn push_arg(c: &mut LuaCall, i: i32) {
    match c.arg_type(i) {
        EVT_LUA_NUMBER => {
            let v = c.num(i).unwrap_or(0.0);
            c.push_num(v)
        }
        EVT_LUA_STRING => {
            let s = c.string(i).unwrap_or_default();
            c.push_str(&s)
        }
        _ => {}
    }
}

fn with_engine<R>(f: impl FnOnce(&Engine, &mut Inner) -> R) -> Option<R> {
    let e = eng()?;
    let mut g = lock(e);
    sync(&mut g, false);
    Some(f(e, &mut g))
}

fn get(c: &mut LuaCall, scope: Scope, what: &str) {
    let Some(m) = mod_arg(c, what) else { return };
    let Some(k) = key_arg(c, what, &m) else { return };
    match with_engine(|_, g| g.store.get(scope, &m, &k)).flatten() {
        Some(v) => push_value(c, &v),
        None => push_arg(c, 2),
    }
}

/// `value` read from argument 2: number / string; `as_bool` = SET_BOOL (a number, 0 = false). nil = delete.
fn set(c: &mut LuaCall, scope: Scope, what: &str, as_bool: bool) {
    let Some(m) = mod_arg(c, what) else {
        c.push_bool(false);
        return;
    };
    let Some(k) = key_arg(c, what, &m) else {
        c.push_bool(false);
        return;
    };
    let v = match c.arg_type(2) {
        EVT_LUA_NUMBER if as_bool => Some(Value::Bool(c.num(2).unwrap_or(0.0) != 0.0)),
        EVT_LUA_NUMBER => c.num(2).and_then(crate::num_value),
        EVT_LUA_STRING if !as_bool => c.string(2).and_then(|s| crate::str_value(&s)),
        EVT_LUA_NIL | EVT_LUA_NONE => None,
        EVT_LUA_BOOLEAN => {
            warn_once(format!("bool:{what}:{m}"), || {
                format!(
                    "{what} ({m}): a Lua boolean cannot be read by plugin API v1: use CMND_EVT_SAVE_{}SET_BOOL(mod, key, 1/0) \
                     (EvtSave does it for you); call refused",
                    if scope == Scope::Global { "GLOBAL_" } else { "" }
                )
            });
            c.push_bool(false);
            return;
        }
        _ => {
            warn_once(format!("type:{what}:{m}"), || format!("{what} ({m}): value must be a number or a string (tables: json strings); call refused"));
            c.push_bool(false);
            return;
        }
    };
    let now = c.num(3).is_some_and(|n| n != 0.0);
    let is_del = matches!(c.arg_type(2), EVT_LUA_NIL | EVT_LUA_NONE);
    let r = with_engine(|_, g| {
        let r = match v {
            Some(v) => g.store.set(scope, &m, &k, v).map(|_| true),
            None if is_del => {
                g.store.del(scope, &m, &k);
                Ok(true)
            }
            None => Err(SetError::Value),
        };
        if r.is_ok() && now && scope == Scope::Slot {
            g.src.commit_now.insert(m.clone());
        }
        r
    });
    match r {
        Some(Ok(_)) => c.push_bool(true),
        Some(Err(e)) => {
            warn_once(format!("set:{what}:{m}:{e:?}"), || format!("{what} ({m}, key {k:?}): refused ({e:?}; limits: key 128 B, string 64 KiB, 4096 keys)"));
            c.push_bool(false)
        }
        None => c.push_bool(false),
    }
}

fn del(c: &mut LuaCall, scope: Scope, what: &str) {
    let Some(m) = mod_arg(c, what) else {
        c.push_bool(false);
        return;
    };
    let Some(k) = key_arg(c, what, &m) else {
        c.push_bool(false);
        return;
    };
    let had = with_engine(|_, g| g.store.del(scope, &m, &k)).unwrap_or(false);
    c.push_bool(had);
}

/// `(mod, key [, default]) -> value | default | nothing`
fn cmd_get(c: &mut LuaCall) {
    get(c, Scope::Slot, "SAVE_GET")
}
/// `(mod, key, value [, now]) -> ok`
fn cmd_set(c: &mut LuaCall) {
    set(c, Scope::Slot, "SAVE_SET", false)
}
/// `(mod, key, 1/0 [, now]) -> ok`
fn cmd_set_bool(c: &mut LuaCall) {
    set(c, Scope::Slot, "SAVE_SET_BOOL", true)
}
/// `(mod, key) -> existed`
fn cmd_del(c: &mut LuaCall) {
    del(c, Scope::Slot, "SAVE_DEL")
}
fn cmd_gget(c: &mut LuaCall) {
    get(c, Scope::Global, "SAVE_GLOBAL_GET")
}
fn cmd_gset(c: &mut LuaCall) {
    set(c, Scope::Global, "SAVE_GLOBAL_SET", false)
}
fn cmd_gset_bool(c: &mut LuaCall) {
    set(c, Scope::Global, "SAVE_GLOBAL_SET_BOOL", true)
}
fn cmd_gdel(c: &mut LuaCall) {
    del(c, Scope::Global, "SAVE_GLOBAL_DEL")
}

/// `(mod) -> ok`: write the mod's slot data at the next worker tick, without waiting for the game's save. false when
/// the active slot is not writable (locked slot / none).
fn cmd_commit(c: &mut LuaCall) {
    let Some(m) = mod_arg(c, "SAVE_COMMIT") else {
        c.push_bool(false);
        return;
    };
    let ok = with_engine(|_, g| {
        let w = g.store.writable();
        if w {
            g.src.commit_now.insert(m.clone());
        }
        w
    })
    .unwrap_or(false);
    c.push_bool(ok);
}

/// `() -> slot, writable`: the slot the data follows now (0 = none yet) and whether it can be committed.
fn cmd_slot(c: &mut LuaCall) {
    let (s, w) = with_engine(|_, g| (g.store.slot(), g.store.writable())).unwrap_or((0, false));
    c.push_int(s as i64);
    c.push_bool(w);
}

// ---------------------------------------------------------------- C exports for other plugins

use vr_framework::lua::cstr;

fn scope_of(global: i32) -> Scope {
    if global != 0 {
        Scope::Global
    } else {
        Scope::Slot
    }
}

/// Version of the exported functions below.
#[no_mangle]
pub extern "C" fn save_engine_api_version() -> u32 {
    1
}

/// Value of `key` as JSON text into `buf` (at most `cap - 1` bytes + NUL). Returns the full length, -1 = no such key,
/// -2 = bad arguments / engine not running.
///
/// # Safety
/// `mod_id` / `key` NUL-terminated UTF-8; `buf` writable for `cap` bytes (or NULL with `cap` 0).
#[no_mangle]
pub unsafe extern "C" fn save_engine_get(mod_id: *const c_char, global: i32, key: *const c_char, buf: *mut c_char, cap: usize) -> isize {
    let (Some(m), Some(k)) = (cstr(mod_id), cstr(key)) else { return -2 };
    if !crate::valid_mod_id(m) || !crate::valid_key(k) {
        return -2;
    }
    let r = std::panic::catch_unwind(|| with_engine(|_, g| g.store.get(scope_of(global), m, k)));
    let Ok(Some(v)) = r else { return -2 };
    let Some(v) = v else { return -1 };
    let s = v.to_string();
    if !buf.is_null() && cap > 0 {
        let n = s.len().min(cap - 1);
        std::ptr::copy_nonoverlapping(s.as_ptr(), buf as *mut u8, n);
        *buf.add(n) = 0;
    }
    s.len() as isize
}

/// Set `key` to the JSON value `json` (any JSON value; NULL = delete). 0 = OK, -1 = bad arguments / refused.
///
/// # Safety
/// NUL-terminated UTF-8 strings (or NULL `json`).
#[no_mangle]
pub unsafe extern "C" fn save_engine_set(mod_id: *const c_char, global: i32, key: *const c_char, json: *const c_char) -> i32 {
    let (Some(m), Some(k)) = (cstr(mod_id), cstr(key)) else { return -1 };
    if !crate::valid_mod_id(m) {
        return -1;
    }
    let v = if json.is_null() {
        None
    } else {
        match cstr(json).and_then(|j| serde_json::from_str::<Value>(j).ok()) {
            Some(v) => Some(v),
            None => return -1,
        }
    };
    let r = std::panic::catch_unwind(|| {
        with_engine(|_, g| match v {
            Some(v) => g.store.set(scope_of(global), m, k, v).is_ok(),
            None => {
                g.store.del(scope_of(global), m, k);
                true
            }
        })
    });
    if matches!(r, Ok(Some(true))) {
        0
    } else {
        -1
    }
}

/// Write the mod's slot data at the next worker tick. 0 = queued, -1 = slot not writable / bad id.
///
/// # Safety
/// `mod_id` NUL-terminated UTF-8.
#[no_mangle]
pub unsafe extern "C" fn save_engine_commit(mod_id: *const c_char) -> i32 {
    let Some(m) = cstr(mod_id).filter(|m| crate::valid_mod_id(m)) else { return -1 };
    let r = std::panic::catch_unwind(|| {
        with_engine(|_, g| {
            let w = g.store.writable();
            if w {
                g.src.commit_now.insert(m.to_string());
            }
            w
        })
    });
    if matches!(r, Ok(Some(true))) {
        0
    } else {
        -1
    }
}

/// The slot the data follows now (0 = none / engine not running).
#[no_mangle]
pub extern "C" fn save_engine_active_slot() -> i32 {
    std::panic::catch_unwind(|| with_engine(|_, g| g.store.slot() as i32).unwrap_or(0)).unwrap_or(0)
}

declare_plugin!(init = init);
