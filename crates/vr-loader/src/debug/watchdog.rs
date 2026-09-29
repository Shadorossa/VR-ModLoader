//! Hang watchdog (module `debug`, `[debug] watchdog`).
//!
//! The game's main thread (the one that ran DllMain; it also runs the Lua menus) ticks a heartbeat from the Lua
//! hooks the loader already has: `lua.CommandDispatch` (every `funcLua*Command` call) and the `debug` hooks on
//! `lua_pcallk` / `luaL_loadbufferx`. A background thread checks it every 500 ms. The main thread counts as
//! **stalled** when the heartbeat is older than `watchdog_ms` (3 s) and its window does not answer a `WM_NULL`
//! within 500 ms (the same test Windows uses for "not responding"; when the window belongs to another thread the
//! heartbeat alone decides). Then it logs, once, and again every 5 s while the stall lasts (at most 5 reports):
//!
//! * `HANG watchdog: ...` header (stall time, last heartbeat source, window state);
//! * `HANG hooks: ...`: the last 16 loader hook events (`name>` = detour entered, `name<orig` = the original
//!   function entered from the detour, `crate::hook` thunks);
//! * `HANG lua: ...`: last Lua command, pcall depth, VM chunk, and the Lua call frames (`chunk:line`, best effort:
//!   Lua 5.2 x64 layout read with guarded reads, lines that fail the sanity checks say so);
//! * `HANG stack #NN ...`: the main thread's native stack (suspended, `RtlVirtualUnwind`, resumed before anything is
//!   formatted: it may hold the heap lock), one frame per line as `module+RVA`, frames in the loader DLL with the
//!   nearest symbol (dbghelp + the build's PDB, when it is still next to the DLL's build path), hook thunks and
//!   trampolines by hook name.
//!
//! The log is flushed after every report (`error!` lines are written synchronously).

use crate::{error, info, warn};
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicU32, AtomicU64, AtomicU8, AtomicUsize, Ordering};
use std::sync::OnceLock;
use std::time::Duration;
use windows_sys::Win32::Foundation::{CloseHandle, BOOL, HANDLE, HWND, LPARAM};
use windows_sys::Win32::System::Threading::{
    GetCurrentProcessId, GetCurrentThreadId, OpenThread, THREAD_GET_CONTEXT, THREAD_QUERY_INFORMATION,
    THREAD_SUSPEND_RESUME,
};

extern "C" {
    fn evt_walk_thread(h: HANDLE, out: *mut u64, max: i32, regs: *mut u64) -> i32;
}

/// Heartbeat sources.
pub const SRC_DISPATCH: u8 = 1;
pub const SRC_PCALL: u8 = 2;
pub const SRC_LOAD: u8 = 3;

static MAIN_TID: AtomicU32 = AtomicU32::new(0);
static LAST_TICK_MS: AtomicU64 = AtomicU64::new(0);
static LAST_SRC: AtomicU8 = AtomicU8::new(0);
static LAST_L: AtomicUsize = AtomicUsize::new(0);
static LAST_CMD: AtomicU32 = AtomicU32::new(0);
static PCALL_DEPTH: AtomicI32 = AtomicI32::new(0);
static STARTED: AtomicBool = AtomicBool::new(false);

/// Frames kept per native stack.
const MAX_FRAMES: usize = 64;

fn now_ms() -> u64 {
    unsafe { windows_sys::Win32::System::SystemInformation::GetTickCount64() }
}

/// DllMain: remember the game's main thread (the one loading winmm.dll).
pub fn set_main_thread() {
    MAIN_TID.store(unsafe { GetCurrentThreadId() }, Ordering::Relaxed);
}

pub fn main_thread() -> u32 {
    MAIN_TID.load(Ordering::Relaxed)
}

/// Heartbeat from a hook. `l` = the calling `lua_State*` (0 = unknown). Returns true on the main thread (or when
/// the main thread is not known), false on other threads (their calls are not counted).
#[inline]
pub fn tick(src: u8, l: usize) -> bool {
    let m = MAIN_TID.load(Ordering::Relaxed);
    if m != 0 && unsafe { GetCurrentThreadId() } != m {
        return false;
    }
    LAST_TICK_MS.store(now_ms(), Ordering::Relaxed);
    LAST_SRC.store(src, Ordering::Relaxed);
    if l != 0 {
        LAST_L.store(l, Ordering::Relaxed);
    }
    true
}

