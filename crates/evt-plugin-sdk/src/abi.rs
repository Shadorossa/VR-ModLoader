//! C ABI of ModLoader plugin API **v1** (mirror of `sdk/evt_plugin.h`; docs/app/modloader-plugins.md).
//!
//! Stability rules: [`EvtApi`] / [`EvtPluginInfo`] / [`EvtModInfo`] are `#[repr(C)]`, 64-bit Windows only. A new
//! API version only **appends** fields at the end of [`EvtApi`] (its `size` says how many the loader has); nothing
//! is ever moved, removed or retyped. A plugin built for API v`N` runs on every loader with `api_version >= N`.

use core::ffi::{c_char, c_void};

/// Plugin API version of this SDK (what `evt_plugin_api_version` returns).
pub const EVT_PLUGIN_API_VERSION: u32 = 1;

// ---------------------------------------------------------------- result codes (i32)
pub const EVT_OK: i32 = 0;
/// Bad argument (NULL, bad UTF-8, bad pattern, too many args...).
pub const EVT_E_ARG: i32 = -1;
/// Not found (signature, IAT import, mod, state key...).
pub const EVT_E_NOT_FOUND: i32 = -2;
/// Wrong moment or module off (e.g. Lua registration after init, or `lua_bridge = false`).
pub const EVT_E_STATE: i32 = -3;
/// Memory access or guarded call raised an exception.
pub const EVT_E_FAULT: i32 = -4;
/// Already registered / already hooked with the same `next` slot / command name taken.
pub const EVT_E_CONFLICT: i32 = -5;
/// Hook installation failed (prologue mismatch, VirtualProtect...): details in loader.log.
pub const EVT_E_HOOK: i32 = -6;

// ---------------------------------------------------------------- log levels
pub const EVT_LOG_ERROR: i32 = 0;
pub const EVT_LOG_WARN: i32 = 1;
pub const EVT_LOG_INFO: i32 = 2;
pub const EVT_LOG_DEBUG: i32 = 3;
pub const EVT_LOG_TRACE: i32 = 4;

// ---------------------------------------------------------------- Lua value types (Lua 5.2)
pub const EVT_LUA_NONE: i32 = -1;
pub const EVT_LUA_NIL: i32 = 0;
pub const EVT_LUA_BOOLEAN: i32 = 1;
pub const EVT_LUA_NUMBER: i32 = 3;
pub const EVT_LUA_STRING: i32 = 4;

// ---------------------------------------------------------------- plugin states (EvtModInfo::plugin_state)
pub const EVT_PLUGIN_NONE: u32 = 0;
pub const EVT_PLUGIN_LOADED: u32 = 1;
pub const EVT_PLUGIN_FAILED: u32 = 2;
/// Listed, not loaded yet (plugins load one by one in load order).
pub const EVT_PLUGIN_PENDING: u32 = 3;

/// Opaque handle of one plugin (given in [`EvtPluginInfo::handle`]; first argument of the per-plugin functions).
#[repr(C)]
pub struct EvtPlugin {
    _opaque: [u8; 0],
}

/// Opaque Lua command invocation (valid only during the handler call).
#[repr(C)]
pub struct EvtLuaCall {
    _opaque: [u8; 0],
}

/// Handler of a Lua command: read the arguments (`lua_arg_*`, index 0 = first argument after the command hash) and
/// push the results (`lua_push_*`). Runs on the game's Lua thread: keep it short.
pub type EvtLuaHandler = unsafe extern "C" fn(call: *mut EvtLuaCall, user: *mut c_void);
/// Body of a thread started with `thread_spawn`.
pub type EvtThreadFn = unsafe extern "C" fn(user: *mut c_void);

