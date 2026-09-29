//! Lua error capture (module `debug`, docs/game/engine/debugger.md §2).
//!
//! * `luaL_loadbufferx 0x5EA510` (pre + post): computes a chunk label (loose file matched by CRC-32 in
//!   `[debug] index_dirs`, else `INCLUDE <name>` / `chunk crc=… size=…`), gives nameless chunks that label as their
//!   chunk name (so runtime messages say `file:line:`), registers `print`/`PRINT`/`WARNING`/`ASSERT` in a fresh VM
//!   (stack empty = the script object's own chunk) and logs load errors.
//! * `lua_pcallk 0x5E7CA0`: for engine calls (`errfunc == 0 && k == NULL`) inserts a message handler below the
//!   function that runs `luaL_traceback`, removes it afterwards; logs every error status. Script `pcall`s
//!   (`k != NULL`) are logged without traceback (tagged `pcall`), `xpcall` keeps its own handler.
//!
//! The engine pops the message and continues in every caller (0x4D6B8F, 0x4D6E06, 0x177C8A1), so these lines are
//! the only trace of a failing script.

use super::ratelimit::{self, Limiter};
use super::sigs as dsig;
use super::DebugCfg;
use crate::game::{LuaApi, Text};
use crate::{error, info, warn};
use std::collections::{HashMap, VecDeque};
use std::ffi::CString;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::Instant;

type LoadFn = unsafe extern "C-unwind" fn(*mut u8, *const u8, usize, *const u8, *const u8) -> i32;
type PcallFn = unsafe extern "C-unwind" fn(*mut u8, i32, i32, i32, i32, usize) -> i32;
type CFn = unsafe extern "C-unwind" fn(*mut u8) -> i32;

static ORIG_LOAD: AtomicUsize = AtomicUsize::new(0);
static ORIG_PCALL: AtomicUsize = AtomicUsize::new(0);
static ACTIVE: AtomicBool = AtomicBool::new(false);
static API: OnceLock<Api> = OnceLock::new();
static CFG: OnceLock<DebugCfg> = OnceLock::new();
static EPOCH: OnceLock<Instant> = OnceLock::new();
/// CRC-32 of a loose chunk file -> path relative to its index dir (built on a background thread).
static FILES: OnceLock<HashMap<u32, String>> = OnceLock::new();
/// VM (lua_State*) -> label of its own chunk.
static VMS: Mutex<Option<HashMap<usize, String>>> = Mutex::new(None);
static ERR_LIMIT: Mutex<Limiter> = Mutex::new(Limiter::new(10_000, 20));
static PRINT_LIMIT: Mutex<Limiter> = Mutex::new(Limiter::new(10_000, 100));
/// Last errors (for the debug server / app).
static RECENT: Mutex<VecDeque<LuaError>> = Mutex::new(VecDeque::new());
static SEQ: AtomicU64 = AtomicU64::new(0);
const RECENT_MAX: usize = 200;
const MAX_TRACE_LINES: usize = 30;

const LUA_TNIL: i32 = 0;
const LUA_TBOOLEAN: i32 = 1;
const LUA_TNUMBER: i32 = 3;
const LUA_TSTRING: i32 = 4;
const LUA_YIELD: i32 = 1;

#[derive(Clone, Copy)]
struct Api {
    lua: LuaApi,
    settop: unsafe extern "C-unwind" fn(*mut u8, i32),
    checkstack: unsafe extern "C-unwind" fn(*mut u8, i32) -> i32,
    insert: unsafe extern "C-unwind" fn(*mut u8, i32),
    remove: unsafe extern "C-unwind" fn(*mut u8, i32),
    pushcclosure: unsafe extern "C-unwind" fn(*mut u8, CFn, i32),
    setglobal: unsafe extern "C-unwind" fn(*mut u8, *const u8),
    toboolean: unsafe extern "C-unwind" fn(*mut u8, i32) -> i32,
    traceback: unsafe extern "C-unwind" fn(*mut u8, *mut u8, *const u8, i32),
}

/// One captured error.
#[derive(Debug, Clone)]
pub struct LuaError {
    pub seq: u64,
    pub ms: u64,
    pub kind: &'static str,
    pub label: String,
    pub message: String,
}

