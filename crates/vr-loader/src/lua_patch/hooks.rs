//! Run-time part of module `lua_patch` (see the module docs). Both hooks are installed in DllMain and stay
//! pass-through until [`activate`] (init thread, after the v7.1.2 gate) has set the patch roots and read the
//! fingerprint files.

use super::sigs as lsig;
use super::{
    build_chunk, patch_files_roots_in, script_stem, select, Fingerprints, LuaPatchCfg, MatchMode, PatchFile, Probe, Via,
    DEFAULT_MARKERS, FINGERPRINTS_FILE, FOLDER, PROBE_CHUNK,
};
use crate::game::{read_ptr, LuaApi, Text};
use crate::log::Level;
use crate::{debug, error, info, warn};
use std::ffi::CString;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Mutex, OnceLock};
use windows_sys::Win32::System::Threading::GetCurrentThreadId;

type OpenFn = unsafe extern "C" fn(*mut u8, *const u8, u32, u8, u32) -> *mut u8;
type LoadChunkFn = unsafe extern "C" fn(*mut u8, *mut u8, u64, u64) -> u8;
type LoadFn = unsafe extern "C-unwind" fn(*mut u8, *const u8, usize, *const u8, *const u8) -> i32;
type PcallFn = unsafe extern "C-unwind" fn(*mut u8, i32, i32, i32, i32, usize) -> i32;
type SettopFn = unsafe extern "C-unwind" fn(*mut u8, i32);

#[derive(Clone, Copy)]
struct Api {
    lua: LuaApi,
    load: LoadFn,
    pcall: PcallFn,
    settop: SettopFn,
}

static API: OnceLock<Api> = OnceLock::new();
/// Patch roots in run order: `("", <data_dir>/lua_patches)` when module `lua_patch` is on, then `(mod id,
/// <mod>/lua)` of module `mods`.
static ROOTS: OnceLock<Vec<(String, PathBuf)>> = OnceLock::new();
/// The merged `_fingerprints.json` of every root (empty in name mode or without files).
static FPS: OnceLock<Fingerprints> = OnceLock::new();
/// `[lua_patch]`: match mode and pre-filter switch.
static MODE: OnceLock<(MatchMode, bool)> = OnceLock::new();
static ACTIVE: AtomicBool = AtomicBool::new(false);
static ORIG_OPEN: AtomicUsize = AtomicUsize::new(0);
static ORIG_LOAD_CHUNK: AtomicUsize = AtomicUsize::new(0);
/// Last `.lua.bin` / `.lua` path opened, per thread: `(thread id, sequence, path)`; newest sequence wins.
static LAST_LUA: Mutex<Vec<(u32, u64, String)>> = Mutex::new(Vec::new());
static SEQ: AtomicU64 = AtomicU64::new(0);
/// Threads currently running patch files (re-entrancy guard).
static RUNNING: Mutex<Vec<u32>> = Mutex::new(Vec::new());
const LAST_MAX: usize = 16;
const LUA_TNIL: i32 = 0;
const LUA_TSTRING: i32 = 4;

fn install(text: &Text, sig: &crate::sigs::Sig, steal: usize, detour: usize, orig: &AtomicUsize) -> bool {
    let Some(target) = text.resolve(sig) else { return false };
    let Some(expected) = crate::scan::Pattern::parse(sig.pattern).ok().and_then(|p| p.fixed_prefix(steal)) else {
        error!("lua_patch: {} steal bytes contain wildcards", sig.name);
        return false;
    };
    match unsafe { crate::hook::inline_hook(target, &expected, detour, orig) } {
        Ok(_) => {
            info!("lua_patch: {} hooked at 0x{target:X}", sig.name);
            true
        }
        Err(e) => {
            error!("lua_patch: {} hook failed: {e}", sig.name);
            false
        }
    }
}

