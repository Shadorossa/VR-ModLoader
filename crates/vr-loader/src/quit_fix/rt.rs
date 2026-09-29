//! Run time of module `quit_fix`: watcher thread + `ExitProcess` IAT hook.

use super::sigs as s;
use super::{classify_requests, state_name, QuitFixCfg, Reason, Snapshot, Step, Watch};
use crate::game::{read, seh_call, write, Text};
use crate::{error, info, warn};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::OnceLock;
use std::time::{Duration, Instant};
use windows_sys::Win32::System::Threading::{GetCurrentProcess, TerminateProcess};

/// Poll interval of the watcher.
const POLL_MS: u64 = 100;

#[derive(Debug, Clone, Copy)]
struct Addrs {
    quit_request: usize,
    quit_flag: usize,
    file_pending: usize,
    save_manager: usize,
    save_any_running: usize,
    /// `g_fileReqCapacity` / `g_fileReqArray` (None: the request scan is off, only the counter is used).
    file_capacity: Option<usize>,
    file_array: Option<usize>,
}

static ADDRS: OnceLock<Addrs> = OnceLock::new();
static ORIG_EXIT: AtomicUsize = AtomicUsize::new(0);
static TERMINATING: AtomicBool = AtomicBool::new(false);

fn snapshot(a: &Addrs, detail: bool) -> Snapshot {
    Snapshot {
        requested: read::<u8>(a.quit_request).is_some_and(|v| v != 0),
        quit_flag: read::<u8>(a.quit_flag).is_some_and(|v| v != 0),
        pending_files: read::<u16>(a.file_pending),
        in_flight_files: if detail { request_states(a).map(|v| classify_requests(v.iter().map(|x| x.1)).0) } else { None },
        save_busy: if detail { save_busy(a) } else { None },
    }
}

/// Non-free entries of the async file-request array: (entry address, state). Read-only; None = unreadable.
fn request_states(a: &Addrs) -> Option<Vec<(usize, i8)>> {
    let cap = read::<u16>(a.file_capacity?)? as usize;
    let arr = crate::game::read_ptr(a.file_array?)?;
    if cap == 0 || cap > 0x4000 {
        return None;
    }
    let mut v = Vec::new();
    for i in 0..cap {
        let e = arr + i * s::FR_SIZE;
        let st = read::<i8>(e + s::FR_STATE)?;
        if st != 0 {
            v.push((e, st));
        }
    }
    Some(v)
}

/// "state path" of the first queued requests, for the log.
fn describe_requests(a: &Addrs) -> String {
    let Some(v) = request_states(a) else { return "request list unreadable".into() };
    let (fly, kept) = classify_requests(v.iter().map(|x| x.1));
    let mut out = format!("{fly} in flight, {kept} finished but kept by their owner");
    for (e, st) in v.iter().take(8) {
        let raw = read::<[u8; s::FR_PATH_LEN]>(e + s::FR_PATH).unwrap_or([0; s::FR_PATH_LEN]);
        let end = raw.iter().position(|&c| c == 0).unwrap_or(raw.len());
        out.push_str(&format!("; [{} {}] {}", st, state_name(*st), String::from_utf8_lossy(&raw[..end])));
    }
    if v.len() > 8 {
        out.push_str(&format!("; ... {} more", v.len() - 8));
    }
    out
}

/// The retail save test of the quit gate: `CSaveManager::AnyJobRunning(g_saveManager)` (read-only list walk).
/// No manager = no job. None = the call faulted.
fn save_busy(a: &Addrs) -> Option<bool> {
    let mgr = read::<usize>(a.save_manager)?;
    if mgr < 0x10000 {
        return Some(false);
    }
    seh_call(a.save_any_running, mgr as u64, 0, 0, 0, 0).ok().map(|r| r & 0xFF != 0)
}

fn in_match() -> &'static str {
    match crate::match_state::soccer_mode() {
        Some(true) => "in a match",
        Some(false) => "not in a match",
        None => "match state unknown",
    }
}

fn fmt_pending(p: Option<u16>) -> String {
    p.map_or("?".into(), |n| n.to_string())
}

fn fmt_save(v: Option<bool>) -> &'static str {
    match v {
        Some(true) => "running",
        Some(false) => "idle",
        None => "unknown",
    }
}

/// Flush the log and end the process (never returns).
fn terminate(code: u32, why: &str) -> ! {
    TERMINATING.store(true, Ordering::Release);
    error!("quit_fix: {why}: TerminateProcess({code})");
    crate::log::flush();
    unsafe { TerminateProcess(GetCurrentProcess(), code) };
    loop {
        std::thread::sleep(Duration::from_secs(1));
    }
}

