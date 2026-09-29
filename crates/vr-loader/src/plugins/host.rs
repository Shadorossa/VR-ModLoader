//! Run time of the plugin host: loads every plugin DLL of the active mods (init thread, load order), gives it the
//! API table v1 (`evt_plugin_sdk::abi::EvtApi`) and isolates failures (a plugin whose init fails or faults is
//! disabled: its hooks leave their chains, its IAT patches are restored, its Lua commands are dropped; the game goes
//! on). Every API call made by a plugin is logged with the mod id as prefix.

use super::{check_api_version, mod_config, PluginSpec};
use crate::lua::Call;
use crate::{debug, error, info, warn};
use evt_plugin_sdk::abi::*;
use std::collections::HashMap;
use std::ffi::{c_char, c_void, CStr, CString};
use std::os::windows::ffi::OsStrExt;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicUsize, Ordering};
use std::sync::{Condvar, Mutex, OnceLock};
use std::time::Instant;
use windows_sys::Win32::Foundation::GetLastError;
use windows_sys::Win32::System::LibraryLoader::{GetModuleHandleA, GetProcAddress, LoadLibraryExW, LOAD_WITH_ALTERED_SEARCH_PATH};

extern "C" {
    fn evt_seh_read(src: usize, dst: *mut u8, n: usize) -> i32;
    fn evt_seh_write(dst: usize, src: *const u8, n: usize) -> i32;
}

/// One active mod as the API shows it.
struct ModRec {
    id: CString,
    version: CString,
    dir: CString,
    plugin: CString,
    load_index: u32,
    state: AtomicU32,
    /// `(name, version)` of its id and every `provides` entry.
    provides: Vec<(String, CString)>,
}

/// One plugin (its address is the `EvtPlugin*` handle).
struct PluginRec {
    spec: PluginSpec,
    mod_index: usize,
    config: String,
    /// IAT patches: (slot, detour, orig cell) — restored when init fails.
    iat: Mutex<Vec<(usize, usize, usize)>>,
    /// Lua command hashes it registered.
    cmds: Mutex<Vec<u32>>,
    /// HMODULE once loaded (0 = not loaded).
    hmodule: AtomicUsize,
}

struct SyncApi(EvtApi);
unsafe impl Sync for SyncApi {}
unsafe impl Send for SyncApi {}

static MODS: OnceLock<Vec<ModRec>> = OnceLock::new();
static PLUGINS: OnceLock<Vec<Box<PluginRec>>> = OnceLock::new();
static LOADED: Mutex<Vec<PluginSpec>> = Mutex::new(Vec::new());
static API: OnceLock<SyncApi> = OnceLock::new();
static LOADER_VERSION_C: OnceLock<CString> = OnceLock::new();
/// Plugin Lua commands: hash -> (handler, user, plugin index).
static CMDS: Mutex<Option<HashMap<u32, (EvtLuaHandler, usize, usize)>>> = Mutex::new(None);
/// `sig_find` results on the clean `.text`, by pattern.
static SIG_CACHE: Mutex<Option<HashMap<String, Option<usize>>>> = Mutex::new(None);

/// `n` bytes at `addr` from the clean `.text` copy (None outside `.text` / before the copy).
fn clean_bytes(addr: usize, n: usize) -> Option<&'static [u8]> {
    let clean = crate::game::clean_text()?;
    let text = crate::game::Text::current()?;
    let off = addr.checked_sub(text.text_va)?;
    clean.get(off..off.checked_add(n)?)
}

fn cs(s: &str) -> CString {
    CString::new(s.replace('\0', "")).unwrap_or_default()
}

unsafe fn text_of<'a>(p: *const c_char) -> Option<&'a str> {
    if p.is_null() {
        None
    } else {
        CStr::from_ptr(p).to_str().ok()
    }
}

fn rec(h: *const EvtPlugin) -> Option<(usize, &'static PluginRec)> {
    PLUGINS.get()?.iter().enumerate().find(|(_, p)| std::ptr::eq(&***p, h as *const PluginRec)).map(|(i, p)| (i, &**p))
}

fn id_of(h: *const EvtPlugin) -> &'static str {
    rec(h).map_or("plugin?", |(_, r)| r.spec.mod_id.as_str())
}

/// The loaded plugin that replaces built-in loader module `module` (`provides` names it): its label. The built-in
/// module then stays off.
pub fn replacing_builtin(module: &str) -> Option<String> {
    let l = LOADED.lock().unwrap_or_else(|e| e.into_inner());
    super::replacing(module, &l).map(PluginSpec::label)
}

/// Loaded plugins (init OK), in load order.
pub fn loaded() -> Vec<PluginSpec> {
    LOADED.lock().unwrap_or_else(|e| e.into_inner()).clone()
}

// ---------------------------------------------------------------- API functions

unsafe extern "C" fn api_log(h: *const EvtPlugin, level: i32, msg: *const c_char) {
    let s = if msg.is_null() { String::new() } else { CStr::from_ptr(msg).to_string_lossy().into_owned() };
    let lvl = match level {
        EVT_LOG_ERROR => crate::log::Level::Error,
        EVT_LOG_WARN => crate::log::Level::Warn,
        EVT_LOG_DEBUG => crate::log::Level::Debug,
        EVT_LOG_TRACE => crate::log::Level::Trace,
        _ => crate::log::Level::Info,
    };
    crate::log::write(lvl, format_args!("{}: {s}", id_of(h)));
}

unsafe extern "C" fn api_log_flush() {
    crate::log::flush();
}

unsafe extern "C" fn api_config_get(h: *const EvtPlugin, buf: *mut c_char, cap: usize) -> usize {
    let Some((_, r)) = rec(h) else { return 0 };
    let b = r.config.as_bytes();
    if !buf.is_null() && cap > 0 {
        let n = b.len().min(cap - 1);
        std::ptr::copy_nonoverlapping(b.as_ptr(), buf as *mut u8, n);
        *buf.add(n) = 0;
    }
    b.len()
}

unsafe extern "C" fn api_exe_base() -> usize {
    crate::pe::live::exe_base()
}