/// Hash of the Lua command being dispatched (main thread, after [`tick`]).
#[inline]
pub fn note_command(h: u32) {
    LAST_CMD.store(h, Ordering::Relaxed);
}

/// `lua_pcallk` nesting on the main thread (approximate: a Lua error thrown through the detour skips the leave).
#[inline]
pub fn pcall_enter() {
    PCALL_DEPTH.fetch_add(1, Ordering::Relaxed);
}
#[inline]
pub fn pcall_leave() {
    PCALL_DEPTH.fetch_sub(1, Ordering::Relaxed);
}

// ---------------------------------------------------------------- stall tracker (pure)

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decision {
    /// Nothing to do.
    Idle,
    /// Log report `n` (1-based); the heartbeat is `stalled_ms` old.
    Report { n: u32, stalled_ms: u64 },
    /// The main thread runs again after `reports` report(s).
    Resumed { stalled_ms: u64, reports: u32 },
}

#[derive(Debug, Clone)]
pub struct Tracker {
    pub threshold_ms: u64,
    pub repeat_ms: u64,
    pub max_reports: u32,
    reports: u32,
    last_report: u64,
    last_age: u64,
}

impl Tracker {
    pub fn new(threshold_ms: u64, repeat_ms: u64, max_reports: u32) -> Tracker {
        Tracker { threshold_ms, repeat_ms, max_reports, reports: 0, last_report: 0, last_age: 0 }
    }

    pub fn reports(&self) -> u32 {
        self.reports
    }

    /// `last_tick` = heartbeat time (0 = never), `window_responsive` = the main thread's window answered
    /// (None = no window of the main thread / not checked).
    pub fn check(&mut self, now: u64, last_tick: u64, window_responsive: Option<bool>) -> Decision {
        if last_tick == 0 {
            return Decision::Idle;
        }
        let age = now.saturating_sub(last_tick);
        let stalled = age > self.threshold_ms && window_responsive != Some(true);
        if !stalled {
            if self.reports > 0 {
                let d = Decision::Resumed { stalled_ms: self.last_age, reports: self.reports };
                self.reports = 0;
                return d;
            }
            return Decision::Idle;
        }
        self.last_age = age;
        if self.reports == 0 || (self.reports < self.max_reports && now.saturating_sub(self.last_report) >= self.repeat_ms) {
            self.reports += 1;
            self.last_report = now;
            return Decision::Report { n: self.reports, stalled_ms: age };
        }
        Decision::Idle
    }
}

// ---------------------------------------------------------------- watchdog thread

/// Start the watchdog thread (once).
pub fn start(threshold_ms: u64) {
    if STARTED.swap(true, Ordering::AcqRel) {
        return;
    }
    let threshold_ms = threshold_ms.max(500);
    let ok = std::thread::Builder::new()
        .name("vr-loader-watchdog".into())
        .spawn(move || run(threshold_ms))
        .is_ok();
    if ok {
        info!(
            "debug: hang watchdog on (main thread {}, stall after {} ms without heartbeat and window not responding; HANG lines)",
            main_thread(),
            threshold_ms
        );
    } else {
        warn!("debug: hang watchdog thread could not be started");
    }
}

fn run(threshold_ms: u64) {
    let mut tr = Tracker::new(threshold_ms, 5000, 5);
    loop {
        std::thread::sleep(Duration::from_millis(500));
        let tid = main_thread();
        if tid == 0 {
            continue;
        }
        let now = now_ms();
        let last = LAST_TICK_MS.load(Ordering::Relaxed);
        // the window is only pinged once the heartbeat is old (menus that run no Lua for a while)
        let win = if last != 0 && now.saturating_sub(last) > threshold_ms { window_state(tid) } else { WindowState::default() };
        match tr.check(now, last, win.main_responsive) {
            Decision::Idle => {}
            Decision::Report { n, stalled_ms } => {
                dump(tid, n, tr.max_reports, stalled_ms, &win.describe(), &mut |l| error!("{l}"));
                crate::log::flush();
            }
            Decision::Resumed { stalled_ms, reports } => {
                warn!("HANG watchdog: main thread {tid} runs again (stalled at least {:.1} s, {reports} report(s))", stalled_ms as f64 / 1000.0);
                crate::log::flush();
            }
        }
    }
}