/// DllMain: resolve the Lua functions and install the two hooks (pass-through until [`activate`]). No game thread
/// runs yet. False = the module cannot work (nothing patched, or the file open alone).
pub fn install_hooks(text: &Text) -> bool {
    let api = (|| unsafe {
        use std::mem::transmute;
        Some(Api {
            lua: LuaApi::resolve(text)?,
            load: transmute::<usize, LoadFn>(text.resolve(&lsig::LP_LOADBUFFERX)?),
            pcall: transmute::<usize, PcallFn>(text.resolve(&lsig::LP_PCALLK)?),
            settop: transmute::<usize, SettopFn>(text.resolve(&lsig::LP_SETTOP)?),
        })
    })();
    let Some(api) = api else {
        error!("lua_patch: Lua API signatures not resolved: module disabled");
        return false;
    };
    let _ = API.set(api);
    let open = install(text, &lsig::LP_FILE_OPEN, lsig::FILE_OPEN_STEAL, open_detour as *const () as usize, &ORIG_OPEN);
    if !open {
        return false; // no script names: nothing could be matched
    }
    install(text, &lsig::LP_LOAD_CHUNK, lsig::LOAD_CHUNK_STEAL, load_chunk_detour as *const () as usize, &ORIG_LOAD_CHUNK)
}

/// Init thread (after the gate): set the roots, read the fingerprint files and go live. `legacy` = module
/// `lua_patch` is on: its folder `<data_dir>/lua_patches` is created (so the user finds it) and runs first; `mods` =
/// `(id, <mod>/lua)` of the enabled mods (module `mods`), in load order.
pub fn activate(data_dir: &Path, legacy: bool, mods: Vec<(String, PathBuf)>, cfg: &LuaPatchCfg) -> bool {
    if API.get().is_none() || ORIG_LOAD_CHUNK.load(Ordering::Acquire) == 0 {
        return false;
    }
    let mut roots = Vec::new();
    if legacy {
        let dir = data_dir.join(FOLDER);
        if let Err(e) = std::fs::create_dir_all(&dir) {
            warn!("lua_patch: cannot create {}: {e}", dir.display());
        }
        let scripts = std::fs::read_dir(&dir)
            .map(|rd| rd.flatten().filter(|e| e.path().is_dir() && !e.file_name().to_string_lossy().starts_with('_')).count())
            .unwrap_or(0);
        info!("lua_patch: active: {} ({scripts} script folder(s))", dir.display());
        roots.push((String::new(), dir));
    }
    for (id, dir) in &mods {
        info!("lua_patch: mod {id}: {}", dir.display());
    }
    roots.extend(mods);
    let mode = cfg.mode();
    let mut fps = Fingerprints::default();
    if mode == MatchMode::Fingerprint {
        for (label, root) in &roots {
            let p = root.join(FINGERPRINTS_FILE);
            match Fingerprints::load(&p) {
                Ok(Some(f)) => {
                    let n = f.entries.len();
                    let amb = f.entries.iter().filter(|(_, fp)| !fp.ambiguous.is_empty()).count();
                    for note in fps.merge(f, if label.is_empty() { "lua_patches" } else { label }) {
                        warn!("lua_patch: {note}");
                    }
                    info!("lua_patch: fingerprints: {n} stem(s), {amb} ambiguous, from {}", p.display());
                }
                Ok(None) => {}
                Err(e) => error!("lua_patch: {e}: fingerprints of that root ignored"),
            }
        }
        if fps.entries.is_empty() {
            warn!("lua_patch: no {FINGERPRINTS_FILE} in any root: scripts matched by the opened path only (menu_assemble.py build writes it)");
        }
        if fps.markers.is_empty() {
            fps.markers = DEFAULT_MARKERS.iter().map(|s| s.to_string()).collect();
        }
        info!(
            "lua_patch: match = fingerprint, pre-filter {} ({})",
            if cfg.prefilter { "on" } else { "off" },
            fps.markers.join(", ")
        );
    } else {
        info!("lua_patch: match = name (the opened .lua.bin path only)");
    }
    let _ = ROOTS.set(roots);
    let _ = FPS.set(fps);
    let _ = MODE.set((mode, cfg.prefilter));
    ACTIVE.store(true, Ordering::Release);
    true
}

// ---------------------------------------------------------------- hooks

/// Bounded read of the C string at `p` (the engine's own path buffer).
unsafe fn c_str(p: *const u8, max: usize) -> Option<String> {
    if p.is_null() || (p as usize) < 0x10000 {
        return None;
    }
    let mut n = 0;
    while n < max && *p.add(n) != 0 {
        n += 1;
    }
    (n < max).then(|| String::from_utf8_lossy(std::slice::from_raw_parts(p, n)).into_owned())
}

fn remember(tid: u32, path: String) {
    let seq = SEQ.fetch_add(1, Ordering::Relaxed) + 1;
    let mut v = LAST_LUA.lock().unwrap_or_else(|e| e.into_inner());
    v.retain(|e| e.0 != tid);
    if v.len() >= LAST_MAX {
        v.remove(0);
    }
    v.push((tid, seq, path));
}

