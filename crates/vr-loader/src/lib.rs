//! VR-ModLoader: `winmm.dll` proxy mod loader for Inazuma Eleven Victory Road PC **v7.1.2**.
//!
//! See `crates/vr-loader/README.md`. Layout:
//! * pure, unit-tested modules: [`config`], [`scan`], [`sigs`], [`pe`], [`gate`], [`log`], [`platform`], [`registry`];
//! * Windows run-time: [`proxy`] (export forwarding), [`hook`] (chained inline / IAT hooks), [`game`] (signature
//!   resolution, SEH-guarded calls), [`lua`] (command bridge), [`runtime`] (DllMain / init);
//! * built-in modules: [`mods`] (mod folders, file overlay, data deltas, voice packs), [`lua_patch`] (Lua patch
//!   runner), [`plugins`] (native plugin host, API v1), [`console`], [`debug`] (Lua errors, hang watchdog),
//!   [`quit_fix`], [`chara_legal`], [`match_state`].

pub mod chara_legal;
pub mod config;
pub mod console;
pub mod debug;
pub mod gate;
pub mod log;
pub mod lua_patch;
pub mod match_state;
pub mod mods;
pub mod pe;
pub mod platform;
pub mod plugins;
pub mod quit_fix;
pub mod registry;
pub mod scan;
pub mod sigs;

#[cfg(all(windows, target_arch = "x86_64"))]
pub mod game;
#[cfg(all(windows, target_arch = "x86_64"))]
pub mod hook;
#[cfg(all(windows, target_arch = "x86_64"))]
pub mod lua;
#[cfg(all(windows, target_arch = "x86_64"))]
pub mod proxy;
#[cfg(all(windows, target_arch = "x86_64"))]
pub mod runtime;

/// Lua API version reported by `CMND_EVT_LOADER_VERSION`.
pub const API_VERSION: i64 = 1;
pub const LOADER_VERSION: &str = env!("CARGO_PKG_VERSION");
/// Public ModLoader version (what a mod's `loader_min` is compared with; docs/app/modloader-plugins.md). 1.0.0 =
/// dependency order, `provides`, `loader_min`, `loader_modules` turned on by mods, native plugins (API v1).
pub const MODLOADER_VERSION: &str = "1.0.0";

/// DLL entry point (called by the CRT's `_DllMainCRTStartup`).
#[cfg(all(windows, target_arch = "x86_64"))]
#[no_mangle]
pub extern "system" fn DllMain(hinst: *mut core::ffi::c_void, reason: u32, _reserved: *mut core::ffi::c_void) -> i32 {
    if reason == windows_sys::Win32::System::SystemServices::DLL_PROCESS_ATTACH {
        crate::runtime::on_process_attach(hinst);
    } else if reason == windows_sys::Win32::System::SystemServices::DLL_PROCESS_DETACH {
        // the async log may still hold a few lines (never blocks: other threads may be gone)
        crate::log::try_flush();
    }
    1
}