/// One active mod (strings owned by the loader, valid for the whole process).
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct EvtModInfo {
    pub id: *const c_char,
    pub version: *const c_char,
    /// Absolute path of the mod folder, UTF-8.
    pub dir: *const c_char,
    /// Plugin DLL file name, "" = none.
    pub plugin: *const c_char,
    /// Position in the load order (0 = loads first).
    pub load_index: u32,
    /// `EVT_PLUGIN_*`.
    pub plugin_state: u32,
}

impl Default for EvtModInfo {
    fn default() -> Self {
        EvtModInfo {
            id: core::ptr::null(),
            version: core::ptr::null(),
            dir: core::ptr::null(),
            plugin: core::ptr::null(),
            load_index: 0,
            plugin_state: 0,
        }
    }
}

/// What the loader tells a plugin about itself (valid during `evt_plugin_init`; the strings stay valid forever).
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct EvtPluginInfo {
    /// `sizeof(EvtPluginInfo)` of the loader.
    pub size: u32,
    /// Position of the mod in the load order.
    pub load_index: u32,
    pub handle: *const EvtPlugin,
    pub mod_id: *const c_char,
    pub mod_version: *const c_char,
    /// Absolute path of the mod folder, UTF-8.
    pub mod_dir: *const c_char,
    /// ModLoader version (`"1.0.0"`).
    pub loader_version: *const c_char,
}

/// The function table of plugin API v1. Every function is thread-safe unless noted.
#[repr(C)]
pub struct EvtApi {
    /// Plugin API version of the loader (>= 1).
    pub api_version: u32,
    /// `sizeof(EvtApi)` of the loader: fields past it do not exist.
    pub size: u32,
    /// ModLoader version (`"1.0.0"`).
    pub loader_version: *const c_char,

    // ---- log (`evt_loader\loader.log`, every line prefixed `<mod id>: `)
    pub log: unsafe extern "C" fn(h: *const EvtPlugin, level: i32, msg: *const c_char),
    /// Write the queued log lines to the file now (before a hard exit).
    pub log_flush: unsafe extern "C" fn(),

    // ---- configuration
    /// The mod's configuration as TOML text: `<mod>\config.toml`, overridden by the `[<id>]` section (legacy of a
    /// built-in module) and then by `[mods.<id>]` of `evt_loader\config.toml`. Writes at most `cap - 1` bytes + NUL;
    /// returns the full length (call again with a bigger buffer when it is `>= cap`).
    pub config_get: unsafe extern "C" fn(h: *const EvtPlugin, buf: *mut c_char, cap: usize) -> usize,

    // ---- nie.exe code and memory
    /// Base address of nie.exe.
    pub exe_base: unsafe extern "C" fn() -> usize,
    /// Unique match of an IDA-style pattern (`"48 8B ?? 05"`) in nie.exe `.text` → address. `expected_rva` (0 = none)
    /// is only compared for the log. Results are cached; a pattern that also belongs to a built-in module resolves
    /// on the unpatched code.
    pub sig_find: unsafe extern "C" fn(h: *const EvtPlugin, name: *const c_char, pattern: *const c_char, expected_rva: u32, out: *mut usize) -> i32,
    /// Target of a RIP-relative operand: `insn + next_ip_off + *(i32*)(insn + disp_off)`.
    pub rip_target: unsafe extern "C" fn(insn: usize, disp_off: u32, next_ip_off: u32, out: *mut usize) -> i32,
    /// Guarded copy from game memory (`EVT_E_FAULT` on an access violation).
    pub mem_read: unsafe extern "C" fn(addr: usize, dst: *mut c_void, n: usize) -> i32,
    /// Guarded copy into game **data** (never code: use the hook functions).
    pub mem_write: unsafe extern "C" fn(addr: usize, src: *const c_void, n: usize) -> i32,
    /// Call game code `func(args[0..nargs])` (0..=6 integer / pointer args, Microsoft x64) under SEH.
    pub call_guarded: unsafe extern "C" fn(func: usize, args: *const u64, nargs: u32, ret: *mut u64) -> i32,

