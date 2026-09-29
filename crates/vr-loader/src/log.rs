//! Minimal file logger: `<game>\evt_loader\loader.log` (+ optional OutputDebugString).
//! The previous session's log is kept as `loader.prev.log`.
//!
//! Writes are asynchronous (docs/game/media/audio-crackle.md §5): the calling thread (game thread, Lua dispatcher, CRI
//! audio threads via the `audio` module) only formats the line and pushes it on a queue; the `vr-loader-log` thread
//! writes and flushes the file. `Error` lines (and [`flush`]) drain the queue synchronously so nothing important is
//! lost if the game crashes right after. Lines logged before [`open`] (DllMain) are buffered and written by `open`.

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicU8, Ordering};
use std::sync::{Condvar, Mutex};
use std::time::Duration;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
#[repr(u8)]
pub enum Level {
    Error = 0,
    Warn = 1,
    Info = 2,
    Debug = 3,
    Trace = 4,
}

impl Level {
    pub fn parse(s: &str) -> Level {
        match s.trim().to_ascii_lowercase().as_str() {
            "error" => Level::Error,
            "warn" | "warning" => Level::Warn,
            "debug" => Level::Debug,
            "trace" => Level::Trace,
            _ => Level::Info,
        }
    }
    fn tag(self) -> &'static str {
        match self {
            Level::Error => "ERROR",
            Level::Warn => "WARN ",
            Level::Info => "INFO ",
            Level::Debug => "DEBUG",
            Level::Trace => "TRACE",
        }
    }
}

static FILE: Mutex<Option<File>> = Mutex::new(None);
static LEVEL: AtomicU8 = AtomicU8::new(Level::Info as u8);
static DEBUG_OUT: AtomicBool = AtomicBool::new(false);
/// Lines logged before `open` (DllMain / early init) are buffered here.
static EARLY: Mutex<Vec<String>> = Mutex::new(Vec::new());
/// Set by `open`: lines go to [`QUEUE`] and the writer thread.
static OPENED: AtomicBool = AtomicBool::new(false);
/// Lines waiting for the writer thread.
static QUEUE: Mutex<Vec<String>> = Mutex::new(Vec::new());
static QUEUE_CV: Condvar = Condvar::new();
/// Lines dropped because the queue was full (reported by the next write).
static DROPPED: AtomicU64 = AtomicU64::new(0);
/// Queue cap: a runaway producer cannot eat memory; the overflow is counted in [`DROPPED`].
pub const QUEUE_MAX: usize = 20_000;
/// The writer thread wakes at least this often.
const WRITER_TICK: Duration = Duration::from_millis(250);

pub fn open(path: &Path, level: Level, debug_output: bool) {
    LEVEL.store(level as u8, Ordering::Relaxed);
    DEBUG_OUT.store(debug_output, Ordering::Relaxed);
    if path.exists() {
        let _ = std::fs::rename(path, path.with_file_name("loader.prev.log"));
    }
    if let Ok(mut f) = OpenOptions::new().create(true).write(true).truncate(true).open(path) {
        if let Ok(mut early) = EARLY.lock() {
            for l in early.drain(..) {
                let _ = f.write_all(l.as_bytes());
            }
        }
        let _ = f.flush();
        if let Ok(mut g) = FILE.lock() {
            *g = Some(f);
        }
        let spawned = std::thread::Builder::new().name("vr-loader-log".into()).spawn(writer_thread).is_ok();
        // no writer thread: stay synchronous (every line drains the queue itself)
        OPENED.store(true, Ordering::Release);
        if !spawned {
            SYNC.store(true, Ordering::Release);
        }
    }
}

/// Fallback when the writer thread could not be started.
static SYNC: AtomicBool = AtomicBool::new(false);

pub fn enabled(level: Level) -> bool {
    level as u8 <= LEVEL.load(Ordering::Relaxed)
}

/// Change the level at run time (ModLoader console: `log level`, `config set loader.log_level`).
pub fn set_level(level: Level) {
    LEVEL.store(level as u8, Ordering::Relaxed);
}

/// The current level.
pub fn level() -> Level {
    match LEVEL.load(Ordering::Relaxed) {
        0 => Level::Error,
        1 => Level::Warn,
        2 => Level::Info,
        3 => Level::Debug,
        _ => Level::Trace,
    }
}

