//! Run-time part of module `mods` (see the module docs): the `fs.ResolveOverlayPath` detour. Installed in DllMain
//! with the overlay already built, so it serves mod files from the first open on.
//!
//! Two ways to hand a served file to the engine:
//! * **cpk_list mode** (normal): once the engine has published cpk_list, every served file gets a loose record in
//!   an in-memory copy of the list, under its root-relative path (`mods/<id>/files/data/...`, [`super::cpklist`]),
//!   and `CCriFileOperate+0x170` is set. The detour then writes that relative path: `Open` finds the record and runs
//!   the branch of a file the app installs loose (size at open time, `BindFile` of `<root>/<path>`).
//! * **absolute path** (fallback: before cpk_list is loaded, or when a served file cannot be registered): the
//!   detour writes the mod file's absolute path and `Open` binds it without any cpk_list record (size unknown until
//!   the asynchronous bind completes). This is what the first version did; icons served that way came out blank.

use super::cpklist::{self, Rec};
use super::sigs as msig;
use super::Overlay;
use crate::game::Text;
use crate::{debug, error, info, warn};
use std::collections::{HashMap, HashSet};
use std::sync::atomic::{fence, AtomicU8, AtomicUsize, Ordering};
use std::sync::{Mutex, OnceLock};
use windows_sys::Win32::System::Threading::{EnterCriticalSection, LeaveCriticalSection, CRITICAL_SECTION};

type ResolveFn = unsafe extern "C" fn(*mut u8, *const u8, *mut u8) -> *mut u8;

static OVERLAY: OnceLock<Overlay> = OnceLock::new();
static ORIG: AtomicUsize = AtomicUsize::new(0);
/// Keys already reported as served (one INFO line per file per session).
static SERVED: Mutex<Option<HashSet<String>>> = Mutex::new(None);
/// Engine paths logged at DEBUG so far (the first few show the path form the engine uses).
static PROBED: AtomicUsize = AtomicUsize::new(0);
const PROBE_MAX: usize = 12;

/// cpk_list mode: waiting for the engine's list / active / off (absolute paths for the whole session).
const LIST_WAIT: u8 = 0;
const LIST_ON: u8 = 1;
const LIST_OFF: u8 = 2;
static LIST_MODE: AtomicU8 = AtomicU8::new(LIST_WAIT);
/// The `CCriFileOperate` that got the records, and the records array published there (a list reload replaces it).
static LIST_FO: AtomicUsize = AtomicUsize::new(0);
static LIST_RECS: AtomicUsize = AtomicUsize::new(0);
/// Overlay key -> root-relative C path written into `out` while the list mode is on.
static REL: OnceLock<HashMap<String, Vec<u8>>> = OnceLock::new();
/// Serialises (re)registration (always taken after the engine's file lock).
static LIST_LOCK: Mutex<()> = Mutex::new(());

/// The overlay of DllMain until the engine's first file open, when the files plugins added with `file_serve` (plugin
/// API, early phase) are merged in and [`OVERLAY`] is sealed.
static PENDING: Mutex<Option<Overlay>> = Mutex::new(None);

/// The sealed overlay (first call: DllMain's + the plugins' files). Hot path after that: one `OnceLock::get`.
fn overlay() -> Option<&'static Overlay> {
    if let Some(o) = OVERLAY.get() {
        return Some(o);
    }
    let o = OVERLAY.get_or_init(|| PENDING.lock().unwrap_or_else(|e| e.into_inner()).take().unwrap_or_default());
    Some(o)
}