fn now_ms() -> u64 {
    EPOCH.get_or_init(Instant::now).elapsed().as_millis() as u64
}

fn cfg() -> &'static DebugCfg {
    CFG.get_or_init(DebugCfg::default)
}

// ---------------------------------------------------------------- install (DllMain) / activate (init thread)

fn hook(text: &Text, sig: &crate::sigs::Sig, steal: usize, detour: usize, orig: &AtomicUsize) -> bool {
    let Some(target) = text.resolve(sig) else { return false };
    let Some(expected) = crate::scan::Pattern::parse(sig.pattern).ok().and_then(|p| p.fixed_prefix(steal)) else {
        error!("debug: {} steal bytes contain wildcards", sig.name);
        return false;
    };
    match unsafe { crate::hook::inline_hook(target, &expected, detour, orig) } {
        Ok(_) => {
            info!("debug: {} hooked at 0x{target:X}", sig.name);
            true
        }
        Err(e) => {
            error!("debug: {} hook failed: {e}", sig.name);
            false
        }
    }
}

pub fn install_hooks(text: &Text) -> bool {
    let load = hook(text, &dsig::DBG_LOADBUFFERX, dsig::DBG_LOADBUFFERX_STEAL, load_detour as *const () as usize, &ORIG_LOAD);
    let pcall = hook(text, &dsig::DBG_PCALLK, dsig::DBG_PCALLK_STEAL, pcall_detour as *const () as usize, &ORIG_PCALL);
    load || pcall
}

pub fn activate(text: &Text, game_dir: &Path, c: &DebugCfg) -> bool {
    let _ = CFG.set(c.clone());
    let _ = EPOCH.get_or_init(Instant::now);
    *ERR_LIMIT.lock().unwrap_or_else(|e| e.into_inner()) = Limiter::new(10_000, c.errors_per_10s.max(1));
    *PRINT_LIMIT.lock().unwrap_or_else(|e| e.into_inner()) = Limiter::new(10_000, c.prints_per_10s.max(1));
    let api = (|| unsafe {
        use std::mem::transmute;
        Some(Api {
            lua: LuaApi::resolve(text)?,
            settop: transmute::<usize, _>(text.resolve(&dsig::DBG_SETTOP)?),
            checkstack: transmute::<usize, _>(text.resolve(&dsig::DBG_CHECKSTACK)?),
            insert: transmute::<usize, _>(text.resolve(&dsig::DBG_INSERT)?),
            remove: transmute::<usize, _>(text.resolve(&dsig::DBG_REMOVE)?),
            pushcclosure: transmute::<usize, _>(text.resolve(&dsig::DBG_PUSHCCLOSURE)?),
            setglobal: transmute::<usize, _>(text.resolve(&dsig::DBG_SETGLOBAL)?),
            toboolean: transmute::<usize, _>(text.resolve(&dsig::DBG_TOBOOLEAN)?),
            traceback: transmute::<usize, _>(text.resolve(&dsig::DBG_TRACEBACK)?),
        })
    })();
    let Some(api) = api else {
        error!("debug: Lua API signatures not resolved: Lua error capture disabled");
        return false;
    };
    let _ = API.set(api);
    if c.chunk_names {
        let dirs: Vec<PathBuf> = c
            .index_dirs
            .iter()
            .map(|d| {
                let p = PathBuf::from(d);
                if p.is_absolute() { p } else { game_dir.join(p) }
            })
            .collect();
        let _ = std::thread::Builder::new().name("vr-loader-luaidx".into()).spawn(move || build_index(&dirs));
    }
    ACTIVE.store(true, Ordering::Release);
    info!(
        "debug: Lua error capture on (load hook {}, pcall hook {})",
        ORIG_LOAD.load(Ordering::Acquire) != 0,
        ORIG_PCALL.load(Ordering::Acquire) != 0
    );
    true
}

fn api() -> Option<&'static Api> {
    if ACTIVE.load(Ordering::Acquire) {
        API.get()
    } else {
        None
    }
}

// ---------------------------------------------------------------- chunk file index