unsafe extern "C" fn api_sig_find(h: *const EvtPlugin, name: *const c_char, pattern: *const c_char, rva: u32, out: *mut usize) -> i32 {
    let (Some(p), false) = (text_of(pattern), out.is_null()) else { return EVT_E_ARG };
    if let Err(e) = crate::scan::Pattern::parse(p) {
        error!("{}: sig pattern invalid ({e})", id_of(h));
        return EVT_E_ARG;
    }
    let Some(text) = crate::game::Text::current() else { return EVT_E_STATE };
    let n = text_of(name).map_or_else(|| format!("{}.sig", id_of(h)), str::to_string);
    if let Some(clean) = crate::game::clean_text() {
        // the unpatched copy taken in DllMain: hooks installed since (by anyone) do not hide a pattern
        let key = p.to_string();
        if let Some(v) = SIG_CACHE.lock().unwrap_or_else(|e| e.into_inner()).get_or_insert_with(HashMap::new).get(&key) {
            return v.map_or(EVT_E_NOT_FOUND, |a| {
                *out = a;
                EVT_OK
            });
        }
        let Ok(pat) = crate::scan::Pattern::parse(p) else { return EVT_E_ARG };
        let v = match pat.find_unique(clean) {
            Ok(off) => {
                let a = text.text_va + off;
                let got = (a - text.base) as u32;
                if rva == 0 || got == rva {
                    info!("{}: sig {n} -> RVA 0x{got:X}", id_of(h));
                } else {
                    warn!("{}: sig {n} -> RVA 0x{got:X} (expected 0x{rva:X})", id_of(h));
                }
                Some(a)
            }
            Err(e) => {
                error!("{}: sig {n}: {e}", id_of(h));
                None
            }
        };
        SIG_CACHE.lock().unwrap_or_else(|e| e.into_inner()).get_or_insert_with(HashMap::new).insert(key, v);
        return v.map_or(EVT_E_NOT_FOUND, |a| {
            *out = a;
            EVT_OK
        });
    }
    // leaked: a plugin resolves a handful of signatures once; the cache is keyed by the pattern string
    let sig: &'static crate::sigs::Sig = Box::leak(Box::new(crate::sigs::Sig {
        name: Box::leak(n.into_boxed_str()),
        pattern: Box::leak(p.to_string().into_boxed_str()),
        offset: 0,
        rva,
    }));
    match text.resolve(sig) {
        Some(a) => {
            *out = a;
            EVT_OK
        }
        None => EVT_E_NOT_FOUND,
    }
}

unsafe extern "C" fn api_rip_target(insn: usize, disp_off: u32, next_ip_off: u32, out: *mut usize) -> i32 {
    if out.is_null() {
        return EVT_E_ARG;
    }
    let at = insn + disp_off as usize;
    let d = clean_bytes(at, 4).map(|b| i32::from_le_bytes([b[0], b[1], b[2], b[3]])).or_else(|| crate::game::read::<i32>(at));
    match d {
        Some(d) => {
            *out = (insn as isize + next_ip_off as isize + d as isize) as usize;
            EVT_OK
        }
        None => EVT_E_FAULT,
    }
}

unsafe extern "C" fn api_mem_read(addr: usize, dst: *mut c_void, n: usize) -> i32 {
    if dst.is_null() || addr < 0x10000 {
        return if dst.is_null() { EVT_E_ARG } else { EVT_E_FAULT };
    }
    if evt_seh_read(addr, dst as *mut u8, n) == 0 {
        EVT_OK
    } else {
        EVT_E_FAULT
    }
}

unsafe extern "C" fn api_mem_write(addr: usize, src: *const c_void, n: usize) -> i32 {
    if src.is_null() || addr < 0x10000 {
        return if src.is_null() { EVT_E_ARG } else { EVT_E_FAULT };
    }
    if evt_seh_write(addr, src as *const u8, n) == 0 {
        EVT_OK
    } else {
        EVT_E_FAULT
    }
}

unsafe extern "C" fn api_call_guarded(func: usize, args: *const u64, nargs: u32, ret: *mut u64) -> i32 {
    if func < 0x10000 || nargs > 6 || (nargs > 0 && args.is_null()) {
        return EVT_E_ARG;
    }
    let mut a = [0u64; 6];
    for (k, v) in a.iter_mut().enumerate().take(nargs as usize) {
        *v = *args.add(k);
    }
    match crate::game::seh_call6(func, a[0], a[1], a[2], a[3], a[4], a[5]) {
        Ok(r) => {
            if !ret.is_null() {
                *ret = r;
            }
            EVT_OK
        }
        Err(_) => EVT_E_FAULT,
    }
}

unsafe extern "C" fn api_hook_inline(
    h: *const EvtPlugin,
    target: usize,
    prologue: *const u8,
    len: usize,
    detour: *const c_void,
    next: *mut usize,
    priority: i32,
) -> i32 {
    let Some((_, r)) = rec(h) else { return EVT_E_ARG };
    let id = r.spec.mod_id.as_str();
    if target < 0x10000 || detour.is_null() || next.is_null() || (len > 0 && prologue.is_null()) {
        error!("{id}: hook_inline: bad arguments");
        return EVT_E_ARG;
    }
    let expected: &[u8] = if len == 0 { &[] } else { std::slice::from_raw_parts(prologue, len) };
    let cell = &*(next as *const AtomicUsize);
    let rva = target.wrapping_sub(crate::pe::live::exe_base());
    match crate::hook::chain_hook(target, expected, detour as usize, cell, priority, 1 + r.spec.load_index, id) {
        Ok(_) => {
            let chain: Vec<String> = crate::hook::chain_of(target).into_iter().map(|l| l.owner).collect();
            info!("{id}: hook RVA 0x{rva:X} installed (priority {priority}; chain, first runs first: {})", chain.join(" > "));
            EVT_OK
        }
        Err(e) => {
            error!("{id}: hook RVA 0x{rva:X} failed: {e}");
            if e.contains("already hooked by") {
                EVT_E_CONFLICT
            } else {
                EVT_E_HOOK
            }
        }
    }
}

