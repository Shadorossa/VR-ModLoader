//! Lua command bridge: an inline hook on `lua.CommandDispatch` (`0xCA7550`, the single choke point of all 15
//! `funcLua*Command` globals, lua.md §3.6). If the first argument is the crc32 of one of our `CMND_EVT_*` names the
//! loader answers; everything else goes to the original dispatcher.
//!
//! Handler contract: arguments start at Lua stack index 2 (index 1 = the hash); values pushed by the handler are
//! the call's return values (the dispatcher returns `lua_gettop(L) - top`). Strings are pushed with
//! `lua_pushstring`, which copies them into the calling VM.

use crate::game::LuaApi;
use crate::{debug, error, info};
use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Mutex, OnceLock};

#[repr(C)]
pub struct LuaCmdCtx {
    pub l: *mut u8,
    pub cursor: i32,
    pub base: i32,
}

type DispatchFn = unsafe extern "C" fn(*mut LuaCmdCtx, i32, *mut u8, i32) -> u8;

pub type Handler = fn(&mut Call);

pub struct Command {
    pub name: &'static str,
    pub handler: Handler,
}

/// Commands called every frame: their «returned N value(s)» debug line is not written.
const QUIET_COMMANDS: &[&str] = &[];

static PENDING: Mutex<Vec<(u32, Command)>> = Mutex::new(Vec::new());
static COMMANDS: OnceLock<HashMap<u32, Command>> = OnceLock::new();
/// Filters on **retail** commands: `fn(&mut Call) -> bool`; true = answered (values pushed), false = the engine's
/// own handler runs as usual.
pub type Filter = fn(&mut Call) -> bool;
static PENDING_FILTERS: Mutex<Vec<(u32, &'static str, Filter)>> = Mutex::new(Vec::new());
/// Several modules may filter one retail command: they run in registration order until one answers.
static FILTERS: OnceLock<HashMap<u32, Vec<(&'static str, Filter)>>> = OnceLock::new();
/// After-hooks on **retail** commands: `fn()` run right after the engine's own handler returned (not when a filter
/// answered). They get no arguments: a filter of the same module saves what they need (e.g. the
/// team a place / move must not touch, restored afterwards).
pub type After = fn();
static PENDING_AFTER: Mutex<Vec<(u32, &'static str, After)>> = Mutex::new(Vec::new());
static AFTER: OnceLock<HashMap<u32, Vec<(&'static str, After)>>> = OnceLock::new();
static API: OnceLock<LuaApi> = OnceLock::new();
static ORIG: AtomicUsize = AtomicUsize::new(0);
/// Optional `fn(u32)` told the hash of every engine command the scripts call (e.g. story
/// detection). 0 = none. It must not call Lua.
pub static OBSERVER: AtomicUsize = AtomicUsize::new(0);

pub fn hash(name: &str) -> u32 {
    crc32fast::hash(name.as_bytes())
}

/// Register `CMND_EVT_<name>` (call before `install`).
pub fn register(name: &'static str, handler: Handler) {
    let h = hash(name);
    info!("lua: command {name} = 0x{h:08X} ({h})");
    PENDING.lock().unwrap().push((h, Command { name, handler }));
}

/// Register a command under an explicit hash (`name` is only for the log; ModLoader plugins, call before `activate`).
pub fn register_hash(h: u32, name: &'static str, handler: Handler) {
    info!("lua: command {name} = 0x{h:08X} ({h})");
    PENDING.lock().unwrap().push((h, Command { name, handler }));
}

/// A command with this hash is registered (pending or active).
pub fn is_registered(h: u32) -> bool {
    COMMANDS.get().is_some_and(|m| m.contains_key(&h)) || PENDING.lock().unwrap_or_else(|e| e.into_inner()).iter().any(|(x, _)| *x == h)
}

/// Drop a pending registration (a ModLoader plugin whose init failed). false = not pending.
pub fn unregister_pending(h: u32) -> bool {
    let mut p = PENDING.lock().unwrap_or_else(|e| e.into_inner());
    let n = p.len();
    p.retain(|(x, _)| *x != h);
    p.len() != n
}

/// [`activate`] ran: no more registrations.
pub fn activated() -> bool {
    COMMANDS.get().is_some()
}

/// Register a filter on the retail command `hash` (call before `activate`).
pub fn register_filter(hash: u32, name: &'static str, f: Filter) {
    info!("lua: filter {name} on retail command 0x{hash:08X}");
    PENDING_FILTERS.lock().unwrap().push((hash, name, f));
}

/// Register an after-hook on the retail command `hash` (call before `activate`).
pub fn register_after(hash: u32, name: &'static str, f: After) {
    info!("lua: after-hook {name} on retail command 0x{hash:08X}");
    PENDING_AFTER.lock().unwrap().push((hash, name, f));
}

/// One command invocation.
pub struct Call {
    pub l: *mut u8,
    pub nargs: i32,
    api: LuaApi,
    pushed: i32,
}

pub const LUA_TNIL: i32 = 0;
pub const LUA_TBOOLEAN: i32 = 1;
pub const LUA_TNUMBER: i32 = 3;
pub const LUA_TSTRING: i32 = 4;

impl Call {
    /// The command hash (Lua stack index 1).
    pub fn hash(&self) -> u32 {
        let mut isnum = 0;
        let v = unsafe { (self.api.tonumberx)(self.l, 1, &mut isnum) };
        v as i64 as u32
    }
    /// Argument `i` (0-based, after the hash) as a number.
    pub fn num(&self, i: i32) -> Option<f64> {
        if i >= self.nargs {
            return None;
        }
        let mut isnum = 0;
        let v = unsafe { (self.api.tonumberx)(self.l, i + 2, &mut isnum) };
        (isnum != 0).then_some(v)
    }
    /// Argument as u32 (handles / hashes; negative numbers wrap like the engine's `(u32)` cast).
    pub fn u32(&self, i: i32) -> Option<u32> {
        self.num(i).map(|v| v as i64 as u32)
    }
    pub fn int(&self, i: i32) -> Option<i64> {
        self.num(i).map(|v| v as i64)
    }
    pub fn arg_type(&self, i: i32) -> i32 {
        if i >= self.nargs {
            return -1;
        }
        unsafe { (self.api.type_)(self.l, i + 2) }
    }
    pub fn string(&self, i: i32) -> Option<String> {
        if self.arg_type(i) != LUA_TSTRING {
            return None;
        }
        let mut len = 0usize;
        let p = unsafe { (self.api.tolstring)(self.l, i + 2, &mut len) };
        if p.is_null() {
            return None;
        }
        Some(String::from_utf8_lossy(unsafe { std::slice::from_raw_parts(p, len) }).into_owned())
    }
    /// Human-readable dump of argument `i` (for logs).
    pub fn describe(&self, i: i32) -> String {
        match self.arg_type(i) {
            LUA_TNUMBER => {
                let v = self.num(i).unwrap_or(0.0);
                if v.fract() == 0.0 && v.abs() < 1e15 {
                    format!("{}", v as i64)
                } else {
                    format!("{v}")
                }
            }
            LUA_TSTRING => format!("{:?}", self.string(i).unwrap_or_default()),
            LUA_TBOOLEAN => "bool".into(),
            LUA_TNIL => "nil".into(),
            t => format!("<type {t}>"),
        }
    }
    pub fn push_num(&mut self, v: f64) {
        unsafe { (self.api.pushnumber)(self.l, v) };
        self.pushed += 1;
    }
    pub fn push_int(&mut self, v: i64) {
        self.push_num(v as f64);
    }
    pub fn push_bool(&mut self, v: bool) {
        unsafe { (self.api.pushboolean)(self.l, v as i32) };
        self.pushed += 1;
    }
    /// Push a Lua array `{v1, v2, ...}` (needs [`set_table_api`]; false when it is not available: nothing pushed).
    pub fn push_u32_table(&mut self, vals: &[u32]) -> bool {
        let Some(t) = TABLE_API.get() else { return false };
        unsafe {
            (t.createtable)(self.l, vals.len() as i32, 0);
            for (i, v) in vals.iter().enumerate() {
                (self.api.pushnumber)(self.l, *v as f64);
                (t.rawseti)(self.l, -2, i as i32 + 1);
            }
        }
        self.pushed += 1;
        true
    }
    pub fn push_str(&mut self, s: &str) {
        let c: Vec<u8> = s.bytes().filter(|&b| b != 0).chain(std::iter::once(0)).collect();
        unsafe { (self.api.pushstring)(self.l, c.as_ptr()) };
        self.pushed += 1;
    }
}

/// `lua_createtable` / `lua_rawseti` (Lua 5.2), for commands and filters that answer with tables.
#[derive(Clone, Copy)]
pub struct TableApi {
    pub createtable: unsafe extern "C" fn(*mut u8, i32, i32),
    pub rawseti: unsafe extern "C" fn(*mut u8, i32, i32),
}
static TABLE_API: OnceLock<TableApi> = OnceLock::new();

/// Register the table functions (resolved by the module that needs them).
pub fn set_table_api(t: TableApi) {
    let _ = TABLE_API.set(t);
}


unsafe extern "C" fn dispatch_detour(ctx: *mut LuaCmdCtx, top: i32, table: *mut u8, count: i32) -> u8 {
    let orig: DispatchFn = std::mem::transmute::<usize, DispatchFn>(ORIG.load(Ordering::Acquire));
    let mut after: &[(&'static str, After)] = &[];
    // hang watchdog heartbeat (module debug)
    let on_main = !ctx.is_null() && crate::debug::watchdog::tick(crate::debug::watchdog::SRC_DISPATCH, (*ctx).l as usize);
    if !ctx.is_null() && top >= 1 {
        if let (Some(api), Some(cmds)) = (API.get(), COMMANDS.get()) {
            let l = (*ctx).l;
            let mut isnum = 0;
            let d = (api.tonumberx)(l, 1, &mut isnum);
            if isnum != 0 {
                let h = d as i64 as u32;
                if on_main {
                    crate::debug::watchdog::note_command(h);
                }
                let obs = OBSERVER.load(Ordering::Acquire);
                if obs != 0 {
                    let f: fn(u32) = std::mem::transmute::<usize, fn(u32)>(obs);
                    f(h);
                }
                for (name, f) in FILTERS.get().and_then(|m| m.get(&h)).map(|v| v.as_slice()).unwrap_or(&[]) {
                    let prev = (*ctx).base;
                    (*ctx).base = 1;
                    let api = *api;
                    let r = std::panic::catch_unwind(|| {
                        let mut call = Call { l, nargs: top - 1, api, pushed: 0 };
                        f(&mut call)
                    });
                    match r {
                        Ok(true) => {
                            debug!("lua: filter {name} answered");
                            return 1;
                        }
                        Ok(false) => (*ctx).base = prev,
                        Err(_) => {
                            error!("lua: filter {name} panicked");
                            (*ctx).base = prev;
                        }
                    }
                }
                after = AFTER.get().and_then(|m| m.get(&h)).map(|v| v.as_slice()).unwrap_or(&[]);
                if let Some(cmd) = cmds.get(&h) {
                    (*ctx).base = 1;
                    let api = *api;
                    let r = std::panic::catch_unwind(|| {
                        let mut call = Call { l, nargs: top - 1, api, pushed: 0 };
                        (cmd.handler)(&mut call);
                        call.pushed
                    });
                    return match r {
                        Ok(n) => {
                            // per-frame commands (free-walk doors / icon, field state) are not logged: 4 MB of
                            // «returned 4 value(s)» in minutes (29/09)
                            if !QUIET_COMMANDS.contains(&cmd.name) {
                                debug!("lua: {} returned {n} value(s)", cmd.name);
                            }
                            1
                        }
                        Err(_) => {
                            error!("lua: {} panicked", cmd.name);
                            0
                        }
                    };
                }
            }
        }
    }
    let r = orig(ctx, top, table, count);
    for (name, f) in after {
        if std::panic::catch_unwind(f).is_err() {
            error!("lua: after-hook {name} panicked");
        }
    }
    r
}

/// Install the dispatcher hook (called from DllMain, before any game code runs, so no thread can be executing
/// the patched prologue). Until [`activate`] runs, the detour only forwards to the original.
pub fn install_hook(text: &crate::game::Text) -> bool {
    let Some(target) = text.resolve(&crate::sigs::LUA_COMMAND_DISPATCH) else { return false };
    let pat = crate::scan::Pattern::parse(crate::sigs::LUA_COMMAND_DISPATCH.pattern).unwrap();
    let Some(expected) = pat.fixed_prefix(crate::sigs::LUA_COMMAND_DISPATCH_STEAL) else {
        error!("lua: dispatcher steal bytes contain wildcards");
        return false;
    };
    match unsafe { crate::hook::inline_hook(target, &expected, dispatch_detour as *const () as usize, &ORIG) } {
        Ok(_) => {
            info!("lua: CommandDispatch hooked at 0x{target:X}");
            true
        }
        Err(e) => {
            error!("lua: CommandDispatch hook failed: {e}");
            false
        }
    }
}

/// Name of a loader command / filter by hash (lock-free; hang watchdog). None for retail commands.
pub fn command_name(h: u32) -> Option<&'static str> {
    COMMANDS.get().and_then(|m| m.get(&h)).map(|c| c.name).or_else(|| FILTERS.get().and_then(|m| m.get(&h)).and_then(|v| v.first()).map(|f| f.0))
}

pub fn hook_installed() -> bool {
    ORIG.load(Ordering::Acquire) != 0
}

/// Publish the Lua API and every registered command (init thread).
pub fn activate(api: LuaApi) -> usize {
    let cmds: HashMap<u32, Command> = PENDING.lock().unwrap().drain(..).collect();
    let mut filters: HashMap<u32, Vec<(&'static str, Filter)>> = HashMap::new();
    for (h, n, f) in PENDING_FILTERS.lock().unwrap().drain(..) {
        filters.entry(h).or_default().push((n, f));
    }
    let _ = FILTERS.set(filters);
    let mut after: HashMap<u32, Vec<(&'static str, After)>> = HashMap::new();
    for (h, n, f) in PENDING_AFTER.lock().unwrap().drain(..) {
        after.entry(h).or_default().push((n, f));
    }
    let _ = AFTER.set(after);
    let n = cmds.len();
    let _ = API.set(api);
    let _ = COMMANDS.set(cmds);
    n
}
