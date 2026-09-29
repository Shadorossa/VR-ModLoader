//! Run time of [`crate::match_rules`] (the Opciones half length, was the built-in loader module `match_rules`): the
//! state file `evt_loader\match_rules.json` and `CMND_EVT_MATCH_RULES_GET / _SET / _APPLY` (same names, arguments and
//! results: the match_rules menu grafts call them). The written period only replaces the retail 1800 s, so a ruleset
//! that sets its own `half_minutes` (anything but 30) keeps it.

use super::{info, read, root_ptr, warning, write};
use crate::match_rules::{decide, valid_half, Action, State, ARM_TTL, MAX_HALF, MIN_HALF, OFF_PERIOD_SECS, OFF_SOCCER_MANAGER, RETAIL_HALF_SECS};
use evt_plugin_sdk::{Host, LuaCall};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU16, AtomicU32, Ordering};
use std::sync::{Mutex, OnceLock};

static STATE: Mutex<Option<State>> = Mutex::new(None);
static FILE: OnceLock<PathBuf> = OnceLock::new();
/// Ticks left of the current arm (0 = not armed).
static ARMED: AtomicU32 = AtomicU32::new(0);
/// Last period value written (0 = none).
static LAST_WRITTEN: AtomicU16 = AtomicU16::new(0);

fn state() -> State {
    STATE.lock().unwrap_or_else(|e| e.into_inner()).clone().unwrap_or_default()
}

fn period_addr() -> Option<usize> {
    Some(root_ptr(OFF_SOCCER_MANAGER)? + OFF_PERIOD_SECS)
}

/// `yyyy-mm-dd hh:mm:ss UTC` (informational field of the state file).
fn timestamp() -> String {
    let s = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs()) as i64;
    let (days, rem) = (s.div_euclid(86_400), s.rem_euclid(86_400));
    // civil from days (Howard Hinnant)
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!("{y:04}-{m:02}-{d:02} {:02}:{:02}:{:02} UTC", rem / 3600, rem % 3600 / 60, rem % 60)
}

fn cmd_get(c: &mut LuaCall) {
    let s = state();
    c.push_int(s.half_minutes as i64);
    c.push_int(MIN_HALF as i64);
    c.push_int(MAX_HALF as i64);
}

fn cmd_set(c: &mut LuaCall) {
    let Some(m) = c.int(0).and_then(valid_half) else {
        warning!("match_rules: CMND_EVT_MATCH_RULES_SET({:?}) refused (valid {}..={})", c.num(0), MIN_HALF, MAX_HALF);
        c.push_bool(false);
        return;
    };
    let mut s = state();
    s.half_minutes = m;
    s.changed = format!("{} (Opciones)", timestamp());
    let ok = match FILE.get() {
        Some(p) => match s.save(p) {
            Ok(()) => true,
            Err(e) => {
                warning!("match_rules: cannot write {}: {e}", p.display());
                false
            }
        },
        None => false,
    };
    if ok {
        *STATE.lock().unwrap_or_else(|e| e.into_inner()) = Some(s);
        info!("match_rules: half length set to {m}' (next match)");
    }
    c.push_bool(ok);
}

fn cmd_apply(c: &mut LuaCall) {
    let mode = c.int(0).unwrap_or(0);
    if mode == 1 || mode == 2 {
        ARMED.store(ARM_TTL, Ordering::Release);
        if mode == 1 {
            LAST_WRITTEN.store(0, Ordering::Release);
        }
    }
    let Some(addr) = period_addr() else {
        c.push_int(-1);
        return;
    };
    let Some(cur) = read::<u16>(addr) else {
        c.push_int(-1);
        return;
    };
    let armed = ARMED.load(Ordering::Acquire) > 0;
    if armed && mode == 0 {
        ARMED.fetch_sub(1, Ordering::AcqRel);
    }
    let s = state();
    let code = match decide(s.half_minutes, cur, armed, LAST_WRITTEN.load(Ordering::Acquire)) {
        Action::Nothing => 0,
        Action::Write(v) => {
            if write::<u16>(addr, v) {
                LAST_WRITTEN.store(v, Ordering::Release);
                ARMED.store(0, Ordering::Release);
                info!("match_rules: period {cur} -> {v} s ({}' per half, mode {mode})", s.half_minutes);
                1
            } else {
                warning!("match_rules: write of the period at 0x{addr:X} failed");
                0
            }
        }
        Action::Restore => {
            if write::<u16>(addr, RETAIL_HALF_SECS) {
                LAST_WRITTEN.store(0, Ordering::Release);
                info!("match_rules: period {cur} -> {RETAIL_HALF_SECS} s (setting back to 30')");
                2
            } else {
                0
            }
        }
    };
    c.push_int(code);
}

/// Init thread: the state file (`<evt_loader>\match_rules.json`) and the commands.
pub(super) fn init(host: &Host) {
    let Some(dir) = host.path("loader_dir") else {
        warning!("match_rules: loader_dir unknown: Opciones half length off");
        return;
    };
    let path = dir.join("match_rules.json");
    let s = State::load(&path);
    info!("match_rules: half length {}' ({})", s.half_minutes, path.display());
    *STATE.lock().unwrap_or_else(|e| e.into_inner()) = Some(s);
    let _ = FILE.set(path);
    let cmds: [(&str, vr_framework::lua::Command); 3] = [("CMND_EVT_MATCH_RULES_GET", cmd_get), ("CMND_EVT_MATCH_RULES_SET", cmd_set), ("CMND_EVT_MATCH_RULES_APPLY", cmd_apply)];
    for (n, e) in vr_framework::lua::register_all(host, &cmds) {
        warning!("match_rules: {n} not registered (code {e})");
    }
}