/// The path remembered for this thread (removed), else the newest of any thread (logged), else None.
fn take_last(tid: u32) -> Option<(String, bool)> {
    let mut v = LAST_LUA.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(i) = v.iter().position(|e| e.0 == tid) {
        return Some((v.remove(i).2, true));
    }
    let i = v.iter().enumerate().max_by_key(|(_, e)| e.1).map(|(i, _)| i)?;
    Some((v.remove(i).2, false))
}

unsafe extern "C" fn open_detour(this: *mut u8, path: *const u8, mode: u32, b: u8, flags: u32) -> *mut u8 {
    let orig: OpenFn = std::mem::transmute::<usize, OpenFn>(ORIG_OPEN.load(Ordering::Acquire));
    if ACTIVE.load(Ordering::Acquire) {
        if let Some(p) = c_str(path, 512) {
            if script_stem(&p).is_some() {
                remember(GetCurrentThreadId(), p);
            }
        }
    }
    orig(this, path, mode, b, flags)
}

unsafe extern "C" fn load_chunk_detour(holder: *mut u8, res: *mut u8, a: u64, b: u64) -> u8 {
    let orig: LoadChunkFn = std::mem::transmute::<usize, LoadChunkFn>(ORIG_LOAD_CHUNK.load(Ordering::Acquire));
    let r = orig(holder, res, a, b);
    if r == 0 || !ACTIVE.load(Ordering::Acquire) || holder.is_null() {
        return r;
    }
    let (Some(api), Some(roots), Some(fps), Some(&(mode, prefilter))) = (API.get(), ROOTS.get(), FPS.get(), MODE.get()) else {
        return r;
    };
    let tid = GetCurrentThreadId();
    {
        let mut g = RUNNING.lock().unwrap_or_else(|e| e.into_inner());
        if g.contains(&tid) {
            return r; // a patch file made the engine load another script object on this thread
        }
        g.push(tid);
    }
    let out = std::panic::catch_unwind(|| {
        let Some(l) = read_ptr(holder as usize + lsig::HOLDER_L) else {
            debug!("lua_patch: script object 0x{:X} without a lua_State", holder as usize);
            return;
        };
        let l = l as *mut u8;
        // the opened path: `_all`'s label, stems without a fingerprint, ties between twins (name mode: everything)
        let name = take_last(tid).and_then(|(path, own)| {
            let stem = script_stem(&path)?;
            if !own {
                debug!("lua_patch: {stem}: path opened on another thread ({path})");
            }
            Some(stem)
        });
        let mut eval = |src: &str| probe(api, l, src);
        let sel = select(fps, name.as_deref(), mode, prefilter, &mut eval);
        for (lvl, line) in &sel.notes {
            match lvl {
                Level::Error => error!("lua_patch: {line}"),
                Level::Warn => warn!("lua_patch: {line} (vm 0x{:X})", l as usize),
                Level::Info => info!("lua_patch: {line} (vm 0x{:X})", l as usize),
                _ => debug!("lua_patch: {line} (vm 0x{:X})", l as usize),
            }
        }
        if let Some(i) = sel.mismatch {
            if crate::log::enabled(Level::Debug) {
                let (k, fp) = &fps.entries[i];
                let why = super::mismatch_reason(fp, &mut eval);
                debug!("lua_patch: {k}: the opened path names it but the VM does not match its fingerprint ({why}): not run by name (vm 0x{:X})", l as usize);
            }
        }
        for g in &sel.groups {
            let files = patch_files_roots_in(roots, &g.dirs);
            if files.is_empty() {
                if g.via != Via::All {
                    debug!("lua_patch: {}: no patch files", g.script);
                }
                continue;
            }
            run_patches(api, l, &g.script, &files);
        }
    });
    if out.is_err() {
        error!("lua_patch: panic while running patch files");
    }
    RUNNING.lock().unwrap_or_else(|e| e.into_inner()).retain(|t| *t != tid);
    r
}

// ---------------------------------------------------------------- probes and patch files (game thread, inside the VM)