unsafe extern "C" fn api_hook_iat(
    h: *const EvtPlugin,
    module: *const c_char,
    dll: *const c_char,
    func: *const c_char,
    detour: *const c_void,
    orig: *mut usize,
) -> i32 {
    let Some((_, r)) = rec(h) else { return EVT_E_ARG };
    let id = r.spec.mod_id.as_str();
    let (Some(d), Some(f)) = (text_of(dll), text_of(func)) else { return EVT_E_ARG };
    if detour.is_null() || orig.is_null() {
        return EVT_E_ARG;
    }
    let base = if module.is_null() { crate::pe::live::exe_base() } else { GetModuleHandleA(module as *const u8) as usize };
    if base == 0 {
        error!("{id}: hook_iat: module {:?} not loaded", text_of(module));
        return EVT_E_NOT_FOUND;
    }
    let Some(slot) = crate::pe::live::iat_slot(base, d, f) else {
        error!("{id}: hook_iat: {d}!{f} not imported");
        return EVT_E_NOT_FOUND;
    };
    let cell = &*(orig as *const AtomicUsize);
    if crate::hook::iat_hook(base, d, f, detour as usize, cell) {
        r.iat.lock().unwrap_or_else(|e| e.into_inner()).push((slot as usize, detour as usize, orig as usize));
        info!("{id}: IAT {d}!{f} hooked");
        EVT_OK
    } else {
        error!("{id}: IAT {d}!{f} hook failed");
        EVT_E_HOOK
    }
}

/// Dispatcher of every plugin Lua command (a plain loader handler; finds the plugin by the command hash).
fn plugin_cmd(c: &mut Call) {
    let h = c.hash();
    let e = CMDS.lock().unwrap_or_else(|e| e.into_inner()).as_ref().and_then(|m| m.get(&h).copied());
    if let Some((f, user, _)) = e {
        unsafe { f(c as *mut Call as *mut EvtLuaCall, user as *mut c_void) };
    }
}

unsafe fn register_cmd(h: *const EvtPlugin, hash: u32, name: String, f: EvtLuaHandler, user: *mut c_void) -> i32 {
    let Some((idx, r)) = rec(h) else { return EVT_E_ARG };
    let id = r.spec.mod_id.as_str();
    if !crate::runtime::ctx().is_some_and(|c| c.cfg.modules.lua_bridge) {
        warn!("{id}: Lua command {name} not registered: [modules] lua_bridge is off");
        return EVT_E_STATE;
    }
    if crate::lua::activated() {
        error!("{id}: Lua command {name} not registered: only during evt_plugin_init");
        return EVT_E_STATE;
    }
    if crate::lua::is_registered(hash) {
        error!("{id}: Lua command {name} (0x{hash:08X}) is already registered by the loader or another plugin");
        return EVT_E_CONFLICT;
    }
    CMDS.lock().unwrap_or_else(|e| e.into_inner()).get_or_insert_with(HashMap::new).insert(hash, (f, user as usize, idx));
    r.cmds.lock().unwrap_or_else(|e| e.into_inner()).push(hash);
    crate::lua::register_hash(hash, Box::leak(name.into_boxed_str()), plugin_cmd);
    EVT_OK
}

unsafe extern "C" fn api_lua_register(h: *const EvtPlugin, name: *const c_char, f: EvtLuaHandler, user: *mut c_void) -> i32 {
    let Some(n) = text_of(name).filter(|n| !n.is_empty()) else { return EVT_E_ARG };
    register_cmd(h, crate::lua::hash(n), n.to_string(), f, user)
}

unsafe extern "C" fn api_lua_register_hash(h: *const EvtPlugin, hash: u32, label: *const c_char, f: EvtLuaHandler, user: *mut c_void) -> i32 {
    let n = text_of(label).filter(|n| !n.is_empty()).map_or_else(|| format!("0x{hash:08X}"), str::to_string);
    register_cmd(h, hash, n, f, user)
}

unsafe fn call<'a>(c: *mut EvtLuaCall) -> Option<&'a mut Call> {
    (c as *mut Call).as_mut()
}

unsafe extern "C" fn api_lua_nargs(c: *mut EvtLuaCall) -> i32 {
    call(c).map_or(0, |c| c.nargs)
}
unsafe extern "C" fn api_lua_arg_type(c: *mut EvtLuaCall, i: i32) -> i32 {
    call(c).map_or(EVT_LUA_NONE, |c| c.arg_type(i))
}
unsafe extern "C" fn api_lua_arg_num(c: *mut EvtLuaCall, i: i32, out: *mut f64) -> i32 {
    match (call(c).and_then(|c| c.num(i)), out.is_null()) {
        (Some(v), false) => {
            *out = v;
            EVT_OK
        }
        (_, true) => EVT_E_ARG,
        (None, _) => EVT_E_NOT_FOUND,
    }
}
unsafe extern "C" fn api_lua_arg_str(c: *mut EvtLuaCall, i: i32, buf: *mut c_char, cap: usize) -> isize {
    let Some(s) = call(c).and_then(|c| c.string(i)) else { return -1 };
    let b = s.as_bytes();
    if !buf.is_null() && cap > 0 {
        let n = b.len().min(cap - 1);
        std::ptr::copy_nonoverlapping(b.as_ptr(), buf as *mut u8, n);
        *buf.add(n) = 0;
    }
    b.len() as isize
}
unsafe extern "C" fn api_lua_push_num(c: *mut EvtLuaCall, v: f64) {
    if let Some(c) = call(c) {
        c.push_num(v);
    }
}
unsafe extern "C" fn api_lua_push_bool(c: *mut EvtLuaCall, v: i32) {
    if let Some(c) = call(c) {
        c.push_bool(v != 0);
    }
}
unsafe extern "C" fn api_lua_push_str(c: *mut EvtLuaCall, s: *const c_char) {
    if let (Some(c), false) = (call(c), s.is_null()) {
        c.push_str(&CStr::from_ptr(s).to_string_lossy());
    }
}

fn fill(m: &ModRec, out: *mut EvtModInfo) -> i32 {
    if out.is_null() {
        return EVT_E_ARG;
    }
    unsafe {
        *out = EvtModInfo {
            id: m.id.as_ptr(),
            version: m.version.as_ptr(),
            dir: m.dir.as_ptr(),
            plugin: m.plugin.as_ptr(),
            load_index: m.load_index,
            plugin_state: m.state.load(Ordering::Acquire),
        };
    }
    EVT_OK
}