#[derive(Default)]
struct WindowState {
    /// Some(answered) when a visible window of the main thread exists.
    main_responsive: Option<bool>,
    /// Windows of other threads: (thread, answered).
    others: Vec<(u32, bool)>,
}

impl WindowState {
    fn describe(&self) -> String {
        let main = match self.main_responsive {
            Some(true) => "main window responsive".to_string(),
            Some(false) => "main window NOT responding".to_string(),
            None => "no window on the main thread".to_string(),
        };
        if self.others.is_empty() {
            main
        } else {
            let o: Vec<String> =
                self.others.iter().map(|(t, r)| format!("thread {t} {}", if *r { "responsive" } else { "NOT responding" })).collect();
            format!("{main}; other windows: {}", o.join(", "))
        }
    }
}

unsafe extern "system" fn enum_cb(hwnd: HWND, lp: LPARAM) -> BOOL {
    use windows_sys::Win32::UI::WindowsAndMessaging::{GetWindowThreadProcessId, IsWindowVisible};
    let st = &mut *(lp as *mut (u32, Vec<(HWND, u32)>));
    let mut pid = 0u32;
    let tid = GetWindowThreadProcessId(hwnd, &mut pid);
    if pid == st.0 && IsWindowVisible(hwnd) != 0 {
        st.1.push((hwnd, tid));
    }
    1
}

fn window_state(main_tid: u32) -> WindowState {
    use windows_sys::Win32::UI::WindowsAndMessaging::{EnumWindows, SendMessageTimeoutW, SMTO_ABORTIFHUNG, WM_NULL};
    let mut st: (u32, Vec<(HWND, u32)>) = (unsafe { GetCurrentProcessId() }, Vec::new());
    unsafe { EnumWindows(Some(enum_cb), &mut st as *mut _ as LPARAM) };
    let mut out = WindowState::default();
    for (hwnd, tid) in st.1.into_iter().take(4) {
        let mut res = 0usize;
        let ok = unsafe { SendMessageTimeoutW(hwnd, WM_NULL, 0, 0, SMTO_ABORTIFHUNG, 500, &mut res) } != 0;
        if tid == main_tid {
            out.main_responsive = Some(out.main_responsive.unwrap_or(true) && ok);
        } else if !out.others.iter().any(|o| o.0 == tid) {
            out.others.push((tid, ok));
        }
    }
    out
}

// ---------------------------------------------------------------- report

fn src_name(s: u8) -> &'static str {
    match s {
        SRC_DISPATCH => "lua.CommandDispatch",
        SRC_PCALL => "lua_pcallk",
        SRC_LOAD => "luaL_loadbufferx",
        _ => "none",
    }
}

/// Log report `n` of a stall of thread `tid` through `out` (one line per call).
pub fn dump(tid: u32, n: u32, max: u32, stalled_ms: u64, window: &str, out: &mut dyn FnMut(String)) {
    // 1. native stack first (the thread is suspended only inside `capture`)
    let stack = capture(tid);
    let last = LAST_TICK_MS.load(Ordering::Relaxed);
    out(format!(
        "HANG watchdog: main thread {tid} stalled {:.1} s (last heartbeat: {}, {window}) [report {n}/{max}]",
        stalled_ms as f64 / 1000.0,
        if last == 0 { "never" } else { src_name(LAST_SRC.load(Ordering::Relaxed)) }
    ));
    // 2. loader hooks
    let hooks = crate::hook::hooks();
    let names = hook_names(&hooks);
    let name_of = |id: u32| names.get(&id).cloned().unwrap_or_else(|| format!("hook#{id}"));
    let ev: Vec<String> =
        crate::hook::recent_events().into_iter().map(|(id, orig)| if orig { format!("{}<orig", name_of(id)) } else { format!("{}>", name_of(id)) }).collect();
    out(format!("HANG hooks (last entered, newest first; > detour, <orig original): {}", if ev.is_empty() { "none".into() } else { ev.join(", ") }));
    // 3. Lua
    let l = LAST_L.load(Ordering::Relaxed);
    let cmd = LAST_CMD.load(Ordering::Relaxed);
    let cmd_s = match crate::lua::command_name(cmd) {
        Some(nm) => format!("{nm} (0x{cmd:08X})"),
        None if cmd == 0 => "none".into(),
        None => format!("0x{cmd:08X} (retail)"),
    };
    let vm = if l == 0 { "?".into() } else { super::luaerr::vm_label_try(l).unwrap_or_else(|| format!("vm 0x{l:X}")) };
    out(format!("HANG lua: last command {cmd_s}, pcall depth {}, VM 0x{l:X} = {vm}", PCALL_DEPTH.load(Ordering::Relaxed)));
    for (i, f) in lua_frames(l).iter().enumerate() {
        out(format!("HANG lua #{i:02} {f}"));
    }
    // 4. native frames
    match stack {
        Err(e) => out(format!("HANG stack: not captured ({e})")),
        Ok((frames, regs)) => {
            out(format!("HANG stack of thread {tid}: {} frame(s), rip 0x{:X} rsp 0x{:X} rbp 0x{:X}", frames.len(), regs[0], regs[1], regs[2]));
            let sym = Symbols::get();
            for (i, &a) in frames.iter().enumerate() {
                out(format!("HANG stack #{i:02} {}", describe_addr(a as usize, &hooks, &names, sym)));
            }
        }
    }
}