fn build_index(dirs: &[PathBuf]) {
    let t0 = Instant::now();
    let mut map = HashMap::new();
    let mut files = 0usize;
    for dir in dirs {
        let mut stack = vec![dir.clone()];
        while let Some(d) = stack.pop() {
            let Ok(rd) = std::fs::read_dir(&d) else { continue };
            for e in rd.flatten() {
                let p = e.path();
                let Ok(ft) = e.file_type() else { continue };
                if ft.is_dir() {
                    stack.push(p);
                    continue;
                }
                let name = e.file_name().to_string_lossy().to_ascii_lowercase();
                if !(name.ends_with(".lua.bin") || name.ends_with(".lua")) || files >= 20_000 {
                    continue;
                }
                if e.metadata().map(|m| m.len() > 8 << 20).unwrap_or(true) {
                    continue;
                }
                if let Ok(b) = std::fs::read(&p) {
                    files += 1;
                    let rel = p.strip_prefix(dir).unwrap_or(&p).to_string_lossy().replace('\\', "/");
                    map.entry(crc32fast::hash(&b)).or_insert(rel);
                }
            }
        }
    }
    info!("debug: chunk index: {files} Lua file(s) in {:?} ({} ms)", dirs, t0.elapsed().as_millis());
    let _ = FILES.set(map);
}

// ---------------------------------------------------------------- helpers (game thread)

unsafe fn stack_string(api: &Api, l: *mut u8, idx: i32) -> Option<String> {
    if (api.lua.type_)(l, idx) != LUA_TSTRING {
        return None;
    }
    let mut len = 0usize;
    let p = (api.lua.tolstring)(l, idx, &mut len);
    (!p.is_null()).then(|| String::from_utf8_lossy(std::slice::from_raw_parts(p, len)).into_owned())
}

/// Human-readable value at `idx` (no metamethods, no Lua errors).
unsafe fn describe(api: &Api, l: *mut u8, idx: i32) -> String {
    match (api.lua.type_)(l, idx) {
        LUA_TSTRING => stack_string(api, l, idx).unwrap_or_default(),
        LUA_TNUMBER => {
            let mut ok = 0;
            let v = (api.lua.tonumberx)(l, idx, &mut ok);
            if v.fract() == 0.0 && v.abs() < 1e15 {
                format!("{}", v as i64)
            } else {
                format!("{v}")
            }
        }
        LUA_TBOOLEAN => ((api.toboolean)(l, idx) != 0).to_string(),
        LUA_TNIL => "nil".into(),
        2 => "<lightuserdata>".into(),
        5 => "<table>".into(),
        6 => "<function>".into(),
        7 => "<userdata>".into(),
        8 => "<thread>".into(),
        t => format!("<type {t}>"),
    }
}

/// Label of a chunk being loaded.
unsafe fn chunk_label(api: &Api, l: *mut u8, buf: *const u8, sz: usize, name: *const u8, top: i32) -> String {
    if !name.is_null() {
        let s = std::ffi::CStr::from_ptr(name as *const std::ffi::c_char).to_string_lossy();
        return s.trim_start_matches(['=', '@']).to_string();
    }
    let crc = if !buf.is_null() && sz > 0 && sz < (256 << 20) {
        crc32fast::hash(std::slice::from_raw_parts(buf, sz))
    } else {
        0
    };
    if let Some(path) = FILES.get().and_then(|m| m.get(&crc)) {
        return path.clone();
    }
    // INCLUDE("LUA_X"): the INCLUDE C function's argument is on the stack
    for i in 1..=top.min(4) {
        if let Some(s) = stack_string(api, l, i) {
            if s.starts_with("LUA_") && s.len() < 96 {
                return format!("INCLUDE {s} (crc 0x{crc:08X})");
            }
        }
    }
    format!("chunk crc=0x{crc:08X} size={sz}")
}

fn vm_label(l: *mut u8) -> String {
    VMS.lock()
        .unwrap_or_else(|e| e.into_inner())
        .as_ref()
        .and_then(|m| m.get(&(l as usize)).cloned())
        .unwrap_or_else(|| format!("vm 0x{:X}", l as usize))
}

/// [`vm_label`] without blocking (hang watchdog: the stalled thread may hold the lock). None = unknown / busy.
pub fn vm_label_try(l: usize) -> Option<String> {
    VMS.try_lock().ok()?.as_ref()?.get(&l).cloned()
}

