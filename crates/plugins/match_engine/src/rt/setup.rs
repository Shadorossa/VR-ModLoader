//! Setup of a match (docs/game/modes/match-engine.md §12.2): the ruleset is chosen while the match is being built and
//! its knobs are written into the match info being built (`mi_local`, a stack copy that `CSceneSoccer` init copies to
//! `[[g_gameRoot]+0x6A58]` right after the setup), so the whole engine sees them from the first frame.
//!
//! * `TeamBuild 0xE9C190(side, setup)` (read only): remembers the team id of each side (`setup+0`).
//! * `TeamRecordBuild 0x16B34F0(rec, params)`: on the first call of a setup, [`super::decide`] + [`crate::plan_setup`]
//!   (period, halves, story V-goal, ExRule); on every call, the team's on-pitch count and formation (ruleset, then the
//!   probe config) and a log line of the built team when something was changed or the probe is on.

use super::{cfg, debug, decide, error, info, read, read_ptr, warning, write, Decision, G_ROOT};
use crate::sigs as ms;
use crate::{outfield_shift, plan_setup, team_count, team_formation, team_goalkeeper, team_line, Member, RowRules};
use evt_plugin_sdk::Host;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Mutex;

type Fn4 = unsafe extern "C" fn(u64, u64, u64, u64) -> u64;

static NEXT_TRB: AtomicUsize = AtomicUsize::new(0);
static NEXT_TB: AtomicUsize = AtomicUsize::new(0);
static HOOKED: AtomicBool = AtomicBool::new(false);

/// Team ids of the sides built by the last `TeamBuild` calls.
static TEAM_IDS: Mutex<[Option<u32>; 2]> = Mutex::new([None, None]);

/// The setup in progress.
struct SetupCtx {
    mi_local: usize,
    teams_done: u8,
    user: u8,
    rules: Option<crate::ruleset::Ruleset>,
}
static SETUP: Mutex<Option<SetupCtx>> = Mutex::new(None);

/// The decision of the last setup, for the kick-off (`end::start_match`): taken once.
pub(crate) struct Decided {
    pub game: u32,
    pub d: Decision,
}
pub(crate) static DECIDED: Mutex<Option<Decided>> = Mutex::new(None);

pub(crate) fn hooked() -> bool {
    HOOKED.load(Ordering::Acquire)
}

/// The rival team id of the last setup (None = unknown).
pub(crate) fn rival_team(user: u8) -> Option<u32> {
    TEAM_IDS.lock().unwrap_or_else(|e| e.into_inner())[(1 - (user & 1)) as usize]
}

pub(super) fn install(host: &Host, cfg: &crate::MatchEngineCfg) {
    if !cfg.setup && !cfg.probe_active() {
        info!("setup knobs off (config setup = false): only [end] rules act");
        return;
    }
    match super::hook(host, &ms::TEAM_BUILD, ms::TEAM_BUILD_STEAL, tb_detour as *const (), &NEXT_TB) {
        Some(a) => debug!("{} hooked at 0x{a:X} (read-only: rival team id)", ms::TEAM_BUILD.name),
        None => warning!("TeamBuild not hooked: [teams] of matches.toml cannot match (rival team unknown)"),
    }
    match super::hook(host, &ms::TEAM_RECORD_BUILD, ms::TEAM_RECORD_BUILD_STEAL, trb_detour as *const (), &NEXT_TRB) {
        Some(a) => {
            HOOKED.store(true, Ordering::Release);
            info!("{} hooked at 0x{a:X}: rulesets decided at setup; probe {}", ms::TEAM_RECORD_BUILD.name, cfg.probe_summary());
        }
        None => error!("TeamRecordBuild not hooked: setup knobs off, the ruleset is chosen at kick-off ([end] rules only)"),
    }
}

unsafe extern "C" fn tb_detour(side: u64, setup: u64, r8: u64, r9: u64) -> u64 {
    let next: Fn4 = std::mem::transmute(NEXT_TB.load(Ordering::Acquire));
    let s = (side & 0xFF) as u8;
    if s <= 1 {
        let id = read::<u32>(setup as usize + ms::SETUP_TEAM_ID);
        TEAM_IDS.lock().unwrap_or_else(|e| e.into_inner())[s as usize] = id;
    }
    next(side, setup, r8, r9)
}