/// Plugin API `file_serve`: serve `key` from `r` too (it wins over a mod's whole file of the same key). Err once the
/// overlay is sealed (the engine opened a file) or when another plugin already serves `key`. Ok(previous winner).
pub fn serve_extra(key: &str, r: super::Redirect) -> Result<Option<String>, String> {
    if OVERLAY.get().is_some() {
        return Err("too late: the engine already opens files (call it in evt_plugin_early)".into());
    }
    let mut g = PENDING.lock().unwrap_or_else(|e| e.into_inner());
    if OVERLAY.get().is_some() {
        return Err("too late: the engine already opens files (call it in evt_plugin_early)".into());
    }
    let o = g.get_or_insert_with(Overlay::default);
    let prev = o.map.get(key).map(|p| p.module.clone());
    if let Some(p) = prev.as_deref().filter(|p| p.ends_with("(plugin)") && *p != r.module) {
        return Err(format!("{key} is already served by {p}"));
    }
    o.map.insert(key.to_string(), r);
    Ok(prev)
}

/// DllMain: hook `ResolveOverlayPath` with `overlay` (nothing is hooked for an empty overlay unless a plugin may add
/// files: `plugins`). Logging is not open yet: the caller keeps the error and logs it from the init thread.
pub fn install_hook(text: &Text, overlay: Overlay) -> Result<usize, String> {
    install_hook_with(text, overlay, false)
}

/// [`install_hook`], also hooking with an empty overlay when `plugins` (a plugin may `file_serve`).
pub fn install_hook_with(text: &Text, overlay: Overlay, plugins: bool) -> Result<usize, String> {
    if overlay.map.is_empty() && !plugins {
        return Ok(0);
    }
    let n = overlay.map.len();
    {
        let mut g = PENDING.lock().unwrap_or_else(|e| e.into_inner());
        let mut base = overlay;
        if let Some(extra) = g.take() {
            base.map.extend(extra.map);
        }
        *g = Some(base);
    }
    let sig = &msig::MODS_RESOLVE_OVERLAY;
    let target = text.resolve(sig).ok_or_else(|| format!("{} not found", sig.name))?;
    let expected = crate::scan::Pattern::parse(sig.pattern)
        .ok()
        .and_then(|p| p.fixed_prefix(msig::RESOLVE_OVERLAY_STEAL))
        .ok_or_else(|| format!("{} steal bytes contain wildcards", sig.name))?;
    unsafe { crate::hook::inline_hook(target, &expected, detour as *const () as usize, &ORIG) }
        .map_err(|e| format!("{} hook failed: {e}", sig.name))?;
    Ok(n)
}

/// Bounded read of the engine's C string.
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

unsafe fn rd<T: Copy>(fo: *mut u8, off: usize) -> T {
    std::ptr::read_volatile(fo.add(off) as *const T)
}

unsafe fn wr<T: Copy>(fo: *mut u8, off: usize, v: T) {
    std::ptr::write_volatile(fo.add(off) as *mut T, v)
}

/// The engine's file lock (`CCriFileOperate+0x180`, recursive): held by `Open` / `GetFileSize` / `Read` around every
/// cpk_list access, so the records are swapped while none of them is searching.
struct EngineLock(*mut CRITICAL_SECTION);

impl EngineLock {
    unsafe fn take(fo: *mut u8) -> EngineLock {
        let cs = fo.add(msig::FO_LOCK) as *mut CRITICAL_SECTION;
        EnterCriticalSection(cs);
        EngineLock(cs)
    }
}

impl Drop for EngineLock {
    fn drop(&mut self) {
        unsafe { LeaveCriticalSection(self.0) }
    }
}

/// Absolute form of the engine's data root (`CCriFileOperate+0x18`, relative roots resolved against the current
/// directory). None when empty.
fn absolute_root(root: &str) -> Option<String> {
    if root.is_empty() {
        return None;
    }
    let r = root.replace('\\', "/");
    if r.as_bytes().get(1) == Some(&b':') {
        return Some(r);
    }
    let mut p = std::env::current_dir().ok()?;
    for c in std::path::Path::new(&r).components() {
        match c {
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                p.pop();
            }
            c => p.push(c),
        }
    }
    Some(p.to_str()?.replace('\\', "/"))
}