/// Suspend `tid`, walk its stack, resume it. Nothing is allocated while it is suspended (fixed buffers).
fn capture(tid: u32) -> Result<(Vec<u64>, [u64; 3]), String> {
    if tid == unsafe { GetCurrentThreadId() } {
        return Err("own thread".into());
    }
    let h = unsafe { OpenThread(THREAD_SUSPEND_RESUME | THREAD_GET_CONTEXT | THREAD_QUERY_INFORMATION, 0, tid) };
    if h.is_null() {
        return Err(format!("OpenThread({tid}) failed"));
    }
    let mut buf = [0u64; MAX_FRAMES];
    let mut regs = [0u64; 3];
    let n = unsafe { evt_walk_thread(h, buf.as_mut_ptr(), MAX_FRAMES as i32, regs.as_mut_ptr()) };
    unsafe { CloseHandle(h) };
    match n {
        -1 => Err("SuspendThread failed".into()),
        -2 => Err("GetThreadContext failed".into()),
        n => Ok((buf[..n.max(0) as usize].to_vec(), regs)),
    }
}

/// Hook id -> name (signature of the hooked function, else `nie+RVA`).
fn hook_names(hooks: &[crate::hook::HookInfo]) -> HashMap<u32, String> {
    let base = crate::pe::live::exe_base();
    let mut by_rva: HashMap<u32, &'static str> = HashMap::new();
    for s in crate::registry::all_sigs() {
        by_rva.entry(s.rva).or_insert(s.name);
    }
    for (s, _) in crate::registry::all_hooks() {
        by_rva.insert(s.rva, s.name);
    }
    hooks
        .iter()
        .map(|h| {
            let rva = h.target.wrapping_sub(base) as u32;
            let nm = match by_rva.get(&rva) {
                Some(n) => n.to_string(),
                None if h.target > base && h.target - base < 0x1000_0000 => format!("nie+0x{rva:X}"),
                None => format!("0x{:X}", h.target),
            };
            (h.id, nm)
        })
        .collect()
}

fn module_of(a: usize) -> Option<(usize, String)> {
    use windows_sys::Win32::System::LibraryLoader::{
        GetModuleFileNameW, GetModuleHandleExW, GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS,
        GET_MODULE_HANDLE_EX_FLAG_UNCHANGED_REFCOUNT,
    };
    let mut hm = std::ptr::null_mut();
    let ok = unsafe {
        GetModuleHandleExW(GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS | GET_MODULE_HANDLE_EX_FLAG_UNCHANGED_REFCOUNT, a as *const u16, &mut hm)
    };
    if ok == 0 || hm.is_null() {
        return None;
    }
    let mut w = [0u16; 520];
    let n = unsafe { GetModuleFileNameW(hm, w.as_mut_ptr(), w.len() as u32) } as usize;
    let path = String::from_utf16_lossy(&w[..n]);
    let name = path.rsplit(['\\', '/']).next().unwrap_or(&path).to_string();
    Some((hm as usize, name))
}