fn watch_thread(a: Addrs, cfg: QuitFixCfg) {
    let t0 = Instant::now();
    let mut w = Watch::new(&cfg);
    loop {
        std::thread::sleep(Duration::from_millis(POLL_MS));
        let now = t0.elapsed().as_millis() as u64;
        let snap = snapshot(&a, w.wants_detail());
        match w.step(now, &snap) {
            Step::Wait => {}
            Step::Requested => {
                let save = save_busy(&a);
                let reqs = if snap.pending_files.unwrap_or(1) > 0 { describe_requests(&a) } else { "none".into() };
                info!(
                    "quit_fix: close request seen (window hidden by WM_CLOSE; {}): retail gate: file requests pending {} ({}), save job {}, g_quit {}; grace {} s",
                    in_match(),
                    fmt_pending(snap.pending_files),
                    reqs,
                    fmt_save(save),
                    snap.quit_flag as u8,
                    cfg.grace_ms() / 1000
                );
            }
            Step::Cancelled => info!("quit_fix: close request withdrawn by the game; watching again"),
            Step::RetailQuit { ms, requested: true } => info!(
                "quit_fix: retail quit: the gate opened {ms} ms after the request (g_quit set by the game); shutdown running, guard {} s",
                cfg.grace_ms() / 1000
            ),
            Step::RetailQuit { requested: false, .. } => info!(
                "quit_fix: g_quit set by the game without a close request; shutdown guard {} s",
                cfg.grace_ms() / 1000
            ),
            Step::SaveBusy { waited_ms, known } => warn!(
                "quit_fix: retail gate still closed after the grace, but a save job is {} (waited {} ms, limit {} s): not cutting it",
                if known { "running" } else { "not checkable (counts as running)" },
                waited_ms,
                cfg.save_wait_ms() / 1000
            ),
            Step::SaveStuck { waited_ms } => error!(
                "quit_fix: the save job did not finish in {waited_ms} ms: treated as stuck (no progress), going on"
            ),
            Step::ForceQuitFlag { pending_files, parked_only } => {
                let ok = write::<u8>(a.quit_flag, 1);
                let why = if parked_only {
                    format!(
                        "retail gate closed only by finished requests (pending {}, none in flight, no save job)",
                        fmt_pending(pending_files)
                    )
                } else {
                    format!(
                        "retail gate still closed {} s after the request (file requests pending {}, {})",
                        cfg.grace_ms() / 1000,
                        fmt_pending(pending_files),
                        describe_requests(&a)
                    )
                };
                warn!(
                    "quit_fix: {why}, {}: g_quit = 1 written {} -> the main loop ends at its next frame and runs the retail shutdown (guard {} s)",
                    in_match(),
                    if ok { "OK" } else { "FAILED" },
                    cfg.grace_ms() / 1000
                );
            }
            Step::Terminate(Reason::GateBlocked { pending_files }) => terminate(
                0,
                &format!(
                    "retail gate blocked {} s after the request (file requests pending {}), retail_quit = false",
                    cfg.grace_ms() / 1000,
                    fmt_pending(pending_files)
                ),
            ),
            Step::Terminate(Reason::ShutdownHung { ms }) => terminate(
                0,
                &format!(
                    "process still alive {ms} ms after g_quit (the shutdown did not finish; file requests pending {}, save job {})",
                    fmt_pending(snap.pending_files),
                    // the save manager may already be freed by the shutdown: not called here
                    "not checked (shutdown)"
                ),
            ),
        }
        if w.done() {
            return;
        }
    }
}

/// `ExitProcess` of nie.exe (IAT): after a close request (or with `g_quit` set) -> `TerminateProcess`, so no DLL
/// detach handler can keep the process alive. Any other exit (error boxes, second instance) stays retail.
unsafe extern "system" fn hk_exit_process(code: u32) {
    let quitting = ADDRS.get().is_some_and(|a| {
        read::<u8>(a.quit_request).is_some_and(|v| v != 0) || read::<u8>(a.quit_flag).is_some_and(|v| v != 0)
    });
    if quitting && !TERMINATING.load(Ordering::Acquire) {
        terminate(code, &format!("ExitProcess({code}) after the close request -> TerminateProcess (no DLL detach)"));
    }
    let orig = ORIG_EXIT.load(Ordering::Acquire);
    if orig != 0 {
        let f: unsafe extern "system" fn(u32) = std::mem::transmute(orig);
        f(code);
    }
    // unreachable in practice (ExitProcess does not return)
    TerminateProcess(GetCurrentProcess(), code);
}

/// Init thread: resolve, hook `ExitProcess` (option) and start the watcher. false = module off (logged).
pub fn activate(text: &Text, cfg: &QuitFixCfg) -> bool {
    let get = |r: &crate::sigs::RipRef| {
        let v = text.resolve_rip(r);
        if v.is_none() {
            error!("quit_fix: {} not found", r.name);
        }
        v
    };
    let (Some(quit_request), Some(quit_flag), Some(file_pending), Some(save_manager), Some(save_any_running)) = (
        get(&s::QF_QUIT_REQUEST),
        get(&s::QF_QUIT_FLAG),
        get(&s::QF_FILE_PENDING),
        get(&s::QF_SAVE_MANAGER),
        get(&s::QF_SAVE_ANY_JOB_RUNNING),
    ) else {
        return false;
    };
    // the request scan is optional: without it only the counter is known (no early quit, the grace path works)
    let file_capacity = text.resolve_rip(&s::QF_FILE_CAPACITY);
    let file_array = text.resolve_rip(&s::QF_FILE_ARRAY);
    if file_capacity.is_none() || file_array.is_none() {
        warn!("quit_fix: file-request array not found: no early quit, the {} s grace applies", cfg.grace_ms() / 1000);
    }
    let a = Addrs { quit_request, quit_flag, file_pending, save_manager, save_any_running, file_capacity, file_array };
    let _ = ADDRS.set(a);
    let exit_hook = cfg.exit_terminate
        && crate::hook::iat_hook(
            crate::pe::live::exe_base(),
            "KERNEL32.dll",
            "ExitProcess",
            hk_exit_process as *const () as usize,
            &ORIG_EXIT,
        );
    let c = cfg.clone();
    let spawned = std::thread::Builder::new().name("vr-loader-quit-fix".into()).spawn(move || watch_thread(a, c)).is_ok();
    if !spawned {
        error!("quit_fix: watcher thread not started");
        return false;
    }
    info!(
        "quit_fix: on: close request watched (grace {} s, retail_quit {}, save wait {} s), ExitProcess -> TerminateProcess {}",
        cfg.grace_ms() / 1000,
        cfg.retail_quit,
        cfg.save_wait_ms() / 1000,
        if exit_hook { "on" } else if cfg.exit_terminate { "FAILED (IAT)" } else { "off" }
    );
    true
}