/// Root-relative C path of every served file, or the first one that cannot be registered (and why).
fn relative_paths(o: &Overlay, root: &str) -> Result<HashMap<String, Vec<u8>>, String> {
    let mut m = HashMap::with_capacity(o.map.len());
    for (key, r) in &o.map {
        let abs = String::from_utf8_lossy(r.cpath.strip_suffix(&[0]).unwrap_or(&r.cpath)).into_owned();
        let rel = cpklist::root_relative(&abs, root).ok_or_else(|| format!("{abs} is not below the data root {root}"))?;
        if r.size == 0 || r.size > i32::MAX as u64 {
            return Err(format!("{abs}: size {} not usable in a cpk_list record", r.size));
        }
        let mut v = rel.into_bytes();
        v.push(0);
        if v.len() > msig::OUT_BUF {
            return Err(format!("{abs}: relative path too long"));
        }
        m.insert(key.clone(), v);
    }
    Ok(m)
}

/// Copy the engine's list, add a loose record for every served file and publish the copy (the engine's lock must be
/// held). The old arrays stay allocated (lock-free readers may still be walking them). Returns the record count.
unsafe fn publish(fo: *mut u8, o: &Overlay, rel: &HashMap<String, Vec<u8>>) -> Result<usize, String> {
    let count: u64 = rd(fo, msig::FO_COUNT);
    let recs_p: usize = rd(fo, msig::FO_RECS);
    let pool_p: usize = rd(fo, msig::FO_POOL);
    if count == 0 || count > 16_000_000 || recs_p < 0x10000 || pool_p < 0x10000 {
        return Err(format!("cpk_list not usable (count {count}, records 0x{recs_p:X}, pool 0x{pool_p:X})"));
    }
    let recs = std::slice::from_raw_parts(recs_p as *const Rec, count as usize);
    let strlen = |off: u32| {
        let p = (pool_p + off as usize) as *const u8;
        (0..0x1000).find(|&i| *p.add(i) == 0)
    };
    let pool_len = cpklist::used_pool_len(recs, strlen).ok_or("cpk_list string pool not readable")?;
    let pool = std::slice::from_raw_parts(pool_p as *const u8, pool_len);
    let adds: Vec<(String, i32)> = o
        .map
        .iter()
        .filter_map(|(k, r)| {
            let p = rel.get(k)?;
            Some((String::from_utf8_lossy(&p[..p.len() - 1]).into_owned(), r.size as i32))
        })
        .collect();
    let (np, nr) = cpklist::with_loose_entries(pool, recs, &adds);
    if let Some((p, _)) = adds.iter().find(|(p, _)| cpklist::find(&np, &nr, p).is_none()) {
        return Err(format!("{p}: not found after registration"));
    }
    let n = nr.len();
    let np: &'static mut [u8] = Box::leak(np.into_boxed_slice());
    let nr: &'static mut [Rec] = Box::leak(nr.into_boxed_slice());
    // pool, then records, then count: a lock-free reader (the `exists` slot) that reads the count first never walks
    // past the end of the array it gets
    wr(fo, msig::FO_POOL, np.as_ptr() as usize);
    fence(Ordering::SeqCst);
    wr(fo, msig::FO_RECS, nr.as_ptr() as usize);
    fence(Ordering::SeqCst);
    wr(fo, msig::FO_COUNT, n as u64);
    fence(Ordering::SeqCst);
    wr(fo, msig::FO_OVERLAY_USES_LIST, 1u8);
    LIST_RECS.store(nr.as_ptr() as usize, Ordering::Release);
    Ok(n)
}

