//! Run time of plugin `clean_hud`: the pre-kick-off editor skip (built-in module `prematch`, same patch), through the
//! ModLoader API (guarded reads, log, config) plus its own stub page and code write (plugin API v1 has no
//! `alloc_exec_near` / protected code write yet: docs/app/modloader-split.md §5c A7). The loader prefixes every line
//! with `clean_hud: `; the lines keep the built-in's `prematch: ...` text after it.

use super::*;
use evt_plugin_sdk::{declare_plugin, host, Host, Level};
use std::sync::atomic::{AtomicU32, AtomicUsize, Ordering};
use windows_sys::Win32::System::Diagnostics::Debug::FlushInstructionCache;
use windows_sys::Win32::System::Memory::{
    VirtualAlloc, VirtualProtect, MEM_COMMIT, MEM_RESERVE, PAGE_EXECUTE_READWRITE, PAGE_PROTECTION_FLAGS,
};
use windows_sys::Win32::System::Threading::GetCurrentProcess;

/// Address of the session global (`[rip+d32]` of `0x1239C70`).
static NET_VAR: AtomicUsize = AtomicUsize::new(0);
static SKIPS: AtomicU32 = AtomicU32::new(0);

fn log(level: Level, msg: &str) {
    host().log(level, msg);
}
macro_rules! info { ($($a:tt)*) => { log(Level::Info, &format!($($a)*)) }; }
macro_rules! warn { ($($a:tt)*) => { log(Level::Warn, &format!($($a)*)) }; }

fn read<T: Copy>(a: usize) -> Option<T> {
    host().read::<T>(a)
}

fn network() -> Option<bool> {
    let var = NET_VAR.load(Ordering::Acquire);
    let g = host().read_ptr(var)?;
    network_active(g, read::<u8>, read::<u32>)
}

/// Called by the stub on the game thread, only when `mi & 0x2000000` is set. 1 = skip the editor.
extern "C" fn decide(mi: usize) -> u8 {
    std::panic::catch_unwind(|| {
        let observer = read::<u8>(mi + MI_OFF_OBSERVER);
        let net = network();
        let ty = read::<u8>(mi + MI_OFF_TYPE).unwrap_or(0xFF);
        if should_skip(observer, net) {
            let n = SKIPS.fetch_add(1, Ordering::AcqRel) + 1;
            info!("prematch: pre-kick-off team editor skipped (offline, match type {ty}) -> walk-in [{n}]");
            1
        } else {
            info!("prematch: pre-kick-off team editor kept (network {net:?}, observer {observer:?}, match type {ty})");
            0
        }
    })
    .unwrap_or(0)
}

fn bytes_at(addr: usize, n: usize) -> Vec<u8> {
    (0..n).map(|i| read::<u8>(addr + i).unwrap_or(0)).collect()
}

/// `SizeOfImage` of nie.exe from its PE headers (guarded reads).
fn size_of_image(base: usize) -> Option<usize> {
    let e_lfanew = read::<u32>(base + 0x3C)? as usize;
    (read::<u32>(base + e_lfanew)? == 0x0000_4550).then_some(())?;
    // IMAGE_NT_HEADERS64: Signature (4) + FileHeader (20) + OptionalHeader.SizeOfImage at +56
    read::<u32>(base + e_lfanew + 4 + 20 + 56).map(|v| v as usize)
}

/// One RWX page below the image, within rel32 reach of all of it (like the built-in `office::alloc_near`).
fn alloc_near(base: usize, size: usize) -> Option<usize> {
    const GRAN: usize = 0x10000;
    let floor = near_floor(base, size).max(GRAN);
    let mut a = (base & !(GRAN - 1)).saturating_sub(GRAN);
    while a > floor {
        let p = unsafe { VirtualAlloc(a as _, 0x1000, MEM_COMMIT | MEM_RESERVE, PAGE_EXECUTE_READWRITE) } as usize;
        if p != 0 {
            return Some(p);
        }
        a -= GRAN;
    }
    None
}