unsafe fn stack_string(api: &Api, l: *mut u8, idx: i32) -> String {
    if (api.lua.type_)(l, idx) != LUA_TSTRING {
        return format!("(error object of type {})", (api.lua.type_)(l, idx));
    }
    let mut len = 0usize;
    let p = (api.lua.tolstring)(l, idx, &mut len);
    if p.is_null() {
        return "(no message)".into();
    }
    String::from_utf8_lossy(std::slice::from_raw_parts(p, len)).into_owned()
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

/// Compile + run a probe chunk (`return <expr> or nil`) in `l`: [`Probe::Yes`] when it returned a non-nil value.
/// The stack is left as it was. Only the three Lua functions the module already calls are used.
unsafe fn probe(api: &Api, l: *mut u8, src: &str) -> Probe {
    let top = (api.lua.gettop)(l);
    let Ok(cname) = CString::new(PROBE_CHUNK) else { return Probe::Error("chunk name".into()) };
    let r = (api.load)(l, src.as_ptr(), src.len(), cname.as_ptr() as *const u8, std::ptr::null());
    if r != 0 {
        let msg = stack_string(api, l, -1).lines().next().unwrap_or("").to_string();
        (api.settop)(l, top);
        return Probe::Error(format!("{}: {msg}", status_name(r)));
    }
    let r = (api.pcall)(l, 0, 1, 0, 0, 0);
    let out = if r != 0 {
        Probe::Error(format!("{}: {}", status_name(r), stack_string(api, l, -1).lines().next().unwrap_or("")))
    } else if (api.lua.gettop)(l) > top && (api.lua.type_)(l, -1) != LUA_TNIL {
        Probe::Yes
    } else {
        Probe::No
    };
    (api.settop)(l, top);
    out
}

/// Load + run the patch `files` in `l` with `EVT_PATCH.script = script`. The stack is left as it was.
unsafe fn run_patches(api: &Api, l: *mut u8, script: &str, files: &[PatchFile]) {
    let top = (api.lua.gettop)(l);
    let mut ran = 0usize;
    let mut failed = 0usize;
    for f in files {
        let text = match std::fs::read(&f.path) {
            Ok(t) => t,
            Err(e) => {
                error!("lua_patch: {script}: {}: cannot read: {e}", f.name);
                failed += 1;
                continue;
            }
        };
        let chunk = build_chunk(script, &f.name, &text);
        let Ok(cname) = CString::new(format!("{}{}", super::CHUNK_PREFIX, f.name)) else { continue };
        let r = (api.load)(l, chunk.as_ptr(), chunk.len(), cname.as_ptr() as *const u8, std::ptr::null());
        if r != 0 {
            error!("lua_patch: {script}: {}: {}: {}", f.name, status_name(r), stack_string(api, l, -1).lines().next().unwrap_or(""));
            (api.settop)(l, top);
            failed += 1;
            continue;
        }
        let r = (api.pcall)(l, 0, 0, 0, 0, 0);
        if r != 0 {
            let msg = stack_string(api, l, -1);
            let mut lines = msg.lines();
            error!("lua_patch: {script}: {}: {}: {}", f.name, status_name(r), lines.next().unwrap_or(""));
            for line in lines.take(30) {
                error!("lua_patch:   | {}", line.trim_end());
            }
            (api.settop)(l, top);
            failed += 1;
            continue;
        }
        debug!("lua_patch: {script}: {} ok", f.name);
        ran += 1;
    }
    (api.settop)(l, top);
    // which root / mod each file came from (global folder first, then the mods in load order)
    let from = super::sources_summary(files.iter().map(|f| f.name.as_str()));
    if failed == 0 {
        info!("lua_patch: {script}: ran {ran} files [{from}]");
    } else {
        info!("lua_patch: {script}: ran {ran} files ({failed} failed, see errors above) [{from}]");
    }
}

/// Init thread, after [`activate`]: warn about patches of different roots that overwrite each other and about
/// scripts a mod replaces whole while another root patches them ([`super::root_conflicts`]). `overrides` = module
/// `mods`' whole-file overrides (key -> winning mod id). Skipped when only the global folder is active.
pub fn report_conflicts<'a>(overrides: impl IntoIterator<Item = (&'a String, &'a String)>) {
    let Some(roots) = ROOTS.get() else { return };
    if roots.iter().all(|(l, _)| l.is_empty()) {
        return;
    }
    let lines = super::root_conflicts(roots, &super::full_scripts(overrides));
    for line in &lines {
        warn!("lua_patch: conflict: {line}");
    }
    info!("lua_patch: {} root(s) ({}), {} conflict(s)", roots.len(), roots.iter().map(|(l, _)| if l.is_empty() { FOLDER } else { l.as_str() }).collect::<Vec<_>>().join(" -> "), lines.len());
}
