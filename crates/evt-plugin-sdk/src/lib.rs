//! SDK for native **ModLoader plugins** (Inazuma Eleven Victory Road PC v7.1.2; docs/app/modloader-plugins.md).
//!
//! A plugin is a `cdylib` shipped in a mod folder (`mods\<id>\<name>.dll`, `plugin = "<name>.dll"` in `mod.toml`).
//! The ModLoader (`winmm.dll`) loads it in the init thread, in mod load order, and calls its `evt_plugin_init` with
//! the API table ([`abi::EvtApi`], C ABI, versioned). This crate gives the raw types ([`abi`], mirror of
//! `sdk/evt_plugin.h`), a safe wrapper ([`Host`]) and [`declare_plugin!`]:
//!
//! ```ignore
//! use evt_plugin_sdk::{declare_plugin, evt_info, Host};
//!
//! fn init(host: &'static Host) -> Result<(), String> {
//!     let cfg = host.config_text();                  // <mod>\config.toml + [mods.<id>] overrides
//!     let f = host.sig("MainFrame", "48 83 EC 48 80 3D ?? ?? ?? ?? 00", 0xB486E0).ok_or("MainFrame not found")?;
//!     evt_info!("MainFrame at 0x{f:X}, config {} bytes", cfg.len());   // loader.log: "<mod id>: MainFrame at ..."
//!     Ok(())
//! }
//! declare_plugin!(init = init);
//! ```

pub mod abi;

pub use abi::*;
use core::ffi::{c_char, c_void};
use std::ffi::{CStr, CString};
use std::path::PathBuf;
use std::sync::atomic::AtomicUsize;
use std::sync::OnceLock;

/// Log level of [`Host::log`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(i32)]
pub enum Level {
    Error = EVT_LOG_ERROR,
    Warn = EVT_LOG_WARN,
    Info = EVT_LOG_INFO,
    Debug = EVT_LOG_DEBUG,
    Trace = EVT_LOG_TRACE,
}

/// One active mod ([`Host::mods`]).
#[derive(Debug, Clone, PartialEq)]
pub struct ModEntry {
    pub id: String,
    pub version: String,
    pub dir: PathBuf,
    /// Plugin DLL name ("" = none).
    pub plugin: String,
    pub load_index: u32,
    /// `EVT_PLUGIN_*`.
    pub plugin_state: u32,
}

/// The loader seen from the plugin: API table + this plugin's handle and identity.
pub struct Host {
    api: &'static EvtApi,
    h: *const EvtPlugin,
    pub mod_id: String,
    pub mod_version: String,
    pub mod_dir: PathBuf,
    pub loader_version: String,
    pub load_index: u32,
}

// The handle and the table are owned by the loader and valid for the whole process; every API function is
// thread-safe.
unsafe impl Send for Host {}
unsafe impl Sync for Host {}

static HOST: OnceLock<Host> = OnceLock::new();

/// The host (after `evt_plugin_init` started). Panics before.
pub fn host() -> &'static Host {
    HOST.get().expect("evt-plugin-sdk: host() before evt_plugin_init")
}

/// The host, None before `evt_plugin_init`.
pub fn try_host() -> Option<&'static Host> {
    HOST.get()
}

fn cstring(s: &str) -> CString {
    CString::new(s.replace('\0', "")).unwrap_or_default()
}

unsafe fn owned(p: *const c_char) -> String {
    if p.is_null() {
        String::new()
    } else {
        CStr::from_ptr(p).to_string_lossy().into_owned()
    }
}