fn set_vm_label(l: *mut u8, label: &str) {
    let mut g = VMS.lock().unwrap_or_else(|e| e.into_inner());
    let m = g.get_or_insert_with(HashMap::new);
    if m.len() > 4096 {
        m.clear();
    }
    m.insert(l as usize, label.to_string());
}

fn status_name(s: i32) -> &'static str {
    match s {
        2 => "runtime error",
        3 => "syntax error",
        4 => "out of memory",
        5 => "error in __gc",
        6 => "error in error handler",
        _ => "error",
    }
}

/// Write one LUAERR block (rate-limited) and keep it in the recent list.
fn report(kind: &'static str, status: i32, label: &str, msg: &str) {
    if !cfg().lua_errors {
        return;
    }
    let ms = now_ms();
    let seq = SEQ.fetch_add(1, Ordering::Relaxed) + 1;
    {
        let mut r = RECENT.lock().unwrap_or_else(|e| e.into_inner());
        if r.len() >= RECENT_MAX {
            r.pop_front();
        }
        r.push_back(LuaError { seq, ms, kind, label: label.to_string(), message: msg.to_string() });
    }
    let v = ERR_LIMIT.lock().unwrap_or_else(|e| e.into_inner()).check(ms, ratelimit::key(&format!("{kind}{label}{msg}")));
    for n in &v.notes {
        warn!("LUAERR {n}");
    }
    if !v.log {
        return;
    }
    let mut lines = msg.lines();
    warn!("LUAERR {kind} [{label}] {}: {}", status_name(status), lines.next().unwrap_or(""));
    for (i, line) in lines.enumerate() {
        if i >= MAX_TRACE_LINES {
            warn!("LUAERR   | ...");
            break;
        }
        warn!("LUAERR   | {}", line.trim_end());
    }
}

/// Captured errors with `seq > since` (for the debug server).
pub fn recent(since: u64) -> Vec<LuaError> {
    RECENT.lock().unwrap_or_else(|e| e.into_inner()).iter().filter(|e| e.seq > since).cloned().collect()
}

// ---------------------------------------------------------------- hooks

unsafe extern "C-unwind" fn load_detour(l: *mut u8, buf: *const u8, sz: usize, name: *const u8, mode: *const u8) -> i32 {
    super::watchdog::tick(super::watchdog::SRC_LOAD, l as usize);
    let orig: LoadFn = std::mem::transmute::<usize, LoadFn>(ORIG_LOAD.load(Ordering::Acquire));
    let Some(api) = api() else { return orig(l, buf, sz, name, mode) };
    if l.is_null() {
        return orig(l, buf, sz, name, mode);
    }
    let c = cfg();
    let top = (api.lua.gettop)(l);
    let label = chunk_label(api, l, buf, sz, name, top);
    // top == 0: the script object's own chunk in a fresh VM (INCLUDE and base `load` have arguments on the stack)
    let fresh = top == 0;
    if fresh && c.capture_print && (api.checkstack)(l, 2) != 0 {
        for (g, f) in [
            (&b"print\0"[..], lua_print as CFn),
            (b"PRINT\0", lua_print_upper as CFn),
            (b"WARNING\0", lua_warning as CFn),
            (b"ASSERT\0", lua_assert as CFn),
        ] {
            (api.pushcclosure)(l, f, 0);
            (api.setglobal)(l, g.as_ptr());
        }
    }
    let cname = (name.is_null() && c.chunk_names).then(|| CString::new(format!("={label}")).ok()).flatten();
    let r = orig(l, buf, sz, cname.as_ref().map_or(name, |s| s.as_ptr() as *const u8), mode);
    // the engine's own chunks are nameless; a named chunk on an empty stack is a module lua_patch file (`=patch:...`),
    // which must not rename the VM
    if fresh && name.is_null() {
        set_vm_label(l, &label);
    }
    if r != 0 {
        let msg = stack_string(api, l, -1).unwrap_or_else(|| "(no message)".into());
        report("load", r, &label, &msg);
    }
    r
}

/// Message handler: `luaL_traceback(L, L, msg, 1)` (pushes the message + traceback).
unsafe extern "C-unwind" fn msgh(l: *mut u8) -> i32 {
    let Some(api) = API.get() else { return 1 };
    if (api.lua.type_)(l, 1) != LUA_TSTRING {
        let d = describe(api, l, 1);
        let s = CString::new(format!("(error object is not a string: {d})")).unwrap_or_default();
        (api.lua.pushstring)(l, s.as_ptr() as *const u8);
    } else {
        (api.settop)(l, 1);
    }
    let p = (api.lua.tolstring)(l, -1, std::ptr::null_mut());
    (api.traceback)(l, l, p, 1);
    1
}