unsafe extern "C" fn api_mod_count() -> u32 {
    MODS.get().map_or(0, |m| m.len() as u32)
}
unsafe extern "C" fn api_mod_get(index: u32, out: *mut EvtModInfo) -> i32 {
    MODS.get().and_then(|m| m.get(index as usize)).map_or(EVT_E_NOT_FOUND, |m| fill(m, out))
}
unsafe extern "C" fn api_mod_find(id: *const c_char, out: *mut EvtModInfo) -> i32 {
    let Some(id) = text_of(id) else { return EVT_E_ARG };
    MODS.get().and_then(|m| m.iter().find(|x| x.id.to_str() == Ok(id))).map_or(EVT_E_NOT_FOUND, |m| fill(m, out))
}
unsafe extern "C" fn api_provider_find(name: *const c_char, out: *mut EvtModInfo, version: *mut *const c_char) -> i32 {
    let Some(n) = text_of(name) else { return EVT_E_ARG };
    let Some(mods) = MODS.get() else { return EVT_E_NOT_FOUND };
    for m in mods.iter().rev() {
        if let Some((_, v)) = m.provides.iter().find(|(p, _)| p == n) {
            if !version.is_null() {
                *version = v.as_ptr();
            }
            return fill(m, out);
        }
    }
    EVT_E_NOT_FOUND
}

unsafe extern "C" fn api_game_state(key: *const c_char, out: *mut i64) -> i32 {
    let (Some(k), false) = (text_of(key), out.is_null()) else { return EVT_E_ARG };
    let v = match k {
        "match.soccer_mode" => crate::match_state::soccer_mode().map(i64::from),
        "match.in_match" => Some(crate::match_state::in_match() as i64),
        "loader.modules_mask" => Some(crate::runtime::MODULES.load(Ordering::Acquire) as i64),
        _ => None,
    };
    match v {
        Some(v) => {
            *out = v;
            EVT_OK
        }
        None => EVT_E_NOT_FOUND,
    }
}

unsafe extern "C" fn api_thread_spawn(h: *const EvtPlugin, name: *const c_char, f: EvtThreadFn, user: *mut c_void) -> i32 {
    let Some((_, r)) = rec(h) else { return EVT_E_ARG };
    let n = format!("plugin-{}-{}", r.spec.mod_id, text_of(name).unwrap_or("thread"));
    let u = user as usize;
    match std::thread::Builder::new().name(n.clone()).spawn(move || unsafe { f(u as *mut c_void) }) {
        Ok(_) => EVT_OK,
        Err(e) => {
            error!("{}: thread {n} not started: {e}", r.spec.mod_id);
            EVT_E_STATE
        }
    }
}

unsafe extern "C" fn api_code_read_clean(addr: usize, dst: *mut c_void, n: usize) -> i32 {
    if dst.is_null() {
        return EVT_E_ARG;
    }
    match clean_bytes(addr, n) {
        Some(b) => {
            std::ptr::copy_nonoverlapping(b.as_ptr(), dst as *mut u8, n);
            EVT_OK
        }
        None => EVT_E_NOT_FOUND,
    }
}

unsafe extern "C" fn api_hook_ptr(h: *const EvtPlugin, slot: usize, detour: *const c_void, orig: *mut usize) -> i32 {
    let Some((_, r)) = rec(h) else { return EVT_E_ARG };
    let id = r.spec.mod_id.as_str();
    if slot < 0x10000 || detour.is_null() || orig.is_null() {
        return EVT_E_ARG;
    }
    let Some(cur) = crate::game::read::<usize>(slot) else {
        error!("{id}: hook_ptr: slot 0x{slot:X} not readable");
        return EVT_E_FAULT;
    };
    (*(orig as *const AtomicUsize)).store(cur, Ordering::Release);
    match crate::hook::patch_ptr(slot as *mut usize, detour as usize) {
        Some(_) => {
            r.iat.lock().unwrap_or_else(|e| e.into_inner()).push((slot, detour as usize, orig as usize));
            info!("{id}: pointer slot RVA 0x{:X} hooked", slot.wrapping_sub(crate::pe::live::exe_base()));
            EVT_OK
        }
        None => {
            error!("{id}: hook_ptr: slot 0x{slot:X} not writable");
            EVT_E_HOOK
        }
    }
}

fn dirs() -> Option<(std::path::PathBuf, std::path::PathBuf)> {
    DIRS.get().cloned().or_else(|| crate::runtime::ctx().map(|c| (c.game_dir.clone(), c.data_dir.clone())))
}

/// Copy `s` into a caller buffer (`cap - 1` bytes + NUL at most); returns the full length.
unsafe fn out_str(s: &str, buf: *mut c_char, cap: usize) -> usize {
    let b = s.as_bytes();
    if !buf.is_null() && cap > 0 {
        let n = b.len().min(cap - 1);
        std::ptr::copy_nonoverlapping(b.as_ptr(), buf as *mut u8, n);
        *buf.add(n) = 0;
    }
    b.len()
}

unsafe extern "C" fn api_path_get(key: *const c_char, buf: *mut c_char, cap: usize) -> usize {
    let Some(k) = text_of(key) else { return 0 };
    let Some((game, data)) = dirs() else { return 0 };
    let p = match k {
        "game_dir" => game,
        "loader_dir" => data,
        "mods_dir" => game.join(crate::mods::MODS_DIR),
        // (29/09) generated files of plugins: `<game>\evt_loader\cache` (below the data root, so file_serve accepts it)
        "cache_dir" => data.join("cache"),
        _ => return 0,
    };
    out_str(&p.to_string_lossy(), buf, cap)
}

// ---------------------------------------------------------------- phase, file_serve, game_file_path (appended 29/09)

/// Where the plugins are (`EVT_PHASE_*`, `evt_plugin_sdk::abi`).
static PHASE: AtomicU32 = AtomicU32::new(EVT_PHASE_NONE);

unsafe extern "C" fn api_phase() -> u32 {
    PHASE.load(Ordering::Acquire)
}