    // ---- hooks
    /// Chained inline hook of `target`. The first hook of a target steals `len` (>= 14) bytes that must equal
    /// `prologue` (whole instructions, no relative operands); later hooks of the same target (other plugins or the
    /// loader's built-in modules) join its chain and ignore `prologue`. `*next` (a `usize` that lives forever, read
    /// atomically) always holds what the detour must call to continue: the next detour of the chain or the original
    /// function. Order: higher `priority` runs first (outer); equal priority: the mod that loads later runs first;
    /// the loader's built-in hooks run after every plugin hook of priority >= 0.
    pub hook_inline: unsafe extern "C" fn(
        h: *const EvtPlugin,
        target: usize,
        prologue: *const u8,
        len: usize,
        detour: *const c_void,
        next: *mut usize,
        priority: i32,
    ) -> i32,
    /// IAT hook: `module` (NULL = nie.exe) imports `dll!func` by name; its slot is set to `detour` and the previous
    /// value stored in `*orig` first. Several hooks of one slot chain naturally (the last one runs first).
    pub hook_iat: unsafe extern "C" fn(
        h: *const EvtPlugin,
        module: *const c_char,
        dll: *const c_char,
        func: *const c_char,
        detour: *const c_void,
        orig: *mut usize,
    ) -> i32,

    // ---- Lua commands (module lua_bridge; only during evt_plugin_init)
    /// Register `name` (`"CMND_EVT_MY_THING"`, hash = crc32 of the name as every CMND_EVT_*).
    pub lua_register: unsafe extern "C" fn(h: *const EvtPlugin, name: *const c_char, handler: EvtLuaHandler, user: *mut c_void) -> i32,
    /// Register by hash (`label` is only for the log).
    pub lua_register_hash:
        unsafe extern "C" fn(h: *const EvtPlugin, hash: u32, label: *const c_char, handler: EvtLuaHandler, user: *mut c_void) -> i32,
    pub lua_nargs: unsafe extern "C" fn(c: *mut EvtLuaCall) -> i32,
    /// `EVT_LUA_*` of argument `i` (0-based).
    pub lua_arg_type: unsafe extern "C" fn(c: *mut EvtLuaCall, i: i32) -> i32,
    pub lua_arg_num: unsafe extern "C" fn(c: *mut EvtLuaCall, i: i32, out: *mut f64) -> i32,
    /// String argument: copies at most `cap - 1` bytes + NUL, returns the full length, -1 when not a string.
    pub lua_arg_str: unsafe extern "C" fn(c: *mut EvtLuaCall, i: i32, buf: *mut c_char, cap: usize) -> isize,
    pub lua_push_num: unsafe extern "C" fn(c: *mut EvtLuaCall, v: f64),
    pub lua_push_bool: unsafe extern "C" fn(c: *mut EvtLuaCall, v: i32),
    pub lua_push_str: unsafe extern "C" fn(c: *mut EvtLuaCall, s: *const c_char),

    // ---- active mods (load order)
    pub mod_count: unsafe extern "C" fn() -> u32,
    pub mod_get: unsafe extern "C" fn(index: u32, out: *mut EvtModInfo) -> i32,
    pub mod_find: unsafe extern "C" fn(id: *const c_char, out: *mut EvtModInfo) -> i32,
    /// The last-loading active mod whose id is `name` or that `provides` it; `*version` = the provided version.
    pub provider_find: unsafe extern "C" fn(name: *const c_char, out: *mut EvtModInfo, version: *mut *const c_char) -> i32,

    // ---- game state
    /// `"match.soccer_mode"` (1/0, NOT_FOUND while unknown), `"match.in_match"` (1/0), `"loader.modules_mask"`.
    /// Unknown keys: NOT_FOUND (a loader build with more modules may answer more keys).
    pub game_state: unsafe extern "C" fn(key: *const c_char, out: *mut i64) -> i32,

