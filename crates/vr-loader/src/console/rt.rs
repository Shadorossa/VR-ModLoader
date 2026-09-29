//! Run time of module `console` (Windows): the taps (installed in DllMain), the event writer / state poller threads
//! and the console window (mirror of loader.log + command line), started by the init thread.
//!
//! Threads (none of them is the game thread; the game thread only runs the taps):
//! * `vr-loader-console-ev`: events -> rate limit -> `[category] …` lines in loader.log;
//! * `vr-loader-console-poll`: `g_soccerState` / `g_sceneSoccer` every 16 ms while `states` or `match` is on;
//! * `vr-loader-console`: mirror of loader.log into the window (coloured, filtered by category);
//! * `vr-loader-console-in`: the command line.
//!
//! Closing the window must never end the game: the close button is removed (`[console] block_close`), and the
//! control handler ignores Ctrl+C / Ctrl+Break and answers CTRL_CLOSE_EVENT by detaching the process from the console
//! (`FreeConsole`) before returning, so the console host has no process of ours left to end.

use super::cmds::{self, CmdResult};
use super::sigs as cs;
use super::{
    enabled, format_event, match_event, names, ConsoleCfg, Event, Kind, RateLimiter, Small, CATS, CAT_FILES, CAT_MATCH,
    CAT_MENUS, CAT_SOUND, CAT_STATES, COLORS, FILE_HITS, RATE_LIMIT,
};
use crate::game::{read, read_ptr, Text};
use crate::log::Level;
use crate::lua::Call;
use crate::sigs::Sig;
use crate::{debug, error, info, warn};
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::mpsc::{sync_channel, Receiver, RecvTimeoutError, SyncSender, TrySendError};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};
use windows_sys::Win32::Foundation::{CloseHandle, GetLastError, BOOL, GENERIC_READ, GENERIC_WRITE, HANDLE, INVALID_HANDLE_VALUE};
use windows_sys::Win32::Storage::FileSystem::{CreateFileW, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING};
use windows_sys::Win32::System::Console::{
    AllocConsole, FreeConsole, GetConsoleMode, GetConsoleWindow, ReadConsoleW, SetConsoleCP, SetConsoleCtrlHandler, SetConsoleMode,
    SetConsoleOutputCP, SetConsoleTitleW, WriteConsoleW, CTRL_BREAK_EVENT, CTRL_CLOSE_EVENT, CTRL_C_EVENT, ENABLE_PROCESSED_OUTPUT,
    ENABLE_VIRTUAL_TERMINAL_PROCESSING,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{DeleteMenu, GetSystemMenu, MF_BYCOMMAND, SC_CLOSE};

/// Events queued between the taps and the writer thread (a full queue drops, never blocks).
const QUEUE_CAP: usize = 4096;
const POLL_MS: u64 = 16;

static TX: OnceLock<SyncSender<Event>> = OnceLock::new();
static RX: Mutex<Option<Receiver<Event>>> = Mutex::new(None);
static QUEUE_DROPPED: AtomicU64 = AtomicU64::new(0);
static EVENTS_SENT: AtomicU64 = AtomicU64::new(0);

/// Poller addresses: `g_soccerState` (u8) and `g_sceneSoccer` (pointer variable); 0 = not resolved.
static G_STATE: AtomicUsize = AtomicUsize::new(0);
static G_SCENE: AtomicUsize = AtomicUsize::new(0);

static NEXT_OPEN: AtomicUsize = AtomicUsize::new(0);
static NEXT_MENU: AtomicUsize = AtomicUsize::new(0);
static NEXT_VOICE: AtomicUsize = AtomicUsize::new(0);
/// Which taps are live, for `status`.
static TAPS: Mutex<Vec<String>> = Mutex::new(Vec::new());

static STARTED: AtomicBool = AtomicBool::new(false);
/// The console window is gone (closed, `close`, or never opened): nothing is written to it.
static CLOSED: AtomicBool = AtomicBool::new(true);
static OUT: AtomicUsize = AtomicUsize::new(0);
static INP: AtomicUsize = AtomicUsize::new(0);
static VT: AtomicBool = AtomicBool::new(false);
static WLOCK: Mutex<()> = Mutex::new(());

type F4 = unsafe extern "C" fn(u64, u64, u64, u64) -> u64;
type F5 = unsafe extern "C" fn(u64, u64, u64, u64, u64) -> u64;

#[inline]
fn send(ev: Event) {
    if let Some(tx) = TX.get() {
        match tx.try_send(ev) {
            Ok(()) => {
                EVENTS_SENT.fetch_add(1, Ordering::Relaxed);
            }
            Err(TrySendError::Full(_)) => {
                QUEUE_DROPPED.fetch_add(1, Ordering::Relaxed);
            }
            Err(TrySendError::Disconnected(_)) => {}
        }
    }
}

/// Copy a C string the engine is about to use anyway (at most the event's capacity; no allocation).
unsafe fn push_cstr(s: &mut Small, p: u64) {
    if p < 0x10000 {
        return;
    }
    let mut buf = [0u8; 120];
    let mut n = 0;
    while n < buf.len() {
        let c = *((p as usize + n) as *const u8);
        if c == 0 {
            break;
        }
        buf[n] = c;
        n += 1;
    }
    s.push(&buf[..n]);
}

// ---------------------------------------------------------------- taps (game threads)

/// `CCriFileOperate::Open(this, path, mode, u8, flags)` -> handle, 0 = failed (error code at `this+0x144`).
unsafe extern "C" fn open_detour(this: u64, path: u64, mode: u64, b: u64, flags: u64) -> u64 {
    let next: F5 = std::mem::transmute::<usize, F5>(NEXT_OPEN.load(Ordering::Acquire));
    if !enabled(CAT_FILES) {
        return next(this, path, mode, b, flags);
    }
    let r = next(this, path, mode, b, flags);
    let ok = r != 0;
    if !ok || FILE_HITS.load(Ordering::Relaxed) {
        let mut ev = Event::new(CAT_FILES, Kind::File);
        ev.a = ok as u32;
        if !ok {
            ev.b = read::<u32>(this as usize + 0x144).unwrap_or(0);
        }
        push_cstr(&mut ev.s, path);
        send(ev);
    }
    r
}

/// `CMenuController::OpenMenu(this, const u32* nameHash, const OpenMenuParam* p)` -> bool (rax passed through whole).
unsafe extern "C" fn menu_detour(this: u64, name: u64, p: u64, d: u64) -> u64 {
    let next: F4 = std::mem::transmute::<usize, F4>(NEXT_MENU.load(Ordering::Acquire));
    let (on_m, on_p) = (enabled(CAT_MENUS), enabled(CAT_MATCH));
    if !on_m && !on_p {
        return next(this, name, p, d);
    }
    let h = read::<u32>(name as usize).unwrap_or(0);
    let param = if p != 0 { read::<u32>(p as usize + cs::OMP_PARAM).unwrap_or(0) } else { 0 };
    let r = next(this, name, p, d);
    if on_m {
        let mut ev = Event::new(CAT_MENUS, Kind::MenuOpen);
        ev.a = h;
        ev.b = param;
        ev.f = if r & 0xFF != 0 { 1.0 } else { 0.0 };
        send(ev);
    }
    if on_p && h == cs::MENU_SKILL_TELOP {
        send(Event::new(CAT_MATCH, Kind::Technique));
    }
    r
}

/// `PlayCharaVoice(soundMgr, u32* handle, bank, suffix, params)` (chained hook).
unsafe extern "C" fn voice_detour(mgr: u64, out: u64, bank: u64, suffix: u64, params: u64) -> u64 {
    let next: F5 = std::mem::transmute::<usize, F5>(NEXT_VOICE.load(Ordering::Acquire));
    if !enabled(CAT_SOUND) {
        return next(mgr, out, bank, suffix, params);
    }
    // copy the names first: an inner detour (a plugin) may call the original with another suffix
    let mut ev = Event::new(CAT_SOUND, Kind::Voice);
    push_cstr(&mut ev.s, bank);
    ev.s.push(b"_");
    push_cstr(&mut ev.s, suffix);
    let r = next(mgr, out, bank, suffix, params);
    ev.a = if out != 0 { read::<u32>(out as usize).unwrap_or(0) } else { 0 };
    send(ev);
    r
}

fn f_close(c: &mut Call) -> bool {
    if enabled(CAT_MENUS) {
        let mut ev = Event::new(CAT_MENUS, Kind::MenuClose);
        ev.a = c.u32(0).unwrap_or(0);
        send(ev);
    }
    false
}

fn f_delete(c: &mut Call) -> bool {
    if enabled(CAT_MENUS) {
        let mut ev = Event::new(CAT_MENUS, Kind::MenuDelete);
        ev.a = c.u32(0).unwrap_or(0);
        send(ev);
    }
    false
}

fn f_reserve(c: &mut Call) -> bool {
    if enabled(CAT_MENUS) {
        let mut ev = Event::new(CAT_MENUS, Kind::MenuReserve);
        ev.a = c.u32(0).unwrap_or(0);
        ev.b = c.int(4).unwrap_or(0) as u32;
        send(ev);
    }
    false
}

fn f_soccer(c: &mut Call) -> bool {
    if enabled(CAT_MATCH) {
        let mut ev = Event::new(CAT_MATCH, Kind::SoccerReserve);
        ev.a = c.u32(0).unwrap_or(0);
        ev.b = c.int(2).unwrap_or(0) as u32;
        send(ev);
    }
    false
}

/// Join the chain of `sig` (first hook of the target: its fixed prefix of `steal` bytes is checked).
fn chain(text: &Text, sig: &Sig, steal: usize, detour: usize, next: &'static AtomicUsize) -> Result<usize, String> {
    let addr = text.resolve(sig).ok_or_else(|| format!("{}: signature not found", sig.name))?;
    let exp = crate::scan::Pattern::parse(sig.pattern)
        .ok()
        .and_then(|p| p.fixed_prefix(steal))
        .ok_or_else(|| format!("{}: stolen bytes contain wildcards", sig.name))?;
    unsafe { crate::hook::chain_hook(addr, &exp, detour, next, 0, 0, "console") }.map(|_| addr)
}

/// DllMain (after the other modules' hooks): switches from `[console]`, the event queue, the taps and the Lua
/// filters (registered before `lua::activate`). Nothing is written to a window yet.
pub fn install(text: &Text, cfg: &ConsoleCfg) {
    COLORS.store(cfg.colors, Ordering::Relaxed);
    FILE_HITS.store(cfg.file_hits, Ordering::Relaxed);
    RATE_LIMIT.store(cfg.rate_limit, Ordering::Relaxed);
    super::apply_config_categories(&cfg.categories);
    let (tx, rx) = sync_channel::<Event>(QUEUE_CAP);
    let _ = TX.set(tx);
    *RX.lock().unwrap_or_else(|e| e.into_inner()) = Some(rx);
    let mut taps = Vec::new();
    // states / match: two data addresses read by the poller (no hook)
    match (text.resolve_rip(&crate::sigs::G_SOCCER_STATE), text.resolve_rip(&crate::sigs::G_SCENE_SOCCER)) {
        (Some(s), Some(g)) => {
            G_STATE.store(s, Ordering::Release);
            G_SCENE.store(g, Ordering::Release);
            taps.push("states/match: g_soccerState + g_sceneSoccer (16 ms poll)".to_string());
        }
        _ => warn!("console: g_soccerState / g_sceneSoccer not resolved: no `states` / `match` categories"),
    }
    let hooks: [(&str, &Sig, usize, usize, &'static AtomicUsize); 3] = [
        (
            "files",
            &crate::lua_patch::sigs::LP_FILE_OPEN,
            crate::lua_patch::sigs::FILE_OPEN_STEAL,
            open_detour as *const () as usize,
            &NEXT_OPEN,
        ),
        ("menus", &cs::CO_OPEN_MENU, cs::OPEN_MENU_STEAL, menu_detour as *const () as usize, &NEXT_MENU),
        (
            "sound",
            &cs::CO_PLAY_CHARA_VOICE,
            cs::PLAY_CHARA_VOICE_STEAL,
            voice_detour as *const () as usize,
            &NEXT_VOICE,
        ),
    ];
    for (cat, sig, steal, detour, next) in hooks {
        match chain(text, sig, steal, detour, next) {
            Ok(a) => {
                info!("console: {cat}: {} hooked at 0x{:X} (chain)", sig.name, a - text.base);
                taps.push(format!("{cat}: {}", sig.name));
            }
            Err(e) => warn!("console: {cat}: no hook ({e})"),
        }
    }
    crate::lua::register_filter(cs::CMD_CLOSE_MENU_OBJECT, "console.CloseMenu", f_close);
    crate::lua::register_filter(cs::CMD_DELETE_MENU_OBJECT, "console.DeleteMenu", f_delete);
    crate::lua::register_filter(cs::CMD_RESERVE_MENU, "console.ReserveMenu", f_reserve);
    crate::lua::register_filter(cs::CMD_RESERVE_SOCCER, "console.ReserveSoccer", f_soccer);
    taps.push("menus/match: Lua filters CLOSE/DELETE_MENU_OBJECT, RESERVE_MENU, RESERVE_SOCCER".to_string());
    *TAPS.lock().unwrap_or_else(|e| e.into_inner()) = taps;
}

// ---------------------------------------------------------------- writer / poller threads

fn writer_thread(rx: Receiver<Event>) {
    let t0 = Instant::now();
    let mut rl = RateLimiter::new();
    loop {
        let got = rx.recv_timeout(Duration::from_millis(500));
        let now = t0.elapsed().as_millis() as u64;
        for (cat, n) in rl.tick(now) {
            let name = CATS.name(cat).unwrap_or_else(|| "?".into());
            info!("[{name}] … {n} line(s) dropped in the last second (console.rate_limit = {})", RATE_LIMIT.load(Ordering::Relaxed));
        }
        let lost = QUEUE_DROPPED.swap(0, Ordering::Relaxed);
        if lost > 0 {
            warn!("console: {lost} event(s) lost (queue of {QUEUE_CAP} full)");
        }
        match got {
            Ok(e) => {
                if !CATS.enabled(e.cat) || !rl.admit(e.cat, RATE_LIMIT.load(Ordering::Relaxed)) {
                    continue;
                }
                let name = CATS.name(e.cat).unwrap_or_else(|| "?".into());
                let text = format_event(&e, &names::menu_name);
                crate::log::write(Level::Info, format_args!("[{name}] {text}"));
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => return,
        }
    }
}

fn poll_thread() {
    let (gs, gc) = (G_STATE.load(Ordering::Acquire), G_SCENE.load(Ordering::Acquire));
    let mut last: Option<u8> = None;
    let mut in_scene = false;
    loop {
        let (on_e, on_p) = (enabled(CAT_STATES), enabled(CAT_MATCH));
        if !on_e && !on_p {
            last = None;
            std::thread::sleep(Duration::from_millis(250));
            continue;
        }
        let scene = read_ptr(gc);
        if scene.is_some() != in_scene {
            in_scene = scene.is_some();
            if on_e {
                let mut ev = Event::new(CAT_STATES, Kind::Scene);
                ev.a = in_scene as u32;
                send(ev);
            }
            if !in_scene {
                last = None;
            }
        }
        if let Some(sc) = scene {
            if let Some(st) = read::<u8>(gs) {
                if last != Some(st) {
                    let clock = read::<f32>(sc + crate::sigs::SCENE_CLOCK).unwrap_or(-1.0);
                    let prev = last.unwrap_or(0);
                    if on_e {
                        let mut ev = Event::new(CAT_STATES, Kind::State);
                        ev.a = prev as u32;
                        ev.b = st as u32;
                        ev.f = clock;
                        send(ev);
                    }
                    if on_p && last.is_some() && match_event(prev, st, clock).is_some() {
                        let mut ev = Event::new(CAT_MATCH, Kind::Match);
                        ev.a = prev as u32;
                        ev.b = st as u32;
                        ev.f = clock;
                        send(ev);
                    }
                    last = Some(st);
                }
            }
        }
        std::thread::sleep(Duration::from_millis(POLL_MS));
    }
}

// ---------------------------------------------------------------- console window

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

fn con_write(s: &str) {
    if CLOSED.load(Ordering::Acquire) {
        return;
    }
    let h = OUT.load(Ordering::Acquire) as HANDLE;
    let w: Vec<u16> = s.encode_utf16().collect();
    let _g = WLOCK.lock().unwrap_or_else(|e| e.into_inner());
    let mut done = 0u32;
    unsafe { WriteConsoleW(h, w.as_ptr() as _, w.len() as u32, &mut done, std::ptr::null()) };
}

/// Print `text` (one or more lines) in the SGR colour `color` when colours are on.
fn print(text: &str, color: Option<&str>) {
    let colors = COLORS.load(Ordering::Relaxed) && VT.load(Ordering::Relaxed);
    let mut out = String::with_capacity(text.len() + 16);
    for line in text.lines() {
        match color {
            Some(c) if colors => out.push_str(&format!("\x1b[{c}m{line}\x1b[0m\r\n")),
            _ => {
                out.push_str(line);
                out.push_str("\r\n");
            }
        }
    }
    con_write(&out);
}

/// Detach from the console (the window closes when no process is attached). Never touches the game.
fn detach(why: &str) {
    if CLOSED.swap(true, Ordering::AcqRel) {
        return;
    }
    info!("console: {why}: console closed, the game keeps running (it opens again when the game restarts)");
    unsafe {
        FreeConsole();
        // our CONOUT$ / CONIN$ handles must not keep the console host alive; a thread still using one only gets an
        // error (Read/WriteConsoleW refuse non-console handles, CLOSED is checked before each call)
        for h in [OUT.swap(0, Ordering::AcqRel), INP.swap(0, Ordering::AcqRel)] {
            if h != 0 && h as HANDLE != INVALID_HANDLE_VALUE {
                CloseHandle(h as HANDLE);
            }
        }
    }
}

unsafe extern "system" fn ctrl_handler(ctrl: u32) -> BOOL {
    match ctrl {
        // Ctrl+C / Ctrl+Break in the console must not end the game
        CTRL_C_EVENT | CTRL_BREAK_EVENT => 1,
        // window closed (X, taskbar, Alt+F4 when not blocked): detach first, then report it as handled
        CTRL_CLOSE_EVENT => {
            detach("window closed (CTRL_CLOSE_EVENT)");
            1
        }
        _ => 0,
    }
}

fn mirror_thread(path: PathBuf) {
    let mut file: Option<std::fs::File> = None;
    let mut pos: u64 = 0;
    let mut partial: Vec<u8> = Vec::new();
    loop {
        if CLOSED.load(Ordering::Acquire) {
            return;
        }
        if file.is_none() {
            file = std::fs::File::open(&path).ok();
            pos = 0;
            partial.clear();
        }
        if let Some(f) = file.as_mut() {
            let len = f.metadata().map(|m| m.len()).unwrap_or(pos);
            if len < pos {
                pos = 0;
                partial.clear();
            }
            if len > pos && f.seek(SeekFrom::Start(pos)).is_ok() {
                let mut buf = Vec::with_capacity((len - pos).min(1 << 20) as usize);
                if let Ok(n) = f.by_ref().take((len - pos).min(1 << 20)).read_to_end(&mut buf) {
                    pos += n as u64;
                    partial.extend_from_slice(&buf);
                    let mut out = String::new();
                    let colors = COLORS.load(Ordering::Relaxed) && VT.load(Ordering::Relaxed);
                    while let Some(i) = partial.iter().position(|&b| b == b'\n') {
                        let raw: Vec<u8> = partial.drain(..=i).collect();
                        let line = String::from_utf8_lossy(&raw);
                        let line = line.trim_end_matches(['\r', '\n']);
                        let info = super::classify(line);
                        if !super::show_line(&info, &CATS) {
                            continue;
                        }
                        match super::color_of(&info, &CATS) {
                            Some(c) if colors => out.push_str(&format!("\x1b[{c}m{line}\x1b[0m\r\n")),
                            _ => {
                                out.push_str(line);
                                out.push_str("\r\n");
                            }
                        }
                    }
                    if !out.is_empty() {
                        con_write(&out);
                    }
                    continue; // more may be waiting
                }
            }
        }
        std::thread::sleep(Duration::from_millis(150));
    }
}

fn input_thread() {
    let h = INP.load(Ordering::Acquire) as HANDLE;
    let mut buf = [0u16; 512];
    let mut acc = String::new();
    let mut fails = 0;
    loop {
        if CLOSED.load(Ordering::Acquire) {
            return;
        }
        let mut n = 0u32;
        let ok = unsafe { ReadConsoleW(h, buf.as_mut_ptr() as _, buf.len() as u32, &mut n, std::ptr::null()) };
        if ok == 0 {
            if CLOSED.load(Ordering::Acquire) {
                return;
            }
            fails += 1;
            if fails > 50 {
                warn!("console: the command line stops reading (ReadConsoleW fails: {})", unsafe { GetLastError() });
                return;
            }
            std::thread::sleep(Duration::from_millis(200));
            continue;
        }
        fails = 0;
        acc.push_str(&String::from_utf16_lossy(&buf[..n as usize]));
        while let Some(i) = acc.find('\n') {
            let line: String = acc.drain(..=i).collect();
            run_line(line.trim());
        }
    }
}

fn run_line(line: &str) {
    if line.is_empty() {
        return;
    }
    debug!("console: > {line}");
    match cmds::execute(line) {
        Ok(s) if !s.is_empty() => print(&s, Some("97")),
        Ok(_) => {}
        Err(e) => print(&e, Some("91")),
    }
}

// ---------------------------------------------------------------- Windows-only commands

fn startup_toml() -> Option<toml::Value> {
    crate::runtime::ctx().and_then(|c| toml::Value::try_from(&c.cfg).ok())
}

fn cmd_status(_: &[String]) -> CmdResult {
    let mut out = format!(
        "ModLoader {} (vr-loader {}, API Lua {})\n",
        crate::MODLOADER_VERSION,
        crate::LOADER_VERSION,
        crate::API_VERSION
    );
    if let Some(ctx) = crate::runtime::ctx() {
        let on: Vec<String> = serde_json::to_value(&ctx.cfg.modules)
            .ok()
            .and_then(|v| v.as_object().cloned())
            .map(|o| o.into_iter().filter(|(_, b)| b.as_bool() == Some(true)).map(|(k, _)| k).collect())
            .unwrap_or_default();
        out.push_str(&format!(
            "Modules on in config ({}): {}\nActive module mask: 0x{:08X}{}\n",
            on.len(),
            on.join(", "),
            crate::runtime::MODULES.load(Ordering::Acquire),
            if crate::runtime::enabled() { "" } else { " (loader DISABLED: see the errors in loader.log)" }
        ));
        if ctx.cfg.modules.mods {
            let m = crate::runtime::active_mods();
            out.push_str(&format!("Active mods ({}): {}\n", m.len(), if m.is_empty() { "none".into() } else { m.join(", ") }));
        } else {
            out.push_str("Mods: module `mods` is off\n");
        }
    }
    let p: Vec<String> = crate::plugins::host::loaded().iter().map(|s| s.label()).collect();
    out.push_str(&format!("Loaded plugins ({}): {}\n", p.len(), if p.is_empty() { "none".into() } else { p.join(", ") }));
    let st = match crate::match_state::soccer_mode() {
        Some(true) => "in a match",
        Some(false) => "not in a match",
        None => "unknown",
    };
    let cur = read_ptr(G_SCENE.load(Ordering::Acquire))
        .and_then(|_| read::<u8>(G_STATE.load(Ordering::Acquire)))
        .map(|s| format!(", state {} ({s})", crate::sigs::soccer_state_name(s)))
        .unwrap_or_default();
    out.push_str(&format!("Game: {st}{cur}\n"));
    out.push_str(&format!(
        "Console: categories on: {}; events sent {}, lost to a full queue {}\n",
        CATS.on_names().join(", "),
        EVENTS_SENT.load(Ordering::Relaxed),
        QUEUE_DROPPED.load(Ordering::Relaxed)
    ));
    for t in TAPS.lock().unwrap_or_else(|e| e.into_inner()).iter() {
        out.push_str(&format!("  tap {t}\n"));
    }
    Ok(out.trim_end().to_string())
}

fn cmd_lua(args: &[String]) -> CmdResult {
    let sub = args.first().map(|s| s.to_ascii_lowercase());
    if !matches!(sub.as_deref(), Some("reload" | "recargar")) {
        return Err("usage: lua reload".into());
    }
    let active = crate::runtime::MODULES.load(Ordering::Acquire) & crate::runtime::MOD_LUA_PATCH != 0;
    let mods_lua = !crate::runtime::active_mods().is_empty();
    Ok(format!(
        "No reload needed: lua_patch reads the patches (evt_loader\\lua_patches and mods\\<id>\\lua) from disk EVERY TIME \
the game loads a script. Save the .lua and open that menu / screen again (or change map): the new version runs.\n\
What is NOT reloaded while running: the _fingerprints.json files (read at start) and scripts already loaded \
(a VM that stays open does not run the patches again).\nState: module lua_patch {}; mods with Lua: {}.",
        if active { "active" } else { "off (or no hooks)" },
        if mods_lua { "yes (if module `mods` is on)" } else { "none" }
    ))
}

fn cmd_quit(_: &[String]) -> CmdResult {
    let text = Text::current().ok_or("cannot read nie.exe")?;
    let addr = text.resolve_rip(&crate::quit_fix::sigs::QF_QUIT_REQUEST).ok_or("g_quitRequest not found")?;
    if !crate::game::write::<u8>(addr, 1) {
        return Err("could not write g_quitRequest".into());
    }
    let guard = match crate::runtime::ctx() {
        Some(c) if c.cfg.modules.quit_fix => "quit_fix guards the shutdown (also in a match)",
        _ if crate::plugins::host::replacing_builtin("quit_fix").is_some() => "the quit_fix plugin guards the shutdown",
        _ => "no quit_fix: in a match the retail gate can be slow or never open",
    };
    info!("console: quit: g_quitRequest = 1 (the same request as the game window's X)");
    Ok(format!("Quit requested from the game (g_quitRequest = 1): the game closes once no file or save is in progress; {guard}."))
}

fn cmd_close(_: &[String]) -> CmdResult {
    print("Closing the console (the game keeps running)…", Some("97"));
    detach("close command");
    Ok(String::new())
}

fn cmd_clear(_: &[String]) -> CmdResult {
    if VT.load(Ordering::Relaxed) {
        con_write("\x1b[2J\x1b[3J\x1b[H");
        Ok(String::new())
    } else {
        Err("this console does not support VT sequences".into())
    }
}

fn register_commands() {
    cmds::register_builtins(startup_toml);
    let reg = [
        ("status", "status", "modules, mods, plugins, game and console state", cmd_status as fn(&[String]) -> CmdResult),
        ("lua", "lua reload", "explains how Lua patches are reloaded (they are read each time a script loads)", cmd_lua),
        (
            "quit",
            "quit",
            "closes the game through its own quit request (g_quitRequest, like the X)\nwaits until no file or save is in progress",
            cmd_quit,
        ),
        ("close", "close", "closes this console without touching the game", cmd_close),
        ("clear", "clear", "clears the console screen", cmd_clear),
    ];
    for (n, u, h, f) in reg {
        if let Err(e) = cmds::register_command(n, u, h, "loader", f) {
            error!("console: {e}");
        }
    }
    // Spanish names of the first console versions (hidden aliases, not listed by `help`)
    for (alias, target) in [("cerrar", "close")] {
        if let Err(e) = cmds::register_alias(alias, target) {
            error!("console: {e}");
        }
    }
}

/// Init thread, right after the log is open: event writer + poller, then the window (mirror + command line).
pub fn start(cfg: &'static ConsoleCfg, data_dir: &Path) {
    if STARTED.swap(true, Ordering::AcqRel) {
        return;
    }
    if let Some(rx) = RX.lock().unwrap_or_else(|e| e.into_inner()).take() {
        let _ = std::thread::Builder::new().name("vr-loader-console-ev".into()).spawn(move || writer_thread(rx));
    }
    if G_STATE.load(Ordering::Acquire) != 0 {
        let _ = std::thread::Builder::new().name("vr-loader-console-poll".into()).spawn(poll_thread);
    }
    register_commands();
    let (out, inp, vt) = unsafe {
        if AllocConsole() == 0 {
            warn!("console: AllocConsole failed ({}): no window; the categories still go to loader.log", GetLastError());
            return;
        }
        SetConsoleTitleW(wide("ModLoader").as_ptr());
        SetConsoleOutputCP(65001);
        SetConsoleCP(65001);
        let open = |n: &str| {
            CreateFileW(
                wide(n).as_ptr(),
                GENERIC_READ | GENERIC_WRITE,
                FILE_SHARE_READ | FILE_SHARE_WRITE,
                std::ptr::null(),
                OPEN_EXISTING,
                0,
                std::ptr::null_mut(),
            )
        };
        let out = open("CONOUT$");
        let inp = open("CONIN$");
        let mut mode = 0;
        let vt = GetConsoleMode(out, &mut mode) != 0
            && SetConsoleMode(out, mode | ENABLE_PROCESSED_OUTPUT | ENABLE_VIRTUAL_TERMINAL_PROCESSING) != 0;
        SetConsoleCtrlHandler(Some(ctrl_handler), 1);
        if cfg.block_close {
            let hwnd = GetConsoleWindow();
            if !hwnd.is_null() {
                let m = GetSystemMenu(hwnd, 0);
                if !m.is_null() {
                    DeleteMenu(m, SC_CLOSE, MF_BYCOMMAND);
                }
            }
        }
        (out, inp, vt)
    };
    if out == INVALID_HANDLE_VALUE || out.is_null() {
        warn!("console: CONOUT$ does not open ({}): no window", unsafe { GetLastError() });
        unsafe { FreeConsole() };
        return;
    }
    OUT.store(out as usize, Ordering::Release);
    INP.store(inp as usize, Ordering::Release);
    VT.store(vt, Ordering::Release);
    CLOSED.store(false, Ordering::Release);
    print(
        &format!(
            "ModLoader {} - console (live loader.log). Type help. Categories on: {}{}",
            crate::MODLOADER_VERSION,
            CATS.on_names().join(", "),
            if cfg.block_close { ". To close it: close (the X is disabled)" } else { "" }
        ),
        Some("1;97"),
    );
    info!(
        "console: window open (colors {}, command line {}, X {})",
        if vt && cfg.colors { "yes" } else { "no" },
        if cfg.input && inp != INVALID_HANDLE_VALUE { "yes" } else { "no" },
        if cfg.block_close { "disabled" } else { "enabled (detaches on close)" }
    );
    let log = data_dir.join("loader.log");
    let _ = std::thread::Builder::new().name("vr-loader-console".into()).spawn(move || mirror_thread(log));
    if cfg.input && inp != INVALID_HANDLE_VALUE && !inp.is_null() {
        let _ = std::thread::Builder::new().name("vr-loader-console-in".into()).spawn(input_thread);
    }
}