/// `[[g_gameRoot] + 0x69A8] + 0x1E0` + team*0x4A8: the team setup TeamRecordBuild reads.
fn setup_of(team: u8) -> Option<usize> {
    let g = G_ROOT.load(Ordering::Acquire);
    if g == 0 || team > 1 {
        return None;
    }
    let s = read_ptr(read_ptr(g)? + ms::ROOT_SETUP)?;
    Some(read_ptr(s + ms::SETUP_TEAMS)? + team as usize * ms::SETUP_TEAM_STRIDE)
}

/// First TeamRecordBuild of a setup: choose the ruleset and write its knobs into `mi_local`.
fn begin(mi_local: usize) -> SetupCtx {
    let user = read::<u8>(mi_local + ms::MI_USER).unwrap_or(0).min(1);
    let d = decide(mi_local, rival_team(user));
    let row = RowRules {
        period: read::<u16>(mi_local + ms::MI_PERIOD_SECS).unwrap_or(0),
        bits: read::<u32>(mi_local + ms::MI_RULES).unwrap_or(0),
        exrule: read::<u32>(mi_local + ms::MI_EXRULE).unwrap_or(0),
    };
    let ty = read::<u8>(mi_local + ms::MI_TYPE).unwrap_or(0);
    let head = format!(
        "setup: game 0x{:08X}{}, mode {} (type {ty}), rival team {}, user side {user}",
        d.keys.game,
        if d.keys.orig_game != 0 { format!(" (asked 0x{:08X})", d.keys.orig_game) } else { String::new() },
        d.keys.mode,
        d.keys.rival_team.map_or("?".into(), |t| format!("0x{t:08X}"))
    );
    match &d.rules {
        Some(r) if cfg().setup => {
            let (new, notes) = plan_setup(r, row);
            let mut failed = Vec::new();
            if new.period != row.period && !write::<u16>(mi_local + ms::MI_PERIOD_SECS, new.period) {
                failed.push("period");
            }
            if new.bits != row.bits && !write::<u32>(mi_local + ms::MI_RULES, new.bits) {
                failed.push("rule bits");
            }
            if new.exrule != row.exrule && !write::<u32>(mi_local + ms::MI_EXRULE, new.exrule) {
                failed.push("ExRule");
            }
            info!(
                "{head}: ruleset \"{}\" ({}): {}{}",
                r.id,
                d.source,
                if notes.is_empty() { "every setup knob = row".to_string() } else { notes.join(", ") },
                if failed.is_empty() { String::new() } else { format!(" (WRITE FAILED: {})", failed.join(", ")) }
            );
        }
        Some(r) => info!("{head}: ruleset \"{}\" ({}): setup knobs off in config, [end] only", r.id, d.source),
        None => info!("{head}: no ruleset ({})", d.source),
    }
    let rules = d.rules.clone();
    *DECIDED.lock().unwrap_or_else(|e| e.into_inner()) = Some(Decided { game: d.keys.game, d });
    *super::setup::TEAM_IDS.lock().unwrap_or_else(|e| e.into_inner()) = [None, None];
    SetupCtx { mi_local, teams_done: 0, user, rules }
}