fn describe_addr(a: usize, hooks: &[crate::hook::HookInfo], names: &HashMap<u32, String>, sym: Option<&Symbols>) -> String {
    for h in hooks {
        if a >= h.thunk && a < h.thunk + crate::hook::THUNK_SIZE {
            return format!("0x{a:X} [thunk of hook {}]", names.get(&h.id).map_or("?", |s| s));
        }
        if a >= h.tramp && a < h.tramp + crate::hook::TRAMP_SIZE {
            return format!("0x{a:X} [trampoline of hook {} = its original prologue]", names.get(&h.id).map_or("?", |s| s));
        }
    }
    let Some((base, name)) = module_of(a) else { return format!("0x{a:X} [no module]") };
    let mut s = format!("{name}+0x{:X}", a - base);
    if let Some(sym) = sym {
        if base == sym.base {
            s.push_str(" [LOADER]");
            if let Some(d) = sym.describe(a) {
                s.push(' ');
                s.push_str(&d);
            }
        }
    }
    // a hooked engine function: say which hook sits on it
    if let Some(h) = hooks.iter().find(|h| a >= h.target && a < h.target + 32) {
        s.push_str(&format!(" (hooked function {})", names.get(&h.id).map_or("?", |s| s)));
    }
    s
}

/// dbghelp symbols of the loader module (the DLL / test binary that contains this code).
struct Symbols {
    base: usize,
    loaded: bool,
}

static SYMS: OnceLock<Symbols> = OnceLock::new();
/// dbghelp session id: a private value, not the process handle (with the real handle dbghelp reads the loaded image
/// and finds no PDB; a private id also keeps clear of any other dbghelp user in the game process). The module is
/// loaded from its file.
const SYM_HANDLE: usize = 0x5652_4C44;

impl Symbols {
    fn get() -> Option<&'static Symbols> {
        Some(SYMS.get_or_init(|| {
            use windows_sys::Win32::System::Diagnostics::Debug::{
                SymInitializeW, SymLoadModuleExW, SymSetOptions, SYMOPT_DEFERRED_LOADS, SYMOPT_LOAD_LINES, SYMOPT_UNDNAME,
            };
            use windows_sys::Win32::System::LibraryLoader::GetModuleFileNameW;
            let (base, _) = module_of(Symbols::get as *const () as usize).unwrap_or((0, String::new()));
            if base == 0 {
                return Symbols { base: 0, loaded: false };
            }
            let mut w = [0u16; 520];
            let n = unsafe { GetModuleFileNameW(base as _, w.as_mut_ptr(), w.len() as u32) } as usize;
            let path: Vec<u16> = w[..n].iter().copied().chain(std::iter::once(0)).collect();
            // search path: the DLL's folder, then the build folders of this source tree (the DLL only names
            // `vr_loader.pdb`; dbghelp checks the PDB's GUID, so the PDB of another build is refused, not misused)
            let dir: Vec<u16> = {
                let s = String::from_utf16_lossy(&w[..n]);
                let d = s.rsplit_once('\\').map(|x| x.0.to_string()).unwrap_or_default();
                let root = concat!(env!("CARGO_MANIFEST_DIR"), "\\..\\..\\target");
                format!("{d};{root}\\x86_64-pc-windows-msvc\\release;{root}\\release")
                    .encode_utf16()
                    .chain(std::iter::once(0))
                    .collect()
            };
            let size = unsafe { crate::pe::live::headers(base) }.map(|h| h.size_of_image).unwrap_or(0);
            let proc = SYM_HANDLE as HANDLE;
            let loaded = unsafe {
                SymSetOptions(SYMOPT_UNDNAME | SYMOPT_DEFERRED_LOADS | SYMOPT_LOAD_LINES);
                SymInitializeW(proc, dir.as_ptr(), 0) != 0
                    && SymLoadModuleExW(proc, std::ptr::null_mut(), path.as_ptr(), std::ptr::null(), base as u64, size, std::ptr::null(), 0) != 0
            };
            Symbols { base, loaded }
        }))
    }

    fn describe(&self, a: usize) -> Option<String> {
        use windows_sys::Win32::System::Diagnostics::Debug::{SymFromAddrW, SymGetLineFromAddrW64, IMAGEHLP_LINEW64, SYMBOL_INFOW};
        if !self.loaded {
            return None;
        }
        let proc = SYM_HANDLE as HANDLE;
        const MAXN: usize = 400;
        let mut buf = vec![0u64; (std::mem::size_of::<SYMBOL_INFOW>() + MAXN * 2) / 8 + 1];
        let si = buf.as_mut_ptr() as *mut SYMBOL_INFOW;
        let mut disp = 0u64;
        let name = unsafe {
            (*si).SizeOfStruct = std::mem::size_of::<SYMBOL_INFOW>() as u32;
            (*si).MaxNameLen = MAXN as u32;
            if SymFromAddrW(proc, a as u64, &mut disp, si) == 0 {
                return None;
            }
            let len = ((*si).NameLen as usize).min(MAXN);
            String::from_utf16_lossy(std::slice::from_raw_parts(std::ptr::addr_of!((*si).Name) as *const u16, len))
        };
        let mut s = format!("{name}+0x{disp:X}");
        let mut line: IMAGEHLP_LINEW64 = unsafe { std::mem::zeroed() };
        line.SizeOfStruct = std::mem::size_of::<IMAGEHLP_LINEW64>() as u32;
        let mut ld = 0u32;
        if unsafe { SymGetLineFromAddrW64(proc, a as u64, &mut ld, &mut line) } != 0 && !line.FileName.is_null() {
            let f = unsafe {
                let mut k = 0;
                while *line.FileName.add(k) != 0 && k < 1024 {
                    k += 1;
                }
                String::from_utf16_lossy(std::slice::from_raw_parts(line.FileName, k))
            };
            let short = f.rsplit(['\\', '/']).take(2).collect::<Vec<_>>().into_iter().rev().collect::<Vec<_>>().join("/");
            s.push_str(&format!(" ({short}:{})", line.LineNumber));
        }
        Some(s)
    }
}