impl Host {
    /// The raw API table (for functions this wrapper does not cover).
    pub fn raw(&self) -> &'static EvtApi {
        self.api
    }
    /// The raw plugin handle.
    pub fn handle(&self) -> *const EvtPlugin {
        self.h
    }

    /// One line in `evt_loader\loader.log`, prefixed `<mod id>: ` by the loader.
    pub fn log(&self, level: Level, msg: &str) {
        let c = cstring(msg);
        unsafe { (self.api.log)(self.h, level as i32, c.as_ptr()) }
    }
    /// Write the queued log lines now.
    pub fn flush(&self) {
        unsafe { (self.api.log_flush)() }
    }

    /// The mod's merged configuration as TOML text (empty when there is none).
    pub fn config_text(&self) -> String {
        unsafe {
            let n = (self.api.config_get)(self.h, std::ptr::null_mut(), 0);
            let mut buf = vec![0u8; n + 1];
            let m = (self.api.config_get)(self.h, buf.as_mut_ptr() as *mut c_char, buf.len());
            buf.truncate(m.min(n));
            String::from_utf8_lossy(&buf).into_owned()
        }
    }

    pub fn exe_base(&self) -> usize {
        unsafe { (self.api.exe_base)() }
    }

    /// Unique match of `pattern` in nie.exe `.text` (logged by the loader as `sig <name> -> RVA ...`).
    pub fn sig(&self, name: &str, pattern: &str, expected_rva: u32) -> Option<usize> {
        let (n, p) = (cstring(name), cstring(pattern));
        let mut out = 0usize;
        let r = unsafe { (self.api.sig_find)(self.h, n.as_ptr(), p.as_ptr(), expected_rva, &mut out) };
        (r == EVT_OK).then_some(out)
    }

    /// Target of a RIP-relative operand inside the code at `insn`.
    pub fn rip(&self, insn: usize, disp_off: u32, next_ip_off: u32) -> Option<usize> {
        let mut out = 0usize;
        let r = unsafe { (self.api.rip_target)(insn, disp_off, next_ip_off, &mut out) };
        (r == EVT_OK).then_some(out)
    }

    /// Guarded read of game memory.
    pub fn read<T: Copy>(&self, addr: usize) -> Option<T> {
        let mut v = std::mem::MaybeUninit::<T>::uninit();
        let r = unsafe { (self.api.mem_read)(addr, v.as_mut_ptr() as *mut c_void, std::mem::size_of::<T>()) };
        (r == EVT_OK).then(|| unsafe { v.assume_init() })
    }
    /// Guarded read of a non-null pointer (>= 0x10000).
    pub fn read_ptr(&self, addr: usize) -> Option<usize> {
        self.read::<usize>(addr).filter(|&p| p >= 0x10000)
    }
    /// Guarded write of game data.
    pub fn write<T: Copy>(&self, addr: usize, v: T) -> bool {
        unsafe { (self.api.mem_write)(addr, &v as *const T as *const c_void, std::mem::size_of::<T>()) == EVT_OK }
    }

    /// Call game code with up to 6 integer args under SEH. Err = `EVT_E_*`.
    pub fn call(&self, func: usize, args: &[u64]) -> Result<u64, i32> {
        let mut ret = 0u64;
        let r = unsafe { (self.api.call_guarded)(func, args.as_ptr(), args.len() as u32, &mut ret) };
        if r == EVT_OK {
            Ok(ret)
        } else {
            Err(r)
        }
    }

    /// Chained inline hook (see [`EvtApi::hook_inline`]). `next` must be a `static`; the detour calls
    /// `next.load(Acquire)` to continue.
    ///
    /// # Safety
    /// `detour` must have the exact signature of the hooked function and `prologue` the target's real first bytes.
    pub unsafe fn hook_inline(&self, target: usize, prologue: &[u8], detour: *const (), next: &'static AtomicUsize, priority: i32) -> Result<(), i32> {
        let r = (self.api.hook_inline)(self.h, target, prologue.as_ptr(), prologue.len(), detour as *const c_void, next.as_ptr(), priority);
        if r == EVT_OK {
            Ok(())
        } else {
            Err(r)
        }
    }

    /// IAT hook of `dll!func` imported by `module` (None = nie.exe). `orig` receives the previous slot value.
    ///
    /// # Safety
    /// `detour` must have the exact signature of the import.
    pub unsafe fn hook_iat(&self, module: Option<&str>, dll: &str, func: &str, detour: *const (), orig: &'static AtomicUsize) -> Result<(), i32> {
        let m = module.map(cstring);
        let (d, f) = (cstring(dll), cstring(func));
        let r = (self.api.hook_iat)(
            self.h,
            m.as_ref().map_or(std::ptr::null(), |c| c.as_ptr()),
            d.as_ptr(),
            f.as_ptr(),
            detour as *const c_void,
            orig.as_ptr(),
        );
        if r == EVT_OK {
            Ok(())
        } else {
            Err(r)
        }
    }

    /// Register the Lua command `name` (only during init).
    pub fn lua_register(&self, name: &str, f: fn(&mut LuaCall)) -> Result<(), i32> {
        let c = cstring(name);
        let r = unsafe { (self.api.lua_register)(self.h, c.as_ptr(), lua_trampoline, f as *const () as *mut c_void) };
        if r == EVT_OK {
            Ok(())
        } else {
            Err(r)
        }
    }

    fn entry(&self, m: &EvtModInfo) -> ModEntry {
        unsafe {
            ModEntry {
                id: owned(m.id),
                version: owned(m.version),
                dir: PathBuf::from(owned(m.dir)),
                plugin: owned(m.plugin),
                load_index: m.load_index,
                plugin_state: m.plugin_state,
            }
        }
    }

    /// Active mods in load order.
    pub fn mods(&self) -> Vec<ModEntry> {
        let n = unsafe { (self.api.mod_count)() };
        (0..n)
            .filter_map(|i| {
                let mut m = EvtModInfo::default();
                (unsafe { (self.api.mod_get)(i, &mut m) } == EVT_OK).then(|| self.entry(&m))
            })
            .collect()
    }

    pub fn find_mod(&self, id: &str) -> Option<ModEntry> {
        let c = cstring(id);
        let mut m = EvtModInfo::default();
        (unsafe { (self.api.mod_find)(c.as_ptr(), &mut m) } == EVT_OK).then(|| self.entry(&m))
    }

    /// Mod providing `name` (its id or `provides`) and the provided version.
    pub fn provider(&self, name: &str) -> Option<(ModEntry, String)> {
        let c = cstring(name);
        let mut m = EvtModInfo::default();
        let mut v: *const c_char = std::ptr::null();
        (unsafe { (self.api.provider_find)(c.as_ptr(), &mut m, &mut v) } == EVT_OK).then(|| (self.entry(&m), unsafe { owned(v) }))
    }

    /// A game / loader state value (`"match.soccer_mode"`, `"match.in_match"`, `"loader.modules_mask"`).
    pub fn state(&self, key: &str) -> Option<i64> {
        let c = cstring(key);
        let mut v = 0i64;
        (unsafe { (self.api.game_state)(c.as_ptr(), &mut v) } == EVT_OK).then_some(v)
    }

    /// Bytes of nie.exe `.text` as they were before any patch (None outside `.text`).
    pub fn code_clean(&self, addr: usize, n: usize) -> Option<Vec<u8>> {
        let mut v = vec![0u8; n];
        (unsafe { (self.api.code_read_clean)(addr, v.as_mut_ptr() as *mut c_void, n) } == EVT_OK).then_some(v)
    }

    /// Pointer / vtable slot hook (`orig` receives the previous value).
    ///
    /// # Safety
    /// `slot` must be a pointer slot the game calls through, `detour` of the exact signature.
    pub unsafe fn hook_ptr(&self, slot: usize, detour: *const (), orig: &'static AtomicUsize) -> Result<(), i32> {
        let r = (self.api.hook_ptr)(self.h, slot, detour as *const c_void, orig.as_ptr());
        if r == EVT_OK {
            Ok(())
        } else {
            Err(r)
        }
    }

    /// `"game_dir"`, `"loader_dir"` (evt_loader), `"mods_dir"`.
    pub fn path(&self, key: &str) -> Option<PathBuf> {
        let k = cstring(key);
        let n = unsafe { (self.api.path_get)(k.as_ptr(), std::ptr::null_mut(), 0) };
        if n == 0 {
            return None;
        }
        let mut buf = vec![0u8; n + 1];
        unsafe { (self.api.path_get)(k.as_ptr(), buf.as_mut_ptr() as *mut c_char, buf.len()) };
        buf.truncate(n);
        Some(PathBuf::from(String::from_utf8_lossy(&buf).into_owned()))
    }

    /// Does the loader's API table reach the function at byte offset `field_end - 8` (appended fields may be missing
    /// on an older loader)? `field_end` = offset of the field + 8.
    pub fn has(&self, field_end: usize) -> bool {
        self.api.size as usize >= field_end
    }

    /// `EVT_PHASE_*` (`EVT_PHASE_NONE` on a loader without the function).
    pub fn phase(&self) -> u32 {
        if !self.has(core::mem::offset_of!(EvtApi, phase) + 8) {
            return EVT_PHASE_NONE;
        }
        unsafe { (self.api.phase)() }
    }

    /// Serve `game_path` from `disk_path` (early phase at the entry point only; see [`EvtApi::file_serve`]).
    /// `Err(EVT_E_STATE)` also on a loader without the function.
    pub fn file_serve(&self, game_path: &str, disk_path: &std::path::Path) -> Result<(), i32> {
        if !self.has(core::mem::offset_of!(EvtApi, file_serve) + 8) {
            return Err(EVT_E_STATE);
        }
        let (g, d) = (cstring(game_path), cstring(&disk_path.to_string_lossy()));
        let r = unsafe { (self.api.file_serve)(self.h, g.as_ptr(), d.as_ptr()) };
        if r == EVT_OK {
            Ok(())
        } else {
            Err(r)
        }
    }

    /// A file on disk with the game's own bytes of `game_path` (no mods overlay); None when unknown / unsupported.
    pub fn game_file_path(&self, game_path: &str) -> Option<PathBuf> {
        if !self.has(core::mem::offset_of!(EvtApi, game_file_path) + 8) {
            return None;
        }
        let g = cstring(game_path);
        let n = unsafe { (self.api.game_file_path)(g.as_ptr(), std::ptr::null_mut(), 0) };
        if n == 0 {
            return None;
        }
        let mut buf = vec![0u8; n + 1];
        unsafe { (self.api.game_file_path)(g.as_ptr(), buf.as_mut_ptr() as *mut c_char, buf.len()) };
        buf.truncate(n);
        Some(PathBuf::from(String::from_utf8_lossy(&buf).into_owned()))
    }

    /// Start a named thread through the loader (`plugin-<mod id>-<name>`); panics in `f` are caught and logged.
    pub fn spawn(&self, name: &str, f: impl FnOnce() + Send + 'static) -> bool {
        let b: Box<Box<dyn FnOnce() + Send>> = Box::new(Box::new(f));
        let user = Box::into_raw(b) as *mut c_void;
        let c = cstring(name);
        let r = unsafe { (self.api.thread_spawn)(self.h, c.as_ptr(), thread_trampoline, user) };
        if r != EVT_OK {
            drop(unsafe { Box::from_raw(user as *mut Box<dyn FnOnce() + Send>) });
            return false;
        }
        true
    }
}