/// Code write with the page made writable for the copy, then its protection restored and the i-cache flushed (the
/// built-in `hook::write_code`).
unsafe fn write_code(addr: usize, bytes: &[u8]) -> bool {
    let mut old: PAGE_PROTECTION_FLAGS = 0;
    if VirtualProtect(addr as _, bytes.len(), PAGE_EXECUTE_READWRITE, &mut old) == 0 {
        return false;
    }
    std::ptr::copy_nonoverlapping(bytes.as_ptr(), addr as *mut u8, bytes.len());
    let mut tmp: PAGE_PROTECTION_FLAGS = 0;
    VirtualProtect(addr as _, bytes.len(), old, &mut tmp);
    FlushInstructionCache(GetCurrentProcess(), addr as _, bytes.len());
    true
}

fn config(host: &Host) -> CleanHudCfg {
    let t = host.config_text();
    match toml::from_str::<CleanHudCfg>(&t) {
        Ok(c) => c,
        Err(e) => {
            warn!("configuration invalid ({}): defaults used", e.message());
            CleanHudCfg::default()
        }
    }
}

/// Verify the v7.1.2 bytes, build the stub near the image, patch the site. Any mismatch: nothing patched (the editor
/// opens as in retail). Returns whether the skip is on.
fn prematch_init(host: &Host) -> bool {
    let base = host.exe_base();
    let site = base + SITE_RVA as usize;
    let ctx = bytes_at(base + CONTEXT_RVA as usize, CONTEXT_BYTES.len());
    let cur = bytes_at(site, SITE_BYTES.len());
    if ctx != CONTEXT_BYTES || cur != SITE_BYTES {
        warn!("prematch: BattleSettingFlow state 6 at RVA 0x{SITE_RVA:X} holds {ctx:02X?} {cur:02X?}, not the \
               v7.1.2 bytes: not patched (retail pre-kick-off editor)");
        return false;
    }
    let net_fn = base + NET_FN_RVA as usize;
    let head = bytes_at(net_fn, 3);
    let tail = bytes_at(net_fn + 7, NET_FN_TAIL.len());
    let Some(disp) = read::<i32>(net_fn + 3).filter(|_| head == [0x48, 0x8B, 0x15] && tail == NET_FN_TAIL) else {
        warn!("prematch: network predicate at RVA 0x{NET_FN_RVA:X} not the v7.1.2 bytes: not patched");
        return false;
    };
    NET_VAR.store((net_fn as i64 + 7 + disp as i64) as usize, Ordering::Release);
    let Some(page) = size_of_image(base).and_then(|size| alloc_near(base, size)) else {
        warn!("prematch: no stub page near .text: not patched");
        return false;
    };
    let code = stub_code(page);
    let decide_fn: extern "C" fn(usize) -> u8 = decide;
    unsafe {
        std::ptr::copy_nonoverlapping(code.as_ptr(), page as *mut u8, code.len());
        std::ptr::write_volatile((page + DECIDE_OFF) as *mut u64, decide_fn as usize as u64);
        std::ptr::write_volatile((page + EDITOR_ABS_OFF) as *mut u64, (base + EDITOR_PATH_RVA as usize) as u64);
        std::ptr::write_volatile((page + NO_EDITOR_ABS_OFF) as *mut u64, (base + NO_EDITOR_PATH_RVA as usize) as u64);
    }
    let Some(patch) = site_patch(site, page) else {
        warn!("prematch: stub out of reach: not patched");
        return false;
    };
    if unsafe { write_code(site, &patch) } {
        info!("prematch: pre-kick-off team editor off in offline matches (BattleSettingFlow state 6, RVA 0x{SITE_RVA:X})");
        true
    } else {
        warn!("prematch: write failed: not patched");
        false
    }
}

/// `evt_plugin_init` (init thread, before the built-in modules: the built-in `prematch` yields to this plugin).
/// Never fails: with the skip off or not patched the plugin stays loaded (so the built-in still yields, the same
/// behaviour as `[modules] prematch = false`) and the retail editor opens.
fn init(host: &'static Host) -> Result<(), String> {
    let cfg = config(host);
    if !cfg.skip_prematch_editor {
        info!("prematch: skip_prematch_editor = false: the retail pre-kick-off team editor opens");
        return Ok(());
    }
    if !prematch_init(host) {
        warn!("prematch: not active (see above): the retail pre-kick-off team editor opens");
    }
    Ok(())
}

declare_plugin!(init = init);