/// `file_serve`: the overlay also serves `game_path` from `disk_path` (a file under the game folder, e.g. in
/// `cache_dir`). Only while the overlay is still open: the early phase at the exe entry point, before the engine opens
/// its first file.
unsafe extern "C" fn api_file_serve(h: *const EvtPlugin, game_path: *const c_char, disk_path: *const c_char) -> i32 {
    let Some((_, r)) = rec(h) else { return EVT_E_ARG };
    let id = r.spec.mod_id.as_str();
    let (Some(gp), Some(dp)) = (text_of(game_path), text_of(disk_path)) else { return EVT_E_ARG };
    let Some(key) = evt_modfmt::normalize_key(gp) else {
        error!("{id}: file_serve: {gp:?} is not a game path (data/...)");
        return EVT_E_ARG;
    };
    let path = std::path::PathBuf::from(dp);
    let size = match std::fs::metadata(&path) {
        Ok(m) if m.is_file() && m.len() > 0 => m.len(),
        _ => {
            error!("{id}: file_serve {key}: {dp} is not a non-empty file");
            return EVT_E_NOT_FOUND;
        }
    };
    let under_game = dirs().is_some_and(|(g, _)| {
        let (a, b) = (path.to_string_lossy().replace('\\', "/").to_ascii_lowercase(), g.to_string_lossy().replace('\\', "/").to_ascii_lowercase());
        a.starts_with(&format!("{}/", b.trim_end_matches('/')))
    });
    if !under_game {
        error!("{id}: file_serve {key}: {dp} is not below the game folder (use path_get(\"cache_dir\"))");
        return EVT_E_ARG;
    }
    let Some(cpath) = crate::mods::c_path(&path) else {
        error!("{id}: file_serve {key}: path not usable by the engine (ASCII, < 256 bytes)");
        return EVT_E_ARG;
    };
    match crate::mods::hooks::serve_extra(&key, crate::mods::Redirect { module: format!("{id} (plugin)"), cpath, size }) {
        Ok(prev) => {
            let over = prev.map(|p| format!(", over {p}")).unwrap_or_default();
            info!("{id}: file_serve {key} <- {dp} ({size} B{over})");
            EVT_OK
        }
        Err(e) => {
            error!("{id}: file_serve {key}: {e}");
            if e.contains("served by") {
                EVT_E_CONFLICT
            } else {
                EVT_E_STATE
            }
        }
    }
}

/// Installed game (cpk_list + data), read once: base of `game_file_path`.
static GAME_SRC: Mutex<Option<crate::mods::merge::GameSource>> = Mutex::new(None);

/// `game_file_path`: a file on disk with the bytes the GAME itself has for `game_path` (installed loose file or,
/// for a file inside a CPK, a copy extracted to `evt_loader\cache\game\...`, re-extracted when the CPK changes). The
/// mods overlay is not applied. 0 = not found.
unsafe extern "C" fn api_game_file_path(game_path: *const c_char, buf: *mut c_char, cap: usize) -> usize {
    let Some(gp) = text_of(game_path) else { return 0 };
    let Some(key) = evt_modfmt::normalize_key(gp) else { return 0 };
    let Some((game, data)) = dirs() else { return 0 };
    let res = (|| -> Result<std::path::PathBuf, String> {
        let mut g = GAME_SRC.lock().unwrap_or_else(|e| e.into_inner());
        let src = g.get_or_insert_with(|| crate::mods::merge::GameSource::new(&game));
        if let Some(p) = src.loose_path(&key)? {
            return Ok(p);
        }
        let out = data.join("cache").join("game").join(key.replace('/', std::path::MAIN_SEPARATOR_STR));
        let stamp = out.with_extension(format!("{}.stamp", out.extension().and_then(|e| e.to_str()).unwrap_or("")));
        let deps = src.cpk_stamp(&key)?;
        if out.is_file() && std::fs::read_to_string(&stamp).is_ok_and(|s| s == deps) {
            return Ok(out);
        }
        let (bytes, _) = crate::mods::merge::Source::read(src, &key)?;
        std::fs::create_dir_all(out.parent().unwrap()).map_err(|e| e.to_string())?;
        std::fs::write(&out, &bytes).and_then(|_| std::fs::write(&stamp, &deps)).map_err(|e| format!("{}: {e}", out.display()))?;
        info!("plugins: game_file_path {key}: extracted from its CPK to {} ({} B)", out.display(), bytes.len());
        Ok(out)
    })();
    match res {
        Ok(p) => out_str(&p.to_string_lossy(), buf, cap),
        Err(e) => {
            debug!("plugins: game_file_path {key}: {e}");
            0
        }
    }
}

/// The API table (built once, lives forever).
pub fn api() -> &'static EvtApi {
    let lv = LOADER_VERSION_C.get_or_init(|| cs(crate::MODLOADER_VERSION));
    &API.get_or_init(|| {
        SyncApi(EvtApi {
            api_version: EVT_PLUGIN_API_VERSION,
            size: std::mem::size_of::<EvtApi>() as u32,
            loader_version: lv.as_ptr(),
            log: api_log,
            log_flush: api_log_flush,
            config_get: api_config_get,
            exe_base: api_exe_base,
            sig_find: api_sig_find,
            rip_target: api_rip_target,
            mem_read: api_mem_read,
            mem_write: api_mem_write,
            call_guarded: api_call_guarded,
            hook_inline: api_hook_inline,
            hook_iat: api_hook_iat,
            lua_register: api_lua_register,
            lua_register_hash: api_lua_register_hash,
            lua_nargs: api_lua_nargs,
            lua_arg_type: api_lua_arg_type,
            lua_arg_num: api_lua_arg_num,
            lua_arg_str: api_lua_arg_str,
            lua_push_num: api_lua_push_num,
            lua_push_bool: api_lua_push_bool,
            lua_push_str: api_lua_push_str,
            mod_count: api_mod_count,
            mod_get: api_mod_get,
            mod_find: api_mod_find,
            provider_find: api_provider_find,
            game_state: api_game_state,
            thread_spawn: api_thread_spawn,
            code_read_clean: api_code_read_clean,
            hook_ptr: api_hook_ptr,
            path_get: api_path_get,
            file_serve: api_file_serve,
            phase: api_phase,
            game_file_path: api_game_file_path,
        })
    })
    .0
}

// ---------------------------------------------------------------- loading