unsafe extern "C" fn thread_trampoline(user: *mut c_void) {
    let f = Box::from_raw(user as *mut Box<dyn FnOnce() + Send>);
    if std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || f())).is_err() {
        if let Some(h) = try_host() {
            h.log(Level::Error, "thread panicked");
        }
    }
}

/// One Lua command invocation (valid during the handler).
pub struct LuaCall {
    raw: *mut EvtLuaCall,
    api: &'static EvtApi,
}

impl LuaCall {
    pub fn nargs(&self) -> i32 {
        unsafe { (self.api.lua_nargs)(self.raw) }
    }
    /// `EVT_LUA_*` of argument `i` (0-based).
    pub fn arg_type(&self, i: i32) -> i32 {
        unsafe { (self.api.lua_arg_type)(self.raw, i) }
    }
    pub fn num(&self, i: i32) -> Option<f64> {
        let mut v = 0f64;
        (unsafe { (self.api.lua_arg_num)(self.raw, i, &mut v) } == EVT_OK).then_some(v)
    }
    pub fn int(&self, i: i32) -> Option<i64> {
        self.num(i).map(|v| v as i64)
    }
    pub fn string(&self, i: i32) -> Option<String> {
        let n = unsafe { (self.api.lua_arg_str)(self.raw, i, std::ptr::null_mut(), 0) };
        if n < 0 {
            return None;
        }
        let mut buf = vec![0u8; n as usize + 1];
        unsafe { (self.api.lua_arg_str)(self.raw, i, buf.as_mut_ptr() as *mut c_char, buf.len()) };
        buf.truncate(n as usize);
        Some(String::from_utf8_lossy(&buf).into_owned())
    }
    pub fn push_num(&mut self, v: f64) {
        unsafe { (self.api.lua_push_num)(self.raw, v) }
    }
    pub fn push_int(&mut self, v: i64) {
        self.push_num(v as f64)
    }
    pub fn push_bool(&mut self, v: bool) {
        unsafe { (self.api.lua_push_bool)(self.raw, v as i32) }
    }
    pub fn push_str(&mut self, s: &str) {
        let c = cstring(s);
        unsafe { (self.api.lua_push_str)(self.raw, c.as_ptr()) }
    }
}