// ---------------------------------------------------------------- Lua 5.2 frames (best effort)

/// Call frames of `lua_State* l`, innermost first (Lua 5.2 x64 layout; guarded reads, sanity-checked).
fn lua_frames(l: usize) -> Vec<String> {
    use crate::game::{read, read_ptr};
    let mut v = Vec::new();
    if l == 0 {
        return v;
    }
    // lua_State: ci @0x20 (lua_checkstack reads top @0x10, ci @0x20, stack_last @0x30)
    let mut ci = read_ptr(l + 0x20);
    for _ in 0..16 {
        let Some(c) = ci else { break };
        // CallInfo: func @0, previous @0x10, callstatus @0x22, u.l.savedpc @0x38
        let Some(func) = read_ptr(c) else { break };
        let tt = read::<i32>(func + 8).unwrap_or(-1);
        let line = match tt & 0x3F {
            0x06 => lua_closure_frame(func, c),
            0x16 => format!("[C function {}]", code_name(read_ptr(func).unwrap_or(0))),
            0x26 => format!("[C closure {}]", code_name(read_ptr(func).and_then(|cl| read_ptr(cl + 0x18)).unwrap_or(0))),
            _ => format!("[frame tt 0x{tt:X}: layout mismatch?]"),
        };
        v.push(line);
        ci = read_ptr(c + 0x10);
    }
    v
}

fn code_name(a: usize) -> String {
    match module_of(a) {
        Some((b, n)) => format!("{n}+0x{:X}", a - b),
        None => format!("0x{a:X}"),
    }
}