/// Undo what a failed plugin registered: (hooks, IAT patches, Lua commands).
fn rollback(r: &PluginRec) -> (usize, usize, usize) {
    let hooks = crate::hook::chain_remove_owner(&r.spec.mod_id);
    let mut iat = 0;
    for (slot, detour, orig) in r.iat.lock().unwrap_or_else(|e| e.into_inner()).drain(..) {
        unsafe {
            let cur = std::ptr::read_volatile(slot as *const usize);
            let prev = (*(orig as *const AtomicUsize)).load(Ordering::Acquire);
            if cur == detour && prev != 0 && crate::hook::patch_ptr(slot as *mut usize, prev).is_some() {
                iat += 1;
            }
        }
    }
    let mut cmds = 0;
    for h in r.cmds.lock().unwrap_or_else(|e| e.into_inner()).drain(..) {
        CMDS.lock().unwrap_or_else(|e| e.into_inner()).as_mut().map(|m| m.remove(&h));
        if crate::lua::unregister_pending(h) {
            cmds += 1;
        }
    }
    (hooks, iat, cmds)
}

fn proc(hm: *mut c_void, name: &str) -> usize {
    let c = cs(name);
    unsafe { GetProcAddress(hm as _, c.as_ptr() as *const u8) }.map_or(0, |f| f as usize)
}

fn info_of(r: &'static PluginRec) -> Option<EvtPluginInfo> {
    let m = MODS.get()?.get(r.mod_index)?;
    Some(EvtPluginInfo {
        size: std::mem::size_of::<EvtPluginInfo>() as u32,
        load_index: r.spec.load_index,
        handle: r as *const PluginRec as *const EvtPlugin,
        mod_id: m.id.as_ptr(),
        mod_version: m.version.as_ptr(),
        mod_dir: m.dir.as_ptr(),
        loader_version: LOADER_VERSION_C.get_or_init(|| cs(crate::MODLOADER_VERSION)).as_ptr(),
    })
}

/// LoadLibrary + exports + API version (once). Err = why the plugin is disabled.
fn load_dll(r: &'static PluginRec) -> Result<(), String> {
    if r.hmodule.load(Ordering::Acquire) != 0 {
        return Ok(());
    }
    let path = r.spec.path();
    if !path.is_file() {
        return Err(format!("{} not found in the mod folder", r.spec.dll));
    }
    let wide: Vec<u16> = path.as_os_str().encode_wide().chain(std::iter::once(0)).collect();
    // altered search path: the plugin's own dependencies are looked up next to it first
    let hm = unsafe { LoadLibraryExW(wide.as_ptr(), std::ptr::null_mut(), LOAD_WITH_ALTERED_SEARCH_PATH) } as *mut c_void;
    if hm.is_null() {
        return Err(format!("LoadLibrary failed (Windows error {})", unsafe { GetLastError() }));
    }
    r.hmodule.store(hm as usize, Ordering::Release);
    let (fv, fi) = (proc(hm, EXPORT_API_VERSION), proc(hm, EXPORT_INIT));
    if fv == 0 || fi == 0 {
        return Err(format!("not a ModLoader plugin (exports {EXPORT_API_VERSION} / {EXPORT_INIT} missing)"));
    }
    let v = crate::game::seh_call(fv, 0, 0, 0, 0, 0).map_err(|c| format!("{EXPORT_API_VERSION} raised 0x{c:08X}"))? as u32;
    check_api_version(v)?;
    info!("plugins: {}: loaded (plugin API v{v})", r.spec.label());
    Ok(())
}

/// Call `export` (`evt_plugin_early` / `evt_plugin_init`) under SEH. Ok(None) = the optional export is absent.
fn call_phase(r: &'static PluginRec, export: &str) -> Result<Option<u128>, String> {
    let f = proc(r.hmodule.load(Ordering::Acquire) as *mut c_void, export);
    if f == 0 {
        return Ok(None);
    }
    let info = info_of(r).ok_or("mod list missing")?;
    debug!("plugins: {}: calling {export}", r.spec.label());
    let t0 = Instant::now();
    match crate::game::seh_call(f, api() as *const EvtApi as u64, &info as *const EvtPluginInfo as u64, 0, 0, 0) {
        Ok(rc) if rc as u32 as i32 == EVT_OK => Ok(Some(t0.elapsed().as_millis())),
        Ok(rc) => Err(format!("{export} returned {}", rc as u32 as i32)),
        Err(c) => Err(format!("{export} raised exception 0x{c:08X}")),
    }
}

/// Disable a plugin: state FAILED, rollback, its shutdown export.
fn disable(r: &'static PluginRec, why: &str) {
    if let Some(m) = MODS.get().and_then(|m| m.get(r.mod_index)) {
        m.state.store(EVT_PLUGIN_FAILED, Ordering::Release);
    }
    let (h, i, c) = rollback(r);
    error!(
        "plugins: {}: {why}: plugin DISABLED, the game continues without it (removed {h} hook(s), {i} pointer / IAT patch(es), {c} Lua command(s))",
        r.spec.label()
    );
    let hm = r.hmodule.load(Ordering::Acquire);
    if hm != 0 {
        // the DLL stays loaded (threads it started may still run); only its shutdown export is called
        let sd = proc(hm as *mut c_void, EXPORT_SHUTDOWN);
        if sd != 0 && crate::game::seh_call(sd, 0, 0, 0, 0, 0).is_err() {
            error!("plugins: {}: {EXPORT_SHUTDOWN} raised an exception", r.spec.label());
        }
    }
}

fn failed(r: &PluginRec) -> bool {
    MODS.get().and_then(|m| m.get(r.mod_index)).is_some_and(|m| m.state.load(Ordering::Acquire) == EVT_PLUGIN_FAILED)
}

