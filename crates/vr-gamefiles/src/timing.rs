//! Startup phase timing: the index scan, the schema load and every step of the database build report how
//! long they took, so `examples/db_timing.rs` (and the app's debug log) can print a per-phase table.
//!
//! Recording is off unless [`enable`] was called (or `EVT_TIMING` is set in the environment, which also echoes
//! every phase to stderr as it ends). Off, [`phase`] costs one atomic load per call.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};

static ON: AtomicBool = AtomicBool::new(false);
static ECHO: AtomicBool = AtomicBool::new(false);
static PHASES: Mutex<Vec<(String, Duration)>> = Mutex::new(Vec::new());

fn lock() -> std::sync::MutexGuard<'static, Vec<(String, Duration)>> {
    PHASES.lock().unwrap_or_else(|e| e.into_inner())
}

/// Start recording (clearing anything recorded before). `echo` prints each phase to stderr as it ends.
pub fn enable(echo: bool) {
    lock().clear();
    ECHO.store(echo, Ordering::Relaxed);
    ON.store(true, Ordering::Relaxed);
}

/// Enable from the environment: `EVT_TIMING=1` records and echoes to stderr.
pub fn enable_from_env() {
    if std::env::var_os("EVT_TIMING").is_some_and(|v| !v.is_empty() && v != "0") {
        enable(true);
    }
}

pub fn enabled() -> bool {
    ON.load(Ordering::Relaxed)
}

/// Stop recording and return every `(phase, duration)` in completion order. Nested phases are named
/// `parent/child` and end before their parent, so a parent's time includes its children.
pub fn take() -> Vec<(String, Duration)> {
    ON.store(false, Ordering::Relaxed);
    std::mem::take(&mut *lock())
}

/// Record a finished phase.
pub fn record(name: &str, d: Duration) {
    if !enabled() {
        return;
    }
    if ECHO.load(Ordering::Relaxed) {
        eprintln!("[timing] {name:<44} {:>9.1} ms", d.as_secs_f64() * 1e3);
    }
    lock().push((name.to_string(), d));
}

/// Run `f` as a named phase.
pub fn phase<T>(name: &str, f: impl FnOnce() -> T) -> T {
    if !enabled() {
        return f();
    }
    let t = Instant::now();
    let r = f();
    record(name, t.elapsed());
    r
}

/// The recorded phases as an aligned text table (top-level phases, their children indented, and the sum of
/// the top-level ones), for logs and the timing example.
pub fn table(phases: &[(String, Duration)]) -> String {
    let mut out = String::new();
    let mut total = Duration::ZERO;
    for (name, d) in phases {
        let depth = name.matches('/').count();
        if depth == 0 {
            total += *d;
        }
        let label = name.rsplit('/').next().unwrap_or(name);
        out.push_str(&format!("{:indent$}{label:<width$} {:>9.1} ms\n", "", d.as_secs_f64() * 1e3, indent = depth * 2, width = 44 - depth * 2));
    }
    out.push_str(&format!("{:<44} {:>9.1} ms\n", "TOTAL (fases de primer nivel)", total.as_secs_f64() * 1e3));
    out
}