unsafe extern "C" fn lua_trampoline(call: *mut EvtLuaCall, user: *mut c_void) {
    let Some(h) = try_host() else { return };
    let f: fn(&mut LuaCall) = std::mem::transmute::<*mut c_void, fn(&mut LuaCall)>(user);
    let mut c = LuaCall { raw: call, api: h.api };
    if std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| f(&mut c))).is_err() {
        h.log(Level::Error, "Lua command handler panicked");
    }
}

/// Body of `evt_plugin_init` / `evt_plugin_early` generated by [`declare_plugin!`] (not for direct use).
#[doc(hidden)]
pub unsafe fn __run_phase(
    api: *const EvtApi,
    info: *const EvtPluginInfo,
    phase: &str,
    f: fn(&'static Host) -> Result<(), String>,
) -> i32 {
    if api.is_null() || info.is_null() {
        return EVT_E_ARG;
    }
    let a: &'static EvtApi = &*api;
    // the v1 functions must be there; the ones appended later are checked where they are used (`Host::has`)
    if a.api_version < EVT_PLUGIN_API_VERSION || a.size < EVT_API_SIZE_V1 {
        return EVT_E_STATE;
    }
    let i = &*info;
    let host = HOST.get_or_init(|| Host {
        api: a,
        h: i.handle,
        mod_id: owned(i.mod_id),
        mod_version: owned(i.mod_version),
        mod_dir: PathBuf::from(owned(i.mod_dir)),
        loader_version: owned(i.loader_version),
        load_index: i.load_index,
    });
    match std::panic::catch_unwind(|| f(host)) {
        Ok(Ok(())) => EVT_OK,
        Ok(Err(e)) => {
            host.log(Level::Error, &format!("{phase} failed: {e}"));
            EVT_E_STATE
        }
        Err(_) => {
            host.log(Level::Error, &format!("{phase} panicked"));
            EVT_E_STATE
        }
    }
}

/// Body of `evt_plugin_shutdown` generated by [`declare_plugin!`] (not for direct use).
#[doc(hidden)]
pub fn __run_shutdown(f: fn()) {
    let _ = std::panic::catch_unwind(f);
}

/// Export the plugin entry points: `evt_plugin_api_version`, `evt_plugin_init` (calls `init(&'static Host) ->
/// Result<(), String>`), with `early = g` also `evt_plugin_early` (the early phase, main thread, before any game code)
/// and with `shutdown = f` `evt_plugin_shutdown`. Order of the keys: init, early, shutdown.
#[macro_export]
macro_rules! declare_plugin {
    (init = $init:path $(, early = $early:path)? $(, shutdown = $shut:path)? $(,)?) => {
        #[no_mangle]
        pub extern "C" fn evt_plugin_api_version() -> u32 {
            $crate::EVT_PLUGIN_API_VERSION
        }
        #[no_mangle]
        pub unsafe extern "C" fn evt_plugin_init(api: *const $crate::EvtApi, info: *const $crate::EvtPluginInfo) -> i32 {
            $crate::__run_phase(api, info, "init", $init)
        }
        $(
            #[no_mangle]
            pub unsafe extern "C" fn evt_plugin_early(api: *const $crate::EvtApi, info: *const $crate::EvtPluginInfo) -> i32 {
                $crate::__run_phase(api, info, "early phase", $early)
            }
        )?
        $(
            #[no_mangle]
            pub extern "C" fn evt_plugin_shutdown() {
                $crate::__run_shutdown($shut)
            }
        )?
    };
}

/// `evt_log!(Level::Info, "x {}", 1)`: a formatted line through the host (dropped before init).
#[macro_export]
macro_rules! evt_log {
    ($lvl:expr, $($arg:tt)*) => {
        if let Some(h) = $crate::try_host() {
            h.log($lvl, &format!($($arg)*));
        }
    };
}
#[macro_export]
macro_rules! evt_error { ($($arg:tt)*) => { $crate::evt_log!($crate::Level::Error, $($arg)*) }; }
#[macro_export]
macro_rules! evt_warn { ($($arg:tt)*) => { $crate::evt_log!($crate::Level::Warn, $($arg)*) }; }
#[macro_export]
macro_rules! evt_info { ($($arg:tt)*) => { $crate::evt_log!($crate::Level::Info, $($arg)*) }; }
#[macro_export]
macro_rules! evt_debug { ($($arg:tt)*) => { $crate::evt_log!($crate::Level::Debug, $($arg)*) }; }