/// Build the mod list and the plugin records of `plan` (once; DllMain or the init thread). false = nothing to load.
fn prepare(plan: &evt_modfmt::LoadPlan, data_dir: &std::path::Path) -> bool {
    if MODS.get().is_some() {
        return PLUGINS.get().is_some_and(|p| !p.is_empty());
    }
    let mods: Vec<ModRec> = plan
        .mods
        .iter()
        .enumerate()
        .map(|(i, m)| {
            let mut provides = vec![(m.manifest.id.clone(), cs(&m.manifest.version))];
            for p in &m.manifest.provides {
                if let Ok((n, v)) = evt_modfmt::parse_provide(p) {
                    provides.push((n, cs(v.as_deref().unwrap_or(&m.manifest.version))));
                }
            }
            ModRec {
                id: cs(&m.manifest.id),
                version: cs(&m.manifest.version),
                dir: cs(&m.dir.to_string_lossy()),
                plugin: cs(&m.manifest.plugin),
                load_index: i as u32,
                state: AtomicU32::new(if m.manifest.plugin.is_empty() { EVT_PLUGIN_NONE } else { EVT_PLUGIN_PENDING }),
                provides,
            }
        })
        .collect();
    let _ = MODS.set(mods);
    let _ = DIRS.set((
        data_dir.parent().map(std::path::Path::to_path_buf).unwrap_or_default(),
        data_dir.to_path_buf(),
    ));
    let loader_cfg = std::fs::read_to_string(data_dir.join("config.toml")).ok();
    let recs: Vec<Box<PluginRec>> = super::specs(plan)
        .into_iter()
        .map(|s| {
            let own = std::fs::read_to_string(s.dir.join("config.toml")).ok();
            let (config, problems) = mod_config(&s.mod_id, own.as_deref(), loader_cfg.as_deref());
            for p in problems {
                warn!("plugins: {}: {p}", s.mod_id);
            }
            Box::new(PluginRec {
                mod_index: s.load_index as usize,
                spec: s,
                config,
                iat: Mutex::new(Vec::new()),
                cmds: Mutex::new(Vec::new()),
                hmodule: AtomicUsize::new(0),
            })
        })
        .collect();
    if recs.is_empty() {
        let _ = PLUGINS.set(recs);
        info!("plugins: no active mod ships a plugin");
        return false;
    }
    let list: Vec<String> = recs.iter().map(|r| r.spec.label()).collect();
    info!("plugins: {} plugin(s) in load order: {}", recs.len(), list.join(", "));
    warn!("plugins: a plugin is native code with full access to the game and the PC: install only mods you trust");
    let _ = PLUGINS.set(recs);
    true
}

// ---------------------------------------------------------------- early phase (game main thread, CRT entry point)

/// Game folder and `evt_loader` (for `path_get`).
static DIRS: OnceLock<(std::path::PathBuf, std::path::PathBuf)> = OnceLock::new();
/// The early phase ran (or will never run): the init thread may go on.
static EARLY_DONE: Mutex<bool> = Mutex::new(false);
static EARLY_CV: Condvar = Condvar::new();
/// The entry-point trigger fired (first call only).
static EARLY_FIRED: AtomicBool = AtomicBool::new(false);
/// Entry point address and its original first bytes while the one-shot jump is in place.
static ENTRY_SAVED: Mutex<Option<(usize, [u8; super::ENTRY_PATCH_LEN])>> = Mutex::new(None);
/// How long the init thread waits for the entry-point trigger before loading the plugins itself.
const EARLY_WAIT: std::time::Duration = std::time::Duration::from_secs(2);

fn early_finished() {
    *EARLY_DONE.lock().unwrap_or_else(|e| e.into_inner()) = true;
    EARLY_CV.notify_all();
}

/// Early phase: load every plugin DLL and call its `evt_plugin_early` (if exported), in load order.
fn early_phase() {
    let recs = PLUGINS.get().map(|v| v.as_slice()).unwrap_or(&[]);
    for r in recs {
        if let Err(why) = load_dll(r) {
            disable(r, &why);
            continue;
        }
        match call_phase(r, EXPORT_EARLY) {
            Ok(Some(ms)) => info!("plugins: {}: early phase OK in {ms} ms", r.spec.label()),
            Ok(None) => {}
            Err(why) => disable(r, &why),
        }
    }
    early_finished();
}

/// The exe entry point, reached once through the one-shot jump: put the original bytes back, run the early phase
/// (main thread, loader lock released, before any game code) and continue into the original entry.
unsafe extern "system" fn entry_detour(arg: usize) -> u32 {
    let saved = ENTRY_SAVED.lock().unwrap_or_else(|e| e.into_inner()).take();
    let Some((addr, bytes)) = saved else {
        // cannot happen (the jump is only written with the bytes saved): nothing to return to
        std::process::abort();
    };
    let restored = crate::hook::write_code(addr, &bytes);
    if !EARLY_FIRED.swap(true, Ordering::AcqRel) {
        let t0 = Instant::now();
        info!("plugins: early phase: exe entry point reached (game main thread, before any game code)");
        PHASE.store(EVT_PHASE_EARLY, Ordering::Release);
        if std::panic::catch_unwind(early_phase).is_err() {
            error!("plugins: early phase panicked");
            early_finished();
        }
        // the game's own code runs from here on (the plugins' init comes later, in the init thread)
        PHASE.store(EVT_PHASE_RUNNING, Ordering::Release);
        info!("plugins: early phase done at the entry point in {} ms", t0.elapsed().as_millis());
    }
    if !restored {
        error!("plugins: entry point bytes NOT restored: the game cannot start");
    }
    let f: unsafe extern "system" fn(usize) -> u32 = std::mem::transmute(addr);
    f(arg)
}

/// DllMain: build the plugin list of `plan` and arm the early phase: a one-shot absolute jump on nie.exe's entry
/// point (`AddressOfEntryPoint`, checked to have the MSVC `mainCRTStartup` shape). Not armed = the init thread
/// loads the plugins itself (`evt_plugin_early` runs late).
pub fn arm_early(plan: &evt_modfmt::LoadPlan, data_dir: &std::path::Path) {
    if !prepare(plan, data_dir) {
        early_finished();
        return;
    }
    let base = crate::pe::live::exe_base();
    let hdr = unsafe { std::slice::from_raw_parts(base as *const u8, 0x1000) };
    let armed = (|| {
        let rva = super::entry_point_rva(hdr)?;
        let addr = base + rva as usize;
        let code = crate::game::read::<[u8; 18]>(addr)?;
        if !super::entry_shape_ok(&code) {
            warn!("plugins: entry point RVA 0x{rva:X} has an unexpected shape {code:02X?}: early phase not armed");
            return None;
        }
        let mut saved = [0u8; super::ENTRY_PATCH_LEN];
        saved.copy_from_slice(&code[..super::ENTRY_PATCH_LEN]);
        *ENTRY_SAVED.lock().unwrap_or_else(|e| e.into_inner()) = Some((addr, saved));
        let mut jmp = [0u8; super::ENTRY_PATCH_LEN];
        jmp[0] = 0xFF;
        jmp[1] = 0x25; // jmp qword ptr [rip+0]
        jmp[6..14].copy_from_slice(&(entry_detour as *const () as u64).to_le_bytes());
        if !unsafe { crate::hook::write_code(addr, &jmp) } {
            ENTRY_SAVED.lock().unwrap_or_else(|e| e.into_inner()).take();
            return None;
        }
        Some(rva)
    })();
    match armed {
        Some(rva) => info!("plugins: early phase armed (one-shot jump on the nie.exe entry point, RVA 0x{rva:X})"),
        None => {
            warn!("plugins: early phase not armed: plugins load in the init thread, evt_plugin_early runs late");
            early_finished();
        }
    }
}