fn lua_closure_frame(func: usize, ci: usize) -> String {
    use crate::game::{read, read_ptr};
    let r = (|| {
        let cl = read_ptr(func)?; // LClosure*
        let p = read_ptr(cl + 0x18)?; // Proto*
        let src = read_ptr(p + 0x48).and_then(|ts| {
            let len = read::<usize>(ts + 0x10)?.min(160);
            let mut b = vec![0u8; len];
            for (i, x) in b.iter_mut().enumerate() {
                *x = read::<u8>(ts + 0x18 + i)?;
            }
            let s = String::from_utf8_lossy(&b).into_owned();
            Some(s.trim_start_matches(['=', '@']).to_string())
        });
        let code = read_ptr(p + 0x18)?;
        let lineinfo = read_ptr(p + 0x28);
        let size_li = read::<i32>(p + 0x5C)?;
        let (def, lastdef) = (read::<i32>(p + 0x68)?, read::<i32>(p + 0x6C)?);
        let savedpc = read_ptr(ci + 0x38).unwrap_or(0);
        let pc = if savedpc >= code + 4 { ((savedpc - code) / 4 - 1) as i64 } else { -1 };
        let line = match lineinfo {
            Some(li) if pc >= 0 && pc < size_li as i64 => read::<i32>(li + pc as usize * 4),
            _ => None,
        };
        let src = src.unwrap_or_else(|| "?".into());
        let sane = |ln: i32| ln >= def && (lastdef == 0 || ln <= lastdef);
        Some(match line {
            Some(ln) if sane(ln) => format!("{src}:{ln} (function at line {def})"),
            Some(ln) => format!("{src}:{ln}? (outside function {def}-{lastdef}: layout mismatch?)"),
            None => format!("{src}:? (function at line {def}, pc {pc})"),
        })
    })();
    r.unwrap_or_else(|| "[Lua frame unreadable]".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicBool;
    use std::sync::Arc;

    #[test]
    fn tracker_reports_once_then_every_repeat_up_to_max() {
        let mut t = Tracker::new(3000, 5000, 3);
        assert_eq!(t.check(10_000, 0, None), Decision::Idle); // never ticked
        assert_eq!(t.check(10_000, 8_000, None), Decision::Idle); // 2 s: fine
        assert_eq!(t.check(12_000, 8_000, Some(true)), Decision::Idle); // 4 s but the window answers: idle Lua
        assert_eq!(t.check(12_000, 8_000, Some(false)), Decision::Report { n: 1, stalled_ms: 4000 });
        assert_eq!(t.check(12_500, 8_000, None), Decision::Idle); // < 5 s since the report
        assert_eq!(t.check(17_000, 8_000, None), Decision::Report { n: 2, stalled_ms: 9000 });
        assert_eq!(t.check(22_000, 8_000, None), Decision::Report { n: 3, stalled_ms: 14000 });
        assert_eq!(t.check(40_000, 8_000, None), Decision::Idle); // max reached
        assert_eq!(t.check(40_100, 40_050, None), Decision::Resumed { stalled_ms: 32_000, reports: 3 });
        assert_eq!(t.check(40_200, 40_050, None), Decision::Idle);
        // a new stall reports again
        assert_eq!(t.check(50_000, 40_050, None), Decision::Report { n: 1, stalled_ms: 9950 });
    }

    #[inline(never)]
    fn stuck_in_test_loop(stop: &AtomicBool) -> u64 {
        let mut x = 0u64;
        while !stop.load(Ordering::Relaxed) {
            x = std::hint::black_box(x.wrapping_mul(6364136223846793005).wrapping_add(1));
        }
        x
    }

    #[test]
    fn dump_shows_the_stack_of_a_stalled_thread() {
        let stop = Arc::new(AtomicBool::new(false));
        let (tx, rx) = std::sync::mpsc::channel();
        let s2 = stop.clone();
        let th = std::thread::spawn(move || {
            tx.send(unsafe { GetCurrentThreadId() }).unwrap();
            stuck_in_test_loop(&s2)
        });
        let tid = rx.recv().unwrap();
        std::thread::sleep(Duration::from_millis(100));
        let mut t = Tracker::new(100, 1000, 5);
        let Decision::Report { n, stalled_ms } = t.check(1_300, 1_000, None) else { panic!("no report") };
        let mut lines = Vec::new();
        dump(tid, n, 5, stalled_ms, "test", &mut |l| lines.push(l));
        stop.store(true, Ordering::Relaxed);
        th.join().unwrap();
        let text = lines.join("\n");
        assert!(text.contains("HANG watchdog: main thread") && text.contains("[report 1/5]"), "{text}");
        let frames: Vec<&String> = lines.iter().filter(|l| l.starts_with("HANG stack #")).collect();
        assert!(frames.len() >= 3, "{text}");
        // the innermost frames are the stuck function (debug build: its atomic load may be a frame of its own),
        // symbolized through dbghelp + the test binary's PDB
        assert!(frames.iter().take(4).any(|f| f.contains("[LOADER]") && f.contains("stuck_in_test_loop")), "{text}");
    }
}