/// First detour call after the engine published cpk_list: register the served files (or switch the list mode off).
unsafe fn list_activate(fo: *mut u8, o: &Overlay) {
    if rd::<u64>(fo, msig::FO_COUNT) == 0 {
        return; // cpk_list not loaded yet (boot files): absolute paths meanwhile
    }
    let mut lines: Vec<(bool, String)> = Vec::new();
    {
        let _engine = EngineLock::take(fo);
        let _g = LIST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        if LIST_MODE.load(Ordering::Acquire) != LIST_WAIT {
            return;
        }
        let root = c_str(fo.add(msig::FO_ROOT), msig::FO_ROOT_MAX).unwrap_or_default();
        let res = absolute_root(&root)
            .ok_or_else(|| format!("engine data root {root:?} not usable"))
            .and_then(|abs_root| relative_paths(o, &abs_root).map(|m| (abs_root, m)))
            .and_then(|(abs_root, m)| {
                let rel = REL.get_or_init(|| m);
                publish(fo, o, rel).map(|n| (abs_root, n))
            });
        match res {
            Ok((abs_root, n)) => {
                LIST_FO.store(fo as usize, Ordering::Release);
                LIST_MODE.store(LIST_ON, Ordering::Release);
                lines.push((
                    false,
                    format!(
                        "mods: {} served file(s) registered in cpk_list (in memory, loose, root-relative; {n} records; root {abs_root})",
                        o.map.len()
                    ),
                ));
            }
            Err(e) => {
                LIST_MODE.store(LIST_OFF, Ordering::Release);
                lines.push((true, format!("mods: cpk_list registration off ({e}): served files get their absolute path")));
            }
        }
    }
    for (w, l) in lines {
        if w {
            warn!("{l}");
        } else {
            info!("{l}");
        }
    }
}

/// Root-relative path of `key` while the list mode is on for `fo`. Re-registers when the engine replaced the
/// records (a cpk_list reload); if that fails the mode goes off and the byte goes back to 0 (absolute paths).
unsafe fn list_path(fo: *mut u8, o: &Overlay, key: &str) -> Option<&'static [u8]> {
    if LIST_MODE.load(Ordering::Acquire) != LIST_ON || fo as usize != LIST_FO.load(Ordering::Acquire) {
        return None;
    }
    if rd::<usize>(fo, msig::FO_RECS) != LIST_RECS.load(Ordering::Acquire) || rd::<u8>(fo, msig::FO_OVERLAY_USES_LIST) == 0 {
        let mut err = None;
        {
            let _engine = EngineLock::take(fo);
            let _g = LIST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
            if rd::<usize>(fo, msig::FO_RECS) != LIST_RECS.load(Ordering::Acquire) || rd::<u8>(fo, msig::FO_OVERLAY_USES_LIST) == 0 {
                if let Err(e) = publish(fo, o, REL.get()?) {
                    wr(fo, msig::FO_OVERLAY_USES_LIST, 0u8);
                    LIST_MODE.store(LIST_OFF, Ordering::Release);
                    err = Some(e);
                }
            }
        }
        match err {
            Some(e) => {
                warn!("mods: cpk_list was reloaded and the served files could not be registered again ({e}): absolute paths from now on");
                return None;
            }
            None => info!("mods: cpk_list was reloaded by the engine: served files registered again"),
        }
    }
    REL.get()?.get(key).map(|v| v.as_slice())
}

/// Diagnostic path trace (docs/game/media/cs-map-port.md §15): every engine path containing one of the lines of
/// `<game>\evt_loader\trace_paths.txt` is logged (no file = off). Read once, on the first resolved path.
static TRACE: OnceLock<Vec<String>> = OnceLock::new();
static TRACED: AtomicUsize = AtomicUsize::new(0);

fn trace_list() -> &'static [String] {
    TRACE.get_or_init(|| {
        let f = std::env::current_exe().ok().and_then(|e| e.parent().map(|d| d.join("evt_loader").join("trace_paths.txt")));
        let list: Vec<String> = f
            .and_then(|f| std::fs::read_to_string(f).ok())
            .map(|t| t.lines().map(|l| l.trim().to_string()).filter(|l| !l.is_empty() && !l.starts_with('#')).collect())
            .unwrap_or_default();
        if !list.is_empty() {
            info!("mods: path trace on for {list:?} (evt_loader\\trace_paths.txt)");
        }
        list
    })
}