// ---------------------------------------------------------------- init phase (init thread)

/// Init thread: after the early phase (waited for, [`EARLY_WAIT`] at most), load what is not loaded yet and call every
/// plugin's `evt_plugin_init` in load order (`data_dir` = `evt_loader`, for `[mods.<id>]`).
pub fn load_all(plan: &evt_modfmt::LoadPlan, data_dir: &std::path::Path) {
    let first = MODS.get().is_none();
    if !prepare(plan, data_dir) {
        return;
    }
    if first {
        // arm_early did not run (no DllMain plan): nothing to wait for
        early_finished();
    }
    {
        let d = EARLY_DONE.lock().unwrap_or_else(|e| e.into_inner());
        let (d, t) = EARLY_CV.wait_timeout_while(d, EARLY_WAIT, |done| !*done).unwrap_or_else(|e| e.into_inner());
        if t.timed_out() && !*d {
            if EARLY_FIRED.load(Ordering::Acquire) {
                // the early phase is running at the entry point (a plugin generating files): never init a plugin
                // while its own early phase still runs
                info!("plugins: early phase still running at the entry point: waiting for it");
                let _d = EARLY_CV.wait_while(d, |done| !*done).unwrap_or_else(|e| e.into_inner());
            } else {
                warn!("plugins: early phase did not run within {} s: FALLBACK, loading the plugins from the init thread", EARLY_WAIT.as_secs());
            }
        }
    }
    // a trigger that fires after this point does nothing (no second early phase)
    let late_early = !EARLY_FIRED.swap(true, Ordering::AcqRel);
    if late_early {
        info!("plugins: path: init thread only (no early phase at the entry point)");
    } else {
        info!("plugins: path: early phase at the entry point, now evt_plugin_init in the init thread");
    }
    let recs = PLUGINS.get().map(|v| v.as_slice()).unwrap_or(&[]);
    for r in recs {
        if failed(r) {
            continue;
        }
        if let Err(why) = load_dll(r) {
            disable(r, &why);
            continue;
        }
        if late_early {
            // no early phase happened: its export still runs, first (late: the game is already running)
            PHASE.store(EVT_PHASE_EARLY_LATE, Ordering::Release);
            match call_phase(r, EXPORT_EARLY) {
                Ok(Some(ms)) => warn!("plugins: {}: early phase run LATE (init thread) in {ms} ms", r.spec.label()),
                Ok(None) => {}
                Err(why) => {
                    disable(r, &why);
                    continue;
                }
            }
        }
        PHASE.store(EVT_PHASE_INIT, Ordering::Release);
        match call_phase(r, EXPORT_INIT) {
            Ok(ms) => {
                if let Some(m) = MODS.get().and_then(|m| m.get(r.mod_index)) {
                    m.state.store(EVT_PLUGIN_LOADED, Ordering::Release);
                }
                LOADED.lock().unwrap_or_else(|e| e.into_inner()).push(r.spec.clone());
                let prov = if r.spec.provides.is_empty() { String::new() } else { format!("; provides {}", r.spec.provides.join(", ")) };
                info!("plugins: {}: init OK in {} ms{prov}", r.spec.label(), ms.unwrap_or(0));
            }
            Err(why) => disable(r, &why),
        }
    }
    PHASE.store(EVT_PHASE_RUNNING, Ordering::Release);
    let ok = loaded();
    let names: Vec<String> = ok.iter().map(PluginSpec::label).collect();
    info!("plugins: {}/{} loaded: {}", ok.len(), recs.len(), if names.is_empty() { "none".into() } else { names.join(", ") });
}

#[cfg(test)]
mod tests {
    use super::*;

    /// End to end through the C ABI with a real plugin DLL (set `EVT_TEST_PLUGIN_DLL` to a built `quit_fix.dll`;
    /// skipped otherwise). In the test process nie.exe's signatures do not exist, so the plugin's init fails: the
    /// loader must load it, call it through the API table, log its lines, disable it and restore everything.
    #[test]
    fn real_plugin_failing_init_is_isolated() {
        let Ok(dll) = std::env::var("EVT_TEST_PLUGIN_DLL") else { return };
        let root = std::env::temp_dir().join(format!("evt_plugin_host_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let d = root.join("mods").join("quit_fix");
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(d.join("mod.toml"), "id=\"quit_fix\"\nversion=\"1.0.0\"\nplugin=\"quit_fix.dll\"\nprovides=[\"quit_fix\"]\nloader_min=\"1.0.0\"\n").unwrap();
        std::fs::copy(&dll, d.join("quit_fix.dll")).unwrap();
        if std::env::var("EVT_TEST_PLUGIN_LOG").is_ok() {
            crate::log::open(&root.join("loader.log"), crate::log::Level::Debug, false);
        }
        let plan = evt_modfmt::plan_root_for(&root.join("mods"), Some(crate::MODLOADER_VERSION));
        assert_eq!(plan.mods.len(), 1, "{:?}", plan.skipped);
        load_all(&plan, &root);
        assert!(loaded().is_empty());
        assert!(replacing_builtin("quit_fix").is_none());
        let mut m = EvtModInfo::default();
        unsafe {
            assert_eq!(api_mod_find(c"quit_fix".as_ptr(), &mut m), EVT_OK);
            assert_eq!(m.plugin_state, EVT_PLUGIN_FAILED);
            assert_eq!(api_mod_count(), 1);
        }
        if std::env::var("EVT_TEST_PLUGIN_LOG").is_ok() {
            crate::log::flush();
            println!("{}", std::fs::read_to_string(root.join("loader.log")).unwrap_or_default());
        }
    }
}