pub fn write(level: Level, msg: std::fmt::Arguments) {
    if !enabled(level) {
        return;
    }
    let line = format!("{} {} [{:>5}] {}\r\n", crate::platform::timestamp(), level.tag(), crate::platform::thread_id(), msg);
    if DEBUG_OUT.load(Ordering::Relaxed) {
        crate::platform::debug_string(&format!("[vr-loader] {line}"));
    }
    if OPENED.load(Ordering::Acquire) {
        push(line);
        if level == Level::Error || SYNC.load(Ordering::Acquire) {
            flush();
        }
        return;
    }
    if let Ok(mut early) = EARLY.lock() {
        if early.len() < 1000 {
            early.push(line);
        }
    }
}

fn push(line: String) {
    let Ok(mut q) = QUEUE.lock() else { return };
    if q.len() >= QUEUE_MAX {
        DROPPED.fetch_add(1, Ordering::Relaxed);
        return;
    }
    let dropped = DROPPED.swap(0, Ordering::Relaxed);
    if dropped > 0 {
        q.push(format!("{} WARN  [{:>5}] log: {dropped} line(s) dropped (queue full)\r\n", crate::platform::timestamp(), crate::platform::thread_id()));
    }
    q.push(line);
    QUEUE_CV.notify_one();
}

/// Write every queued line now (file lock first, then the queue: two flushes never reorder lines).
pub fn flush() {
    let Ok(mut g) = FILE.lock() else { return };
    let batch = match QUEUE.lock() {
        Ok(mut q) => std::mem::take(&mut *q),
        Err(_) => return,
    };
    write_batch(&mut g, batch);
}

/// [`flush`] without blocking (DLL_PROCESS_DETACH: another thread may have died holding a lock).
pub fn try_flush() {
    let Ok(mut g) = FILE.try_lock() else { return };
    let batch = match QUEUE.try_lock() {
        Ok(mut q) => std::mem::take(&mut *q),
        Err(_) => return,
    };
    write_batch(&mut g, batch);
}

fn write_batch(g: &mut Option<File>, batch: Vec<String>) {
    if batch.is_empty() {
        return;
    }
    if let Some(f) = g.as_mut() {
        let _ = f.write_all(batch.concat().as_bytes());
        let _ = f.flush();
    }
}

fn writer_thread() {
    loop {
        {
            let Ok(q) = QUEUE.lock() else { return };
            let _ = QUEUE_CV.wait_timeout_while(q, WRITER_TICK, |q| q.is_empty());
        }
        flush();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn async_writes_reach_the_file_in_order() {
        // global logger: other tests may log concurrently, so only this test's marked lines are checked
        let dir = std::env::temp_dir().join(format!("vr_loader_log_test_{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("loader.log");
        write(Level::Info, format_args!("logtest early"));
        open(&path, Level::Debug, false);
        for i in 0..500 {
            write(Level::Info, format_args!("logtest line {i}"));
        }
        write(Level::Trace, format_args!("logtest filtered"));
        write(Level::Error, format_args!("logtest error")); // synchronous drain
        let text = std::fs::read_to_string(&path).unwrap();
        let ours: Vec<&str> = text.lines().filter(|l| l.contains("logtest")).collect();
        assert_eq!(ours.len(), 502, "{ours:?}");
        assert!(ours[0].ends_with("logtest early"));
        for i in 0..500 {
            assert!(ours[1 + i].ends_with(&format!("logtest line {i}")), "{}", ours[1 + i]);
        }
        assert!(ours[501].contains("ERROR") && ours[501].ends_with("logtest error"));
        let _ = std::fs::remove_dir_all(&dir);
    }
}

#[macro_export]
macro_rules! log_at {
    ($lvl:expr, $($arg:tt)*) => { $crate::log::write($lvl, format_args!($($arg)*)) };
}
#[macro_export]
macro_rules! error { ($($arg:tt)*) => { $crate::log_at!($crate::log::Level::Error, $($arg)*) }; }
#[macro_export]
macro_rules! warn { ($($arg:tt)*) => { $crate::log_at!($crate::log::Level::Warn, $($arg)*) }; }
#[macro_export]
macro_rules! info { ($($arg:tt)*) => { $crate::log_at!($crate::log::Level::Info, $($arg)*) }; }
#[macro_export]
macro_rules! debug { ($($arg:tt)*) => { $crate::log_at!($crate::log::Level::Debug, $($arg)*) }; }
#[macro_export]
macro_rules! trace { ($($arg:tt)*) => { $crate::log_at!($crate::log::Level::Trace, $($arg)*) }; }