unsafe extern "C" fn detour(this: *mut u8, path: *const u8, out: *mut u8) -> *mut u8 {
    let orig: ResolveFn = std::mem::transmute::<usize, ResolveFn>(ORIG.load(Ordering::Acquire));
    if let (Some(o), false) = (overlay().filter(|o| !o.map.is_empty()), out.is_null() || this.is_null()) {
        if LIST_MODE.load(Ordering::Acquire) == LIST_WAIT {
            list_activate(this, o);
        }
        if let Some(p) = c_str(path, 1024) {
            let tl = trace_list();
            if !tl.is_empty() && tl.iter().any(|t| p.contains(t.as_str())) && TRACED.fetch_add(1, Ordering::Relaxed) < 2000 {
                info!("mods: trace: engine path {p:?}");
            }
            if PROBED.load(Ordering::Relaxed) < PROBE_MAX {
                PROBED.fetch_add(1, Ordering::Relaxed);
                debug!("mods: engine path {p:?}");
            }
            // voice pack language: a retail voice bank (ja / en) or a shared root bank (bgm, bgm_chronicle…) the
            // pack ships wins over everything else
            let voice = super::voice::active_code()
                .filter(|_| p.contains("sound_asset"))
                .and_then(|code| super::voice::resolve_any(o, &p, code));
            if let Some((key, r)) = voice.or_else(|| o.lookup(&p)) {
                let rel = list_path(this, o, key);
                let cpath: &[u8] = rel.unwrap_or(&r.cpath);
                std::ptr::copy_nonoverlapping(cpath.as_ptr(), out, cpath.len());
                let mut g = SERVED.lock().unwrap_or_else(|e| e.into_inner());
                if g.get_or_insert_with(HashSet::new).insert(key.to_string()) {
                    drop(g);
                    let how = if rel.is_some() { "cpk_list loose record" } else { "absolute path" };
                    info!("mods: {key} served from {} ({}, {how})", r.module, String::from_utf8_lossy(&cpath[..cpath.len() - 1]));
                }
                // non-NULL = "overlay hit": the callers only test the result, then use `out` as the path
                return this;
            }
        }
    }
    orig(this, path, out)
}

/// The mod file that serves game path `key` (`data/...`), when the overlay is installed and a mod overrides it
pub fn overlay_file(key: &str) -> Option<std::path::PathBuf> {
    // never seals the overlay (a module may ask before the engine's first open)
    let r = match OVERLAY.get() {
        Some(o) => o.lookup(key)?.1.clone(),
        None => PENDING.lock().unwrap_or_else(|e| e.into_inner()).as_ref()?.lookup(key)?.1.clone(),
    };
    let p = r.cpath.strip_suffix(&[0]).unwrap_or(&r.cpath);
    Some(std::path::PathBuf::from(String::from_utf8_lossy(p).into_owned()))
}

/// Init thread: log an install failure kept from DllMain.
pub fn log_install_error(e: &str) {
    error!("mods: {e}: file overrides NOT active");
}

#[cfg(test)]
mod tests {
    use super::*;

    fn red(m: &str, p: &str) -> super::super::Redirect {
        let mut v = p.as_bytes().to_vec();
        v.push(0);
        super::super::Redirect { module: m.into(), cpath: v, size: 3 }
    }

    /// Plugin `file_serve` (the only test touching the process-wide overlay): open until the first engine open.
    #[test]
    fn plugin_files_join_the_overlay_until_it_is_sealed() {
        let k = "data/common/sound/sound_queue_sheet.cfg.bin";
        assert_eq!(serve_extra(k, red("audio_engine (plugin)", "D:/g/evt_loader/cache/a")), Ok(None));
        // the same plugin again: replaces; another plugin: conflict
        assert!(serve_extra(k, red("audio_engine (plugin)", "D:/g/evt_loader/cache/b")).is_ok());
        assert!(serve_extra(k, red("other (plugin)", "D:/g/x")).unwrap_err().contains("served by"));
        // readable before sealing, without sealing
        assert!(overlay_file(k).is_some_and(|p| p.to_string_lossy().ends_with("cache/b")));
        assert!(OVERLAY.get().is_none());
        let o = overlay().unwrap();
        assert!(o.lookup(k).is_some());
        assert!(serve_extra("data/x.bin", red("audio_engine (plugin)", "D:/g/y")).unwrap_err().contains("too late"));
    }
}