/// Heartbeat + pcall depth for the hang watchdog around [`pcall_inner`].
unsafe extern "C-unwind" fn pcall_detour(l: *mut u8, nargs: i32, nres: i32, errfunc: i32, ctx: i32, k: usize) -> i32 {
    let main = super::watchdog::tick(super::watchdog::SRC_PCALL, l as usize);
    if main {
        super::watchdog::pcall_enter();
    }
    let r = pcall_inner(l, nargs, nres, errfunc, ctx, k);
    if main {
        super::watchdog::pcall_leave();
        super::watchdog::tick(super::watchdog::SRC_PCALL, 0);
    }
    r
}

unsafe fn pcall_inner(l: *mut u8, nargs: i32, nres: i32, errfunc: i32, ctx: i32, k: usize) -> i32 {
    let orig: PcallFn = std::mem::transmute::<usize, PcallFn>(ORIG_PCALL.load(Ordering::Acquire));
    let Some(api) = api() else { return orig(l, nargs, nres, errfunc, ctx, k) };
    if l.is_null() {
        return orig(l, nargs, nres, errfunc, ctx, k);
    }
    let c = cfg();
    let engine_call = errfunc == 0 && k == 0;
    let func = (api.lua.gettop)(l) - nargs;
    if c.traceback && c.lua_errors && engine_call && func >= 1 && (api.checkstack)(l, 2) != 0 {
        (api.pushcclosure)(l, msgh as CFn, 0);
        (api.insert)(l, func);
        let r = orig(l, nargs, nres, func, ctx, k);
        (api.remove)(l, func);
        if r != 0 && r != LUA_YIELD {
            let msg = stack_string(api, l, -1).unwrap_or_else(|| describe(api, l, -1));
            report("run", r, &vm_label(l), &msg);
        }
        return r;
    }
    let r = orig(l, nargs, nres, errfunc, ctx, k);
    if r != 0 && r != LUA_YIELD {
        let msg = stack_string(api, l, -1).unwrap_or_else(|| describe(api, l, -1));
        let kind = if k != 0 { "pcall" } else if errfunc != 0 { "xpcall" } else { "run" };
        report(kind, r, &vm_label(l), &msg);
    }
    r
}

// ---------------------------------------------------------------- print capture

unsafe fn log_print(l: *mut u8, tag: &str, from: i32) {
    let Some(api) = API.get() else { return };
    let n = (api.lua.gettop)(l);
    let parts: Vec<String> = (from..=n.min(from + 15)).map(|i| describe(api, l, i)).collect();
    let text = parts.join("\t");
    let label = vm_label(l);
    let v = PRINT_LIMIT.lock().unwrap_or_else(|e| e.into_inner()).check(now_ms(), ratelimit::key(&format!("{label}{text}")));
    for note in &v.notes {
        info!("LUAPRINT {note}");
    }
    if v.log {
        info!("LUAPRINT {tag} [{label}] {text}");
    }
}

unsafe fn push_true(l: *mut u8) -> i32 {
    if let Some(api) = API.get() {
        (api.lua.pushboolean)(l, 1);
        1
    } else {
        0
    }
}

unsafe extern "C-unwind" fn lua_print(l: *mut u8) -> i32 {
    log_print(l, "print", 1);
    push_true(l)
}

unsafe extern "C-unwind" fn lua_print_upper(l: *mut u8) -> i32 {
    log_print(l, "PRINT", 1);
    push_true(l)
}

unsafe extern "C-unwind" fn lua_warning(l: *mut u8) -> i32 {
    log_print(l, "WARNING", 1);
    push_true(l)
}

unsafe extern "C-unwind" fn lua_assert(l: *mut u8) -> i32 {
    if let Some(api) = API.get() {
        if (api.lua.gettop)(l) >= 1 && (api.toboolean)(l, 1) == 0 {
            log_print(l, "ASSERT failed", 2);
        }
    }
    push_true(l)
}
