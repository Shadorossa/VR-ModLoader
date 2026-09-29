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
/// Public ModLoader version (what a mod's `loader_min` is compared with) = the package version of this crate. 1.0.0 =
/// dependency order, `provides`, `loader_min`, `loader_modules` turned on by mods, native plugins (API v1).
pub const MODLOADER_VERSION: &str = env!("CARGO_PKG_VERSION");

const VERSION_MARKER_STR: &str = concat!("EVT_MODLOADER_VERSION=", env!("CARGO_PKG_VERSION"), "\0");

const fn version_marker_bytes() -> [u8; VERSION_MARKER_STR.len()] {
    let src = VERSION_MARKER_STR.as_bytes();
    let mut out = [0u8; VERSION_MARKER_STR.len()];
    let mut i = 0;
    while i < src.len() {
        out[i] = src[i];
        i += 1;
    }
    out
}

/// `EVT_MODLOADER_VERSION=<version>\0` in the DLL's read-only data: VR-ModLoader.exe and evt-installer read the
/// version of an installed `winmm.dll` from the file (`evt_installer::modpack::scan_version_marker`). `#[used]` and
/// the read in [`version_marker`] (logged at start) keep it through LTO and the linker's `/OPT:REF`.
#[used]
static VERSION_MARKER: [u8; VERSION_MARKER_STR.len()] = version_marker_bytes();

/// The version in the embedded marker (read through `black_box`, so the static is referenced by live code).
pub fn version_marker() -> &'static str {
    let bytes: &'static [u8] = core::hint::black_box(&VERSION_MARKER);
    let v = &bytes[b"EVT_MODLOADER_VERSION=".len()..bytes.len() - 1];
    core::str::from_utf8(v).unwrap_or("?")
}

#[cfg(test)]
mod version_marker_tests {
    #[test]
    fn marker_holds_the_package_version() {
        assert_eq!(super::version_marker(), env!("CARGO_PKG_VERSION"));
        assert_eq!(super::MODLOADER_VERSION, env!("CARGO_PKG_VERSION"));
        let m = &super::VERSION_MARKER;
        assert!(m.starts_with(b"EVT_MODLOADER_VERSION="));
        assert_eq!(m.last(), Some(&0));
        let v = &m[22..m.len() - 1];
        assert!(!v.is_empty() && v.iter().all(|b| b.is_ascii_alphanumeric() || b"._-+".contains(b)), "scan_version_marker reads it whole");
    }
}

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