unsafe extern "C" fn trb_detour(rec: u64, params: u64, r8: u64, r9: u64) -> u64 {
    let next: Fn4 = std::mem::transmute(NEXT_TRB.load(Ordering::Acquire));
    let (rec, p) = (rec as usize, params as usize);
    let team = read::<u8>(p + ms::PARAM_TEAM).unwrap_or(0xFF);
    if team > 1 {
        return next(rec as u64, params, r8, r9);
    }
    let mi_local = rec.wrapping_sub(ms::MI_TEAMS + team as usize * ms::TEAM_STRIDE);
    let before = read::<u32>(p + ms::PARAM_COUNT).unwrap_or(0);
    // choose (first call of a setup) and apply the per-team knobs
    let (count, formation, keeper, user) = match std::panic::catch_unwind(|| {
        let mut g = SETUP.lock().unwrap_or_else(|e| e.into_inner());
        let fresh = match g.as_ref() {
            None => true,
            Some(c) => c.mi_local != mi_local || c.teams_done & (1 << team) != 0,
        };
        if fresh {
            *g = Some(begin(mi_local));
        }
        let c = g.as_mut().unwrap();
        c.teams_done |= 1 << team;
        let rules = if cfg().setup { c.rules.as_ref() } else { None };
        (team_count(cfg(), rules, team, c.user), team_formation(cfg(), rules, team, c.user), team_goalkeeper(rules, team, c.user), c.user)
    }) {
        Ok(v) => v,
        Err(_) => {
            error!("setup hook panicked: team {team} built as the row says");
            (None, None, None, 0)
        }
    };
    if let Some(n) = count.filter(|&n| n != before) {
        if !write::<u32>(p + ms::PARAM_COUNT, n) {
            warning!("setup: team {team}: count write failed");
        }
    }
    let mut restore: Option<(usize, u32)> = None;
    if let (Some(f), Some(src)) = (formation, setup_of(team)) {
        let at = src + ms::SETUP_FORMATION;
        if let Some(old) = read::<u32>(at) {
            if old != f && write::<u32>(at, f) {
                restore = Some((at, old));
            }
        }
    }
    let r = next(rec as u64, params, r8, r9);
    if let Some((at, old)) = restore {
        let _ = write::<u32>(at, old);
    }
    let outfield = if keeper == Some(false) { Some(outfield_team(rec, team)) } else { None };
    let changed = count.is_some_and(|n| n != before) || restore.is_some() || outfield.is_some();
    if changed || cfg().probe_active() {
        let after = read::<u32>(rec + ms::TEAM_ON_PITCH).unwrap_or(0);
        let form = read::<u32>(rec + ms::TEAM_FORMATION).unwrap_or(0);
        let members: Vec<Member> = (0..ms::MEMBER_SLOTS)
            .map(|k| {
                let m = rec + (k + 1) * ms::TEAM_MEMBER_STRIDE;
                Member {
                    slot: k as u8,
                    position: read::<u8>(m + ms::MEM_POSITION).unwrap_or(0xFF),
                    chara: read::<u32>(m + ms::MEM_CHARA).unwrap_or(0),
                    flags: read::<u16>(m + ms::MEM_FLAGS).unwrap_or(ms::MEM_FLAG_EMPTY),
                }
            })
            .collect();
        let valid = read::<u32>(rec + ms::TEAM_VALID_MEMBERS).unwrap_or(0);
        info!(
            "{} ({} side, valid {valid}, result {})",
            team_line(team, before, after, form, restore.is_some(), &members),
            if team == user { "user" } else { "rival" },
            r & 0xFF
        );
    }
    r
}

/// `goalkeeper = false`: the office Desafío shift on the record just built (positions + 1, one more on the pitch;
/// the formation should be an all-outfield one, `[teams] formation`). Returns the log text.
fn outfield_team(rec: usize, team: u8) -> String {
    let mut mem = Vec::new();
    for k in 0..ms::MEMBER_SLOTS {
        let m = rec + (k + 1) * ms::TEAM_MEMBER_STRIDE;
        let flags = read::<u16>(m + ms::MEM_FLAGS).unwrap_or(ms::MEM_FLAG_EMPTY);
        let pos = read::<u8>(m + ms::MEM_POSITION).unwrap_or(0xFF);
        if flags & ms::MEM_FLAG_EMPTY == 0 && pos != 0xFF {
            mem.push((m, pos));
        }
    }
    let on = read::<u32>(rec + ms::TEAM_ON_PITCH).unwrap_or(0);
    let positions: Vec<u8> = mem.iter().map(|&(_, p)| p).collect();
    let Some((new_pos, new_on)) = outfield_shift(&positions, on) else {
        let t = format!("team {team} keeps its goalkeeper: not a plain team (on pitch {on}, positions {positions:?})");
        warning!("setup: {t}");
        return t;
    };
    let mut ok = true;
    for (&(m, _), &p) in mem.iter().zip(new_pos.iter()) {
        ok &= write::<u8>(m + ms::MEM_POSITION, p);
    }
    ok &= write::<u32>(rec + ms::TEAM_ON_PITCH, new_on);
    let t = format!("team {team} without goalkeeper: positions {positions:?} -> {new_pos:?}, on pitch {on} -> {new_on}, write {ok}");
    info!("setup: {t}");
    t
}