    // ---- threads
    /// Start a named thread (`plugin-<mod id>-<name>`) running `f(user)`.
    pub thread_spawn: unsafe extern "C" fn(h: *const EvtPlugin, name: *const c_char, f: EvtThreadFn, user: *mut c_void) -> i32,

    // ---- clean code, pointer hooks, paths
    /// Copy bytes of nie.exe `.text` **as they were before any patch** (snapshot taken in DllMain). `EVT_E_NOT_FOUND`
    /// outside `.text`. (`sig_find` and `rip_target` already search / read this copy.)
    pub code_read_clean: unsafe extern "C" fn(addr: usize, dst: *mut c_void, n: usize) -> i32,
    /// Pointer / vtable slot hook: `*orig` = the slot's value, then the slot = `detour` (write-protected slots are
    /// handled). Several hooks of one slot chain naturally (the last one runs first).
    pub hook_ptr: unsafe extern "C" fn(h: *const EvtPlugin, slot: usize, detour: *const c_void, orig: *mut usize) -> i32,
    /// A path, UTF-8: `"game_dir"`, `"loader_dir"` (`evt_loader`), `"mods_dir"`. Same buffer contract as
    /// `config_get`; 0 = unknown key.
    pub path_get: unsafe extern "C" fn(key: *const c_char, buf: *mut c_char, cap: usize) -> usize,

    // ---- appended 29/09/2026 (still API v1: check `size` before using them, see `EVT_API_SIZE_V1`)
    /// Serve `game_path` (`data/...`) from `disk_path` (a file **below the game folder**, e.g. under
    /// `path_get("cache_dir")`), through the same overlay as the mods' `files\` (it wins over a mod's whole file of
    /// that path). Only in the early phase at the exe entry point (`phase() == EVT_PHASE_EARLY`), before the engine
    /// opens its first file: later → `EVT_E_STATE`. Another plugin serving the same path → `EVT_E_CONFLICT`.
    pub file_serve: unsafe extern "C" fn(h: *const EvtPlugin, game_path: *const c_char, disk_path: *const c_char) -> i32,
    /// `EVT_PHASE_*`: where the loader is (early phase at the entry point, early phase run late, init, running).
    pub phase: unsafe extern "C" fn() -> u32,
    /// Path of a file on disk holding the GAME's own bytes of `game_path` (the installed loose file, or a copy
    /// extracted from its CPK into `evt_loader\cache\game\`), without the mods overlay. Same buffer contract as
    /// `config_get`; 0 = not a game file.
    pub game_file_path: unsafe extern "C" fn(game_path: *const c_char, buf: *mut c_char, cap: usize) -> usize,
}

/// `sizeof(EvtApi)` of the first v1 loaders (29 functions): the fields past it may be missing on an older loader.
pub const EVT_API_SIZE_V1: u32 = 16 + 8 * 29;

// ---------------------------------------------------------------- phases (`EvtApi::phase`)
pub const EVT_PHASE_NONE: u32 = 0;
/// `evt_plugin_early` at the exe entry point: game main thread, before any game code (`file_serve` allowed).
pub const EVT_PHASE_EARLY: u32 = 1;
/// `evt_plugin_early` run late from the loader's init thread (the entry point could not be armed): the game already
/// runs, files may already be open.
pub const EVT_PHASE_EARLY_LATE: u32 = 2;
/// `evt_plugin_init` of the plugins (init thread).
pub const EVT_PHASE_INIT: u32 = 3;
/// The game runs (after the early phase at the entry point, and after every init).
pub const EVT_PHASE_RUNNING: u32 = 4;

/// `evt_plugin_api_version`: the API version the plugin was built against.
pub type EvtPluginApiVersionFn = unsafe extern "C" fn() -> u32;
/// `evt_plugin_init`: 0 = OK; anything else = the plugin is disabled (its hooks and commands are removed).
pub type EvtPluginInitFn = unsafe extern "C" fn(api: *const EvtApi, info: *const EvtPluginInfo) -> i32;
/// `evt_plugin_early` (optional, same signature as init): the **early phase**, on the game's main thread at its CRT
/// entry point (loader lock released, before any game code or C++ static initializer runs). For hooks that must be in
/// place before the game starts (IAT of Steam, file loaders...). 0 = OK; anything else disables the plugin.
pub type EvtPluginEarlyFn = unsafe extern "C" fn(api: *const EvtApi, info: *const EvtPluginInfo) -> i32;
/// `evt_plugin_shutdown` (optional): called when init failed, to clean up.
pub type EvtPluginShutdownFn = unsafe extern "C" fn();

pub const EXPORT_API_VERSION: &str = "evt_plugin_api_version";
pub const EXPORT_INIT: &str = "evt_plugin_init";
pub const EXPORT_EARLY: &str = "evt_plugin_early";
pub const EXPORT_SHUTDOWN: &str = "evt_plugin_shutdown";

#[cfg(test)]
mod tests {
    use super::*;
    use core::mem::{offset_of, size_of};

    /// The layout the C header documents (x64): a change here breaks every built plugin.
    #[test]
    fn api_v1_layout() {
        assert_eq!(offset_of!(EvtApi, api_version), 0);
        assert_eq!(offset_of!(EvtApi, size), 4);
        assert_eq!(offset_of!(EvtApi, loader_version), 8);
        let order = [
            offset_of!(EvtApi, log),
            offset_of!(EvtApi, log_flush),
            offset_of!(EvtApi, config_get),
            offset_of!(EvtApi, exe_base),
            offset_of!(EvtApi, sig_find),
            offset_of!(EvtApi, rip_target),
            offset_of!(EvtApi, mem_read),
            offset_of!(EvtApi, mem_write),
            offset_of!(EvtApi, call_guarded),
            offset_of!(EvtApi, hook_inline),
            offset_of!(EvtApi, hook_iat),
            offset_of!(EvtApi, lua_register),
            offset_of!(EvtApi, lua_register_hash),
            offset_of!(EvtApi, lua_nargs),
            offset_of!(EvtApi, lua_arg_type),
            offset_of!(EvtApi, lua_arg_num),
            offset_of!(EvtApi, lua_arg_str),
            offset_of!(EvtApi, lua_push_num),
            offset_of!(EvtApi, lua_push_bool),
            offset_of!(EvtApi, lua_push_str),
            offset_of!(EvtApi, mod_count),
            offset_of!(EvtApi, mod_get),
            offset_of!(EvtApi, mod_find),
            offset_of!(EvtApi, provider_find),
            offset_of!(EvtApi, game_state),
            offset_of!(EvtApi, thread_spawn),
            offset_of!(EvtApi, code_read_clean),
            offset_of!(EvtApi, hook_ptr),
            offset_of!(EvtApi, path_get),
            // appended 29/09
            offset_of!(EvtApi, file_serve),
            offset_of!(EvtApi, phase),
            offset_of!(EvtApi, game_file_path),
        ];
        for (k, off) in order.iter().enumerate() {
            assert_eq!(*off, 16 + 8 * k, "EvtApi function #{k}");
        }
        assert_eq!(offset_of!(EvtApi, file_serve), EVT_API_SIZE_V1 as usize);
        assert_eq!(size_of::<EvtApi>(), 16 + 8 * 32);
        assert_eq!(size_of::<EvtPluginInfo>(), 48);
        assert_eq!(offset_of!(EvtPluginInfo, handle), 8);
        assert_eq!(offset_of!(EvtPluginInfo, loader_version), 40);
        assert_eq!(size_of::<EvtModInfo>(), 40);
        assert_eq!(offset_of!(EvtModInfo, load_index), 32);
        assert_eq!(offset_of!(EvtModInfo, plugin_state), 36);
    }
}
