//! End of match (docs/game/modes/prorroga-penaltis.md §8): extra time, golden goal and penalty shootout of the
//! match's ruleset, and the Lua commands of the match engine.
//!
//! * **NextHalf detour** ([`ms::NEXT_HALF`]): the engine asks `NextHalf(half)` at every period end (and in a few
//!   queries); the answer follows [`el::next_half`]: 3 / 4 = the engine's own extra halves, 5 = the shootout period,
//!   6 = over (golden goal).
//! * **InPlay detour** (slot 2 of the InPlay state vtable, every frame of live play, before the retail update): takes
//!   the ruleset chosen at the setup of a new match, sets the extra-half length (`mi+0x1C`), keeps the retail V-goal
//!   flag (golden goal) on while the ruleset wants it, and runs the shootout: one reserved penalty kick at a time (the
//!   reservation block of `ReservePenaltyKick` + restart type 8, written natively and consumed by the retail InPlay
//!   update of the same frame), [`KickWatch`] decides when each kick is over, [`Shootout`] when the shootout is. At the
//!   end the score of the result screen is set ([`el::final_score`]) and the period clock jumps to its end.
//!
//! | Command | Args | Returns |
//! |---|---|---|
//! | `CMND_EVT_MATCH_ENGINE_VERSION` | – | API version (2), rulesets loaded |
//! | `CMND_EVT_MATCH_ENGINE_LIST` | index (1-based) | id, name, mod (`false` past the end) |
//! | `CMND_EVT_MATCH_ENGINE_SELECT` | id (`""` / nil = clear, `"retail"` = the row's rules) | ok: the ruleset of the NEXT match |
//! | `CMND_EVT_MATCH_ENGINE_GET` | key (`"id"`, `"time.half_minutes"`, `"end.draw"`, …) | value of the running match's ruleset (else the selected one), `false` = none |
//! | `CMND_EVT_MATCH_ENGINE_STATE` | – | phase (`retail` / `regulation` / `extra_time` / `penalties` / `over`), ruleset id, score user, score rival, shootout user, shootout rival, winner (0 none, 1 user, 2 rival, 3 draw), extra time played, regulation score user, rival, source of the ruleset |

use super::{error, find_ruleset, host, info, mi, read, read_ptr, rulesets, warning, write, Pending, G_ROOT, PENDING};
use crate::end_logic::{self as el, KickFrame, KickWatch, Outcome, Phase, Shootout};
use crate::ruleset::{PenFirst, Ruleset, Value};
use crate::sigs as ms;
use evt_plugin_sdk::{Host, LuaCall, EVT_LUA_BOOLEAN, EVT_LUA_NIL, EVT_LUA_NUMBER, EVT_LUA_STRING};
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicUsize, Ordering};
use std::sync::{Mutex, OnceLock};

type NextHalfFn = unsafe extern "C" fn(u64) -> u64;
type UpdateFn = unsafe extern "C" fn(u64, u64) -> u64;

static NEXT_NEXT_HALF: AtomicUsize = AtomicUsize::new(0);
static ORIG_INPLAY: AtomicUsize = AtomicUsize::new(0);
static G_SCENE: AtomicUsize = AtomicUsize::new(0);
static G_ACTORS: AtomicUsize = AtomicUsize::new(0);
static G_BALL: AtomicUsize = AtomicUsize::new(0);
static G_GAME: AtomicUsize = AtomicUsize::new(0);
static FIND_FLAG: AtomicUsize = AtomicUsize::new(0);
/// Index of the restart-type flag in the temp byte flags (-1 = not looked up yet, -2 = lookup failed).
static FLAG_IDX: AtomicI32 = AtomicI32::new(-1);
static ACTIVE: AtomicBool = AtomicBool::new(false);
static T0: OnceLock<std::time::Instant> = OnceLock::new();

static CTX: Mutex<Option<MatchCtx>> = Mutex::new(None);

/// Tries of one reservation before the shootout is abandoned, and InPlay frames between tries.
const RESERVE_TRIES: u32 = 3;
const RESERVE_WAIT_FRAMES: u32 = 90;
/// Period length during the shootout (the clock must not end it) and before the final time-up.
const SHOOTOUT_PERIOD_SECS: u16 = 0xFFF0;
const END_PERIOD_SECS: u16 = 60;

#[derive(Debug, Clone, Copy, PartialEq)]
enum PkStep {
    Idle,
    Reserved,
    Live,
}

struct ShootRt {
    s: Shootout,
    /// Score per team index when the shootout started.
    base: [u8; 2],
    order: [Vec<u8>; 2],
    step: PkStep,
    /// Kicker of the current kick: (team, slot, actor handle).
    kicker: Option<(u8, u8, u32)>,
    watch: KickWatch,
    before: [u8; 2],
    wait: u32,
    tries: u32,
    aborted: Option<String>,
    end_logged: bool,
    end_frames: u32,
}

impl ShootRt {
    fn finished(&self) -> bool {
        self.s.outcome != Outcome::Running || self.aborted.is_some()
    }
}

struct MatchCtx {
    mi: usize,
    rules: Option<Ruleset>,
    source: String,
    user: u8,
    last_half: u8,
    last_clock: f32,
    last_ms: u64,
    score: [u8; 2],
    reg_score: Option<[u8; 2]>,
    et_played: bool,
    golden_by_us: bool,
    /// The periods of the ruleset (None = no ruleset: retail).
    flow: Option<el::Flow>,
    /// Current period of the flow (None = before the first / the skipped half 1).
    cur: Option<usize>,
    /// Period length at kick-off (s): the golden-goal period and the default extra halves.
    base_period: u16,
    /// Periods whose start was logged (bits).
    entered: u32,
    skip_logged: bool,
    warned_zero_et: bool,
    next_half_logged: Vec<(u8, u8, u8)>,
    shoot: Option<ShootRt>,
}

fn team_name(team: u8, user: u8) -> &'static str {
    if team == user { "user" } else { "rival" }
}

fn now_ms() -> u64 {
    T0.get_or_init(std::time::Instant::now).elapsed().as_millis() as u64 + 1
}

fn scene() -> Option<usize> {
    read_ptr(G_SCENE.load(Ordering::Acquire))
}

fn score(mi: usize) -> [u8; 2] {
    [read::<u8>(mi + ms::MI_SCORE).unwrap_or(0), read::<u8>(mi + ms::MI_SCORE + 1).unwrap_or(0)]
}

/// Ball owner team index (None = loose).
fn ball_owner() -> Option<u8> {
    let b = read_ptr(G_BALL.load(Ordering::Acquire))?;
    let h = read::<u32>(b + ms::BALL_OWNER).unwrap_or(0);
    if h == 0 {
        return None;
    }
    read::<u8>(b + ms::BALL_OWNER_TEAM).filter(|&t| t <= 1)
}

/// Actor handle of a valid on-pitch actor, else None.
fn valid_actor(hdl: u32) -> Option<u32> {
    let idx = (hdl & 0xFF) as usize;
    if hdl == 0 || idx >= ms::ACTOR_COUNT {
        return None;
    }
    let a = read_ptr(G_ACTORS.load(Ordering::Acquire))? + idx * ms::ACTOR_STRIDE;
    (read::<u32>(a + ms::ACTOR_HDL)? == hdl && read::<u8>(a + ms::ACTOR_IN_PLAY)? != 0).then_some(hdl)
}

fn team_rec(mi: usize, team: u8) -> usize {
    mi + ms::MI_TEAMS + team as usize * ms::TEAM_STRIDE
}

fn member(mi: usize, team: u8, slot: u8) -> usize {
    team_rec(mi, team) + (slot as usize + 1) * ms::TEAM_MEMBER_STRIDE
}

/// `(slot, position)` of the players on the pitch of `team`.
fn on_pitch(mi: usize, team: u8) -> Vec<(u8, u8)> {
    let count = read::<u32>(team_rec(mi, team) + ms::TEAM_ON_PITCH).unwrap_or(0);
    (0..ms::MEMBER_SLOTS as u8)
        .filter_map(|k| {
            let m = member(mi, team, k);
            let flags = read::<u16>(m + ms::MEM_FLAGS)?;
            let pos = read::<u8>(m + ms::MEM_POSITION)?;
            (flags & ms::MEM_NOT_PLAYING == 0 && (pos as u32) < count).then_some((k, pos))
        })
        .collect()
}

/// The V-goal flag of the clock object (golden goal).
fn set_vgoal(on: bool) -> Option<bool> {
    let sc = scene()?;
    let f = read::<u32>(sc + ms::SCENE_FLAGS)?;
    let want = if on { f | ms::FLAG_VGOAL } else { f & !ms::FLAG_VGOAL };
    if want != f {
        write::<u32>(sc + ms::SCENE_FLAGS, want);
        return Some(true);
    }
    Some(false)
}

/// Index of the restart-type temp flag (looked up once, game thread).
fn flag_index() -> Option<usize> {
    let cur = FLAG_IDX.load(Ordering::Acquire);
    if cur >= 0 {
        return Some(cur as usize);
    }
    if cur == -2 {
        return None;
    }
    let f = FIND_FLAG.load(Ordering::Acquire);
    let mut h: u32 = ms::FLAG_RESTART_TYPE;
    let entry = host().call(f, &[ms::FLAG_BANK as u64, &mut h as *mut u32 as u64]).ok().map(|e| e as usize);
    let idx = entry.and_then(|e| read::<u16>(e + 4)).filter(|&i| i < 0x10);
    match idx {
        Some(i) => {
            FLAG_IDX.store(i as i32, Ordering::Release);
            info!("restart-type flag 0x{:08X} = temp byte flag {i}", ms::FLAG_RESTART_TYPE);
            Some(i as usize)
        }
        None => {
            FLAG_IDX.store(-2, Ordering::Release);
            error!("restart-type flag 0x{:08X} not found (entry {entry:X?}): no penalty kicks", ms::FLAG_RESTART_TYPE);
            None
        }
    }
}

/// `ReservePenaltyKick` natively: the reservation block (team last: InPlay consumes it once the team byte is 0/1)
/// and the restart type 8.
fn reserve_penalty(team: u8, hdl: u32) -> Result<(), &'static str> {
    let g = read_ptr(G_GAME.load(Ordering::Acquire)).ok_or("no g_soccerGame")?;
    let idx = flag_index().ok_or("no restart-type flag")?;
    let root = read_ptr(G_ROOT.load(Ordering::Acquire)).ok_or("no game root")?;
    let flags = read_ptr(root + ms::ROOT_FLAGS).ok_or("no flag bank")?;
    let ok = write::<f32>(g + ms::RSV_X, 0.0)
        && write::<f32>(g + ms::RSV_Z, 0.0)
        && write::<f32>(g + ms::RSV_F, 0.0)
        && write::<u32>(g + ms::RSV_ACTOR, hdl)
        && write::<u32>(g + ms::RSV_ARG5, 0)
        && write::<u8>(g + ms::RSV_KIND, ms::KIND_FOUL)
        && write::<u8>(g + ms::RSV_ARG6, 0)
        && write::<u8>(g + ms::RSV_PAD, 0)
        && write::<u8>(flags + ms::FLAGS_BYTES + idx, ms::RESTART_PENALTY)
        && write::<u8>(g + ms::RSV_TEAM, team);
    if ok { Ok(()) } else { Err("write failed") }
}

fn reservation_pending() -> bool {
    read_ptr(G_GAME.load(Ordering::Acquire))
        .and_then(|g| read::<u8>(g + ms::RSV_TEAM))
        .is_some_and(|t| t <= 1)
}

// ---------------------------------------------------------------- NextHalf

unsafe extern "C" fn next_half_detour(half: u64) -> u64 {
    let orig: NextHalfFn = std::mem::transmute(NEXT_NEXT_HALF.load(Ordering::Acquire));
    let r = orig(half);
    if !ACTIVE.load(Ordering::Acquire) {
        return r;
    }
    let o = (r & 0xFF) as u8;
    let n = std::panic::catch_unwind(|| decide_next_half(half as u8, o)).unwrap_or(o);
    (r & !0xFF) | n as u64
}

fn decide_next_half(half: u8, orig: u8) -> u8 {
    let Ok(mut g) = CTX.try_lock() else { return orig };
    let Some(ctx) = g.as_mut() else { return orig };
    let Some(flow) = ctx.flow.as_ref() else { return orig };
    let Some(mi) = mi().filter(|&m| m == ctx.mi) else { return orig };
    let s = score(mi);
    let tied = s[0] == s[1];
    let done = ctx.shoot.as_ref().is_some_and(|x| x.finished());
    let n = el::next_half(orig, half, tied, flow, ctx.cur, done);
    if n != orig && !ctx.next_half_logged.contains(&(half, orig, n)) && ctx.next_half_logged.len() < 16 {
        ctx.next_half_logged.push((half, orig, n));
        let what = match n {
            el::HALF_OVER => "match over".to_string(),
            x => {
                let next = flow.index_on_enter(ctx.cur, x);
                format!("{} (half {x})", flow.phase_of(next).as_str())
            }
        };
        info!(
            "period end of half {half} ({}): engine {orig} -> {n} = {what} (score {}-{} user-rival, ruleset {})",
            flow.phase_of(ctx.cur).as_str(),
            s[ctx.user as usize & 1],
            s[1 - (ctx.user as usize & 1)],
            ctx.rules.as_ref().map_or("", |r| r.id.as_str())
        );
    }
    n
}

// ---------------------------------------------------------------- InPlay

unsafe extern "C" fn inplay_detour(state: u64, request: u64) -> u64 {
    let orig: UpdateFn = std::mem::transmute(ORIG_INPLAY.load(Ordering::Acquire));
    if ACTIVE.load(Ordering::Acquire) && std::panic::catch_unwind(on_inplay).is_err() {
        error!("InPlay hook panicked");
    }
    orig(state, request)
}

/// Time-up of the current period: the clock past its end (+ added time), every frame until the engine whistles.
fn time_up(mi: usize) {
    let mut len = read::<u16>(mi + ms::MI_PERIOD_SECS).unwrap_or(END_PERIOD_SECS);
    if len == 0 {
        // a stopped clock never ends the period
        len = END_PERIOD_SECS;
        let _ = write::<u16>(mi + ms::MI_PERIOD_SECS, len);
    }
    let added = read::<u32>(mi + ms::MI_ADDED).unwrap_or(0).min(3600);
    if let Some(sc) = scene() {
        let target = len as f32 + added as f32 + 1.0;
        if read::<f32>(sc + ms::SCENE_CLOCK).is_some_and(|c| c < target) {
            let _ = write::<f32>(sc + ms::SCENE_CLOCK, target);
        }
    }
}

fn on_inplay() {
    let Some(mi) = mi() else { return };
    let half = read::<u8>(mi + ms::MI_HALF).unwrap_or(0);
    let clock = scene().and_then(|s| read::<f32>(s + ms::SCENE_CLOCK)).unwrap_or(0.0);
    let now = now_ms();
    let mut g = CTX.lock().unwrap_or_else(|e| e.into_inner());
    let new_match = match g.as_ref() {
        None => true,
        Some(c) => c.mi != mi || half < c.last_half || (half == 1 && c.last_half == 1 && clock + 30.0 < c.last_clock),
    };
    if new_match {
        *g = Some(start_match(mi));
    }
    let Some(ctx) = g.as_mut() else { return };
    let dt = if ctx.last_ms == 0 { 0.0 } else { (now.saturating_sub(ctx.last_ms) as f32 / 1000.0).min(0.5) };
    ctx.last_ms = now;
    ctx.score = score(mi);
    let half_changed = half != ctx.last_half;
    // the same half again with its clock back at the start: a repeated period (golden goal after two extra halves)
    let reentry = !half_changed && half >= 3 && clock + 30.0 < ctx.last_clock;
    ctx.last_half = half;
    ctx.last_clock = clock;
    let (Some(rules), Some(flow)) = (ctx.rules.clone(), ctx.flow.clone()) else { return };
    let e = &rules.end;
    let u = ctx.user as usize & 1;
    // no regular halves: the engine's half 1 ends on its first frame of play
    if flow.skip_first && half == 1 && ctx.cur.is_none() {
        if !ctx.skip_logged {
            ctx.skip_logged = true;
            info!("no regular halves (ruleset {}): half 1 ends now, next {}", rules.id, flow.phase_of(Some(0)).as_str());
        }
        time_up(mi);
        return;
    }
    if half_changed || reentry {
        let before = ctx.cur;
        ctx.cur = flow.index_on_enter(ctx.cur, half);
        if ctx.cur != before {
            if let Some(i) = ctx.cur.filter(|&i| i < 32 && ctx.entered & (1 << i) == 0) {
                ctx.entered |= 1 << i;
                let p = flow.label(i);
                if flow.periods[i].phase != el::Phase::Regulation {
                    info!(
                        "{} (half {half}{}, score {}-{} user-rival, ruleset {})",
                        p,
                        if reentry { ", repeated" } else { "" },
                        ctx.score[u],
                        ctx.score[1 - u],
                        rules.id
                    );
                }
            }
        }
    }
    let phase = flow.phase_of(ctx.cur);
    if phase != el::Phase::Regulation && ctx.reg_score.is_none() {
        ctx.reg_score = Some(ctx.score);
    }
    // period length of the phase (the regular halves keep what the setup wrote)
    let want = match phase {
        el::Phase::ExtraTime => {
            ctx.et_played = true;
            let w = e.extra_time_secs(ctx.base_period);
            if w == 0 {
                if !ctx.warned_zero_et {
                    ctx.warned_zero_et = true;
                    warning!("extra time with a stopped clock (period 0) would never end: 15' per extra half");
                }
                Some(900)
            } else {
                Some(w)
            }
        }
        el::Phase::GoldenGoal => Some(ctx.base_period),
        _ => None,
    };
    if let Some(w) = want {
        let cur = read::<u16>(mi + ms::MI_PERIOD_SECS).unwrap_or(0);
        if cur != w && write::<u16>(mi + ms::MI_PERIOD_SECS, w) {
            info!("{} half {half}: period {cur} -> {w} s{}", phase.as_str(), if w == 0 { " (no time limit)" } else { "" });
        }
    }
    // golden-goal period: the retail V-goal flag (the next goal ends the match)
    if flow.golden_live(ctx.cur) {
        if set_vgoal(true) == Some(true) && !ctx.golden_by_us {
            ctx.golden_by_us = true;
            info!("golden goal live (half {half}): the next goal ends the match");
        }
    } else if ctx.golden_by_us && set_vgoal(false) == Some(true) {
        ctx.golden_by_us = false;
        info!("golden goal off (half {half})");
    }
    if phase == el::Phase::Penalties && half == el::HALF_SHOOTOUT {
        shootout_frame(ctx, mi, dt, &rules);
    }
}

/// A new match: the ruleset chosen at its setup (`setup::DECIDED`, same game id), else chosen now (the setup hook
/// missed it: only the `[end]` phases can still act), and the flow of periods it builds.
fn start_match(mi: usize) -> MatchCtx {
    let user = read::<u8>(mi + ms::MI_USER).unwrap_or(0).min(1);
    let rules_bits = read::<u32>(mi + ms::MI_RULES).unwrap_or(0);
    let ty = read::<u8>(mi + ms::MI_TYPE).unwrap_or(0);
    let game = read::<u32>(mi + ms::MI_GAME).unwrap_or(0);
    let base_period = read::<u16>(mi + ms::MI_PERIOD_SECS).unwrap_or(0);
    let taken = super::setup::DECIDED.lock().unwrap_or_else(|e| e.into_inner()).take().filter(|x| x.game == game);
    let (d, when) = match taken {
        Some(x) => (x.d, "setup"),
        None => (super::decide(mi, super::setup::rival_team(user)), "kick-off"),
    };
    let row_halves = if rules_bits & crate::BIT_TWO_HALVES != 0 { 2 } else { 1 };
    let flow = d.rules.as_ref().and_then(|r| match el::Flow::build(r.time.halves.unwrap_or(row_halves), &r.end) {
        Ok(f) => Some(f),
        Err(e) => {
            warning!("ruleset \"{}\": {e}: the row's flow", r.id);
            None
        }
    });
    match &d.rules {
        Some(r) => info!(
            "match start: ruleset \"{}\" ({}, chosen at {when}): {}; periods [{}] [mi 0x{mi:X}, game 0x{game:08X}, type {ty}, rules 0x{rules_bits:X}, period {base_period} s, user team {user}]",
            r.id,
            d.source,
            r.summary(),
            flow.as_ref().map_or("row".to_string(), |f| f.describe())
        ),
        None => info!("match start: no ruleset ({}, chosen at {when}) [game 0x{game:08X}, type {ty}, user team {user}]", d.source),
    }
    MatchCtx {
        mi,
        rules: d.rules,
        source: d.source,
        flow,
        cur: None,
        base_period,
        entered: 0,
        skip_logged: false,
        warned_zero_et: false,
        user,
        last_half: 0,
        last_clock: 0.0,
        last_ms: 0,
        score: score(mi),
        reg_score: None,
        et_played: false,
        golden_by_us: false,
        next_half_logged: Vec::new(),
        shoot: None,
    }
}

fn shootout_frame(ctx: &mut MatchCtx, mi: usize, dt: f32, rules: &Ruleset) {
    let e = &rules.end;
    let user = ctx.user & 1;
    if ctx.shoot.is_none() {
        let _ = write::<u16>(mi + ms::MI_PERIOD_SECS, SHOOTOUT_PERIOD_SECS);
        if ctx.golden_by_us {
            set_vgoal(false);
        }
        let first = if e.penalties_first == PenFirst::User { user } else { 1 - user };
        let order = [
            el::kicker_order(&on_pitch(mi, 0), e.penalties_order),
            el::kicker_order(&on_pitch(mi, 1), e.penalties_order),
        ];
        info!(
            "penalties: start at {}-{} user-rival, {} kicks{}, first {}, kickers (slots) user {:?} rival {:?}",
            ctx.score[user as usize],
            ctx.score[1 - user as usize],
            e.penalties_kicks,
            if e.penalties_sudden_death { " + sudden death" } else { "" },
            team_name(first, user),
            order[user as usize],
            order[1 - user as usize]
        );
        let mut rt = ShootRt {
            s: Shootout::new(e.penalties_kicks, e.penalties_sudden_death, first),
            base: ctx.score,
            order,
            step: PkStep::Idle,
            kicker: None,
            watch: KickWatch::default(),
            before: ctx.score,
            wait: 0,
            tries: 0,
            aborted: None,
            end_logged: false,
            end_frames: 0,
        };
        if rt.order[0].is_empty() || rt.order[1].is_empty() {
            rt.aborted = Some("a team has nobody on the pitch".into());
        }
        ctx.shoot = Some(rt);
    }
    let sc = ctx.score;
    let Some(rt) = ctx.shoot.as_mut() else { return };
    if rt.finished() {
        end_after_shootout(rt, mi, user, e.penalties_final_score, sc);
        return;
    }
    match rt.step {
        PkStep::Idle => next_kick(rt, mi, user, sc),
        PkStep::Reserved => {
            if !reservation_pending() {
                rt.step = PkStep::Live;
                rt.watch = KickWatch::new(rt.kicker.map_or(0, |k| k.0));
                live_frame(rt, mi, user, sc, dt);
            } else {
                rt.wait += 1;
                if rt.wait >= RESERVE_WAIT_FRAMES {
                    rt.wait = 0;
                    rt.tries += 1;
                    if rt.tries >= RESERVE_TRIES {
                        rt.aborted = Some(format!("penalty not taken after {RESERVE_TRIES} reservations"));
                        warning!("penalties: the engine did not start the penalty kick: shootout abandoned");
                    } else if let Some((t, _, h)) = rt.kicker {
                        let r = reserve_penalty(t, h);
                        warning!("penalties: reservation still pending after {RESERVE_WAIT_FRAMES} frames: written again ({r:?})");
                    }
                }
            }
        }
        PkStep::Live => live_frame(rt, mi, user, sc, dt),
    }
}

fn live_frame(rt: &mut ShootRt, mi: usize, user: u8, sc: [u8; 2], dt: f32) {
    let Some((team, slot, _)) = rt.kicker else {
        rt.step = PkStep::Idle;
        return;
    };
    let t = team as usize & 1;
    let f = KickFrame { owner: ball_owner(), scored: sc[t] > rt.before[t], dt };
    let Some((scored, why)) = rt.watch.step(&f) else { return };
    let n = rt.s.taken[t] + 1;
    let out = rt.s.record(scored);
    info!(
        "penalties: kick {n} of the {} (slot {slot}): {} ({why}); shootout {} user-rival",
        team_name(team, user),
        if scored { "GOAL" } else { "missed" },
        rt.s.score_text(user)
    );
    rt.step = PkStep::Idle;
    rt.kicker = None;
    match out {
        Outcome::Running => next_kick(rt, mi, user, sc),
        Outcome::Winner(w) => info!(
            "penalties: WINNER {} ({}-{} (pen. {}) user-rival, kicks {}+{})",
            team_name(w, user),
            rt.base[user as usize],
            rt.base[1 - user as usize],
            rt.s.score_text(user),
            rt.s.taken[user as usize],
            rt.s.taken[1 - user as usize]
        ),
        Outcome::Draw => info!("penalties: still level after the kicks, no sudden death: DRAW ({} user-rival)", rt.s.score_text(user)),
    }
}

fn next_kick(rt: &mut ShootRt, mi: usize, user: u8, sc: [u8; 2]) {
    let team = rt.s.next_team();
    let order = &rt.order[team as usize];
    let k = rt.s.next_kick_of(team) as usize;
    let pick = (0..order.len()).map(|i| order[(k + i) % order.len()]).find_map(|slot| {
        let hdl = read::<u32>(member(mi, team, slot) + ms::MEM_ACTOR).and_then(valid_actor)?;
        Some((slot, hdl))
    });
    let Some((slot, hdl)) = pick else {
        rt.aborted = Some(format!("no kicker on the pitch for the {}", team_name(team, user)));
        warning!("penalties: no valid kicker for the {}: shootout abandoned", team_name(team, user));
        return;
    };
    match reserve_penalty(team, hdl) {
        Ok(()) => {
            rt.kicker = Some((team, slot, hdl));
            rt.before = sc;
            rt.step = PkStep::Reserved;
            rt.wait = 0;
            rt.tries = 0;
            info!(
                "penalties: kick {} of the {}: slot {slot}, actor 0x{hdl:08X} (penalty kick reserved)",
                k + 1,
                team_name(team, user)
            );
        }
        Err(why) => {
            rt.aborted = Some(format!("reservation failed: {why}"));
            error!("penalties: reservation failed ({why}): shootout abandoned");
        }
    }
}

/// The shootout has a result (or was abandoned): the score of the result screen, then the time-up every frame until
/// the engine whistles (NextHalf(5) = 6).
fn end_after_shootout(rt: &mut ShootRt, mi: usize, user: u8, mode: crate::ruleset::PenFinalScore, now: [u8; 2]) {
    rt.end_frames += 1;
    if !rt.end_logged {
        rt.end_logged = true;
        let fin = el::final_score(rt.base, now, rt.s.outcome, mode);
        for t in 0..2 {
            let _ = write::<u8>(mi + ms::MI_SCORE + t, fin[t]);
        }
        let _ = write::<u16>(mi + ms::MI_PERIOD_SECS, END_PERIOD_SECS);
        info!(
            "penalties over{}: result screen score {}-{} user-rival (before the shootout {}-{}, shootout {}); ending the period",
            rt.aborted.as_ref().map(|a| format!(" (ABANDONED: {a})")).unwrap_or_default(),
            fin[user as usize],
            fin[1 - user as usize],
            rt.base[user as usize],
            rt.base[1 - user as usize],
            rt.s.score_text(user)
        );
    }
    // time-up: clock past the period (+ added time)
    let added = read::<u32>(mi + ms::MI_ADDED).unwrap_or(0).min(3600);
    let len = read::<u16>(mi + ms::MI_PERIOD_SECS).unwrap_or(END_PERIOD_SECS);
    if let Some(sc) = scene() {
        let target = len as f32 + added as f32 + 1.0;
        if read::<f32>(sc + ms::SCENE_CLOCK).is_some_and(|c| c < target) {
            let _ = write::<f32>(sc + ms::SCENE_CLOCK, target);
        }
    }
    if rt.end_frames == 180 {
        warning!("penalties: no final whistle 180 InPlay frames after the time-up (half {:?})", read::<u8>(mi + ms::MI_HALF));
    }
}

// ---------------------------------------------------------------- Lua commands

/// An argument for the log (`"text"`, 12, nil…).
fn describe(c: &LuaCall, i: i32) -> String {
    match c.arg_type(i) {
        EVT_LUA_NUMBER => {
            let v = c.num(i).unwrap_or(0.0);
            if v.fract() == 0.0 && v.abs() < 1e15 { format!("{}", v as i64) } else { format!("{v}") }
        }
        EVT_LUA_STRING => format!("{:?}", c.string(i).unwrap_or_default()),
        EVT_LUA_BOOLEAN => "bool".into(),
        EVT_LUA_NIL => "nil".into(),
        t => format!("<type {t}>"),
    }
}

fn push_value(c: &mut LuaCall, v: Value) {
    match v {
        Value::Str(s) => c.push_str(&s),
        Value::Int(i) => c.push_int(i),
        Value::Bool(b) => c.push_bool(b),
    }
}

/// API version of the commands: 2 = rulesets from mods + setup knobs; LIST also gives the mod, STATE the source.
pub(crate) const API_VERSION: i64 = 2;

fn cmd_version(c: &mut LuaCall) {
    c.push_int(API_VERSION);
    c.push_int(rulesets().len() as i64);
}

fn cmd_list(c: &mut LuaCall) {
    let i = c.int(0).unwrap_or(0);
    match (i >= 1).then(|| rulesets().get(i as usize - 1)).flatten() {
        Some(r) => {
            c.push_str(&r.id);
            c.push_str(&r.name);
            c.push_str(&r.source_mod);
        }
        None => c.push_bool(false),
    }
}

fn cmd_select(c: &mut LuaCall) {
    let id = c.string(0).map(|s| s.trim().to_string()).unwrap_or_default();
    let mut p = PENDING.lock().unwrap_or_else(|e| e.into_inner());
    if id.is_empty() {
        if p.take().is_some() {
            info!("selection cleared: the assignment tables decide the next match");
        }
        c.push_bool(true);
        return;
    }
    if id.eq_ignore_ascii_case("retail") {
        info!("\"retail\" selected: the next match keeps the rules of its row");
        *p = Some(Pending::Retail);
        c.push_bool(true);
        return;
    }
    match find_ruleset(&id) {
        Some(r) => {
            info!("ruleset \"{}\" selected for the next match: {}", r.id, r.summary());
            *p = Some(Pending::Ruleset(r.id));
            c.push_bool(true);
        }
        None => {
            warning!("CMND_EVT_MATCH_ENGINE_SELECT({}) refused: no such ruleset", describe(c, 0));
            c.push_bool(false);
        }
    }
}

fn cmd_get(c: &mut LuaCall) {
    let key = c.string(0).unwrap_or_default();
    let cur = CTX.lock().unwrap_or_else(|e| e.into_inner()).as_ref().and_then(|x| x.rules.clone());
    let r = cur.or_else(|| match PENDING.lock().unwrap_or_else(|e| e.into_inner()).clone() {
        Some(Pending::Ruleset(id)) => find_ruleset(&id),
        _ => None,
    });
    match r.and_then(|r| r.get(key.trim())) {
        Some(v) => push_value(c, v),
        None => c.push_bool(false),
    }
}

fn cmd_state(c: &mut LuaCall) {
    let g = CTX.lock().unwrap_or_else(|e| e.into_inner());
    let Some(ctx) = g.as_ref() else {
        c.push_str("retail");
        c.push_str("");
        return;
    };
    let u = ctx.user as usize & 1;
    let sc = if mi() == Some(ctx.mi) { score(ctx.mi) } else { ctx.score };
    let phase = match (&ctx.rules, &ctx.shoot) {
        (None, _) => "retail",
        (Some(_), Some(s)) if s.finished() => Phase::Over.as_str(),
        (Some(_), _) if ctx.last_half >= el::HALF_OVER => Phase::Over.as_str(),
        (Some(_), _) => ctx.flow.as_ref().map_or(Phase::of_half(ctx.last_half), |f| f.phase_of(ctx.cur)).as_str(),
    };
    let (pu, pr, winner) = match &ctx.shoot {
        Some(s) => (
            s.s.goals[u],
            s.s.goals[1 - u],
            match s.s.outcome {
                Outcome::Winner(w) => if w as usize == u { 1 } else { 2 },
                Outcome::Draw => 3,
                Outcome::Running => 0,
            },
        ),
        None => (0, 0, if sc[u] > sc[1 - u] { 1 } else if sc[u] < sc[1 - u] { 2 } else { 3 }),
    };
    let reg = ctx.reg_score.unwrap_or(sc);
    c.push_str(phase);
    c.push_str(ctx.rules.as_ref().map_or("", |r| r.id.as_str()));
    c.push_int(sc[u] as i64);
    c.push_int(sc[1 - u] as i64);
    c.push_int(pu as i64);
    c.push_int(pr as i64);
    c.push_int(winner);
    c.push_bool(ctx.et_played);
    c.push_int(reg[u] as i64);
    c.push_int(reg[1 - u] as i64);
    c.push_str(&ctx.source);
}

pub(super) fn register(host: &Host) {
    let cmds: [(&str, fn(&mut LuaCall)); 5] = [
        ("CMND_EVT_MATCH_ENGINE_VERSION", cmd_version),
        ("CMND_EVT_MATCH_ENGINE_LIST", cmd_list),
        ("CMND_EVT_MATCH_ENGINE_SELECT", cmd_select),
        ("CMND_EVT_MATCH_ENGINE_GET", cmd_get),
        ("CMND_EVT_MATCH_ENGINE_STATE", cmd_state),
    ];
    for (n, f) in cmds {
        if let Err(e) = host.lua_register(n, f) {
            error!("{n} not registered (code {e}: lua_bridge off or the name is taken)");
        }
    }
}

// ---------------------------------------------------------------- install

/// Init thread: resolve the globals, hook NextHalf + the InPlay update slot. True when the end rules are on.
pub(super) fn install(host: &Host) -> bool {
    let (Some(sc), Some(actors), Some(ball), Some(game)) = (
        super::resolve_rip(host, &ms::G_SCENE),
        super::resolve_rip(host, &ms::G_ACTORS),
        super::resolve_rip(host, &ms::G_BALL),
        super::resolve_rip(host, &ms::G_SOCCER_GAME),
    ) else {
        error!("a match global (scene / actors / ball / soccer game) not resolved: end rules off");
        return false;
    };
    G_SCENE.store(sc, Ordering::Release);
    G_ACTORS.store(actors, Ordering::Release);
    G_BALL.store(ball, Ordering::Release);
    G_GAME.store(game, Ordering::Release);
    let Some(ff) = host.sig(ms::FIND_FLAG.name, ms::FIND_FLAG.pattern, ms::FIND_FLAG.rva) else {
        error!("{} not found: end rules off", ms::FIND_FLAG.name);
        return false;
    };
    FIND_FLAG.store(ff, Ordering::Release);
    let Some(upd) = host.sig(ms::INPLAY_UPDATE.name, ms::INPLAY_UPDATE.pattern, ms::INPLAY_UPDATE.rva) else {
        error!("{} not found: end rules off", ms::INPLAY_UPDATE.name);
        return false;
    };
    let slot = host.exe_base() + ms::INPLAY_VTABLE_RVA + ms::VTABLE_UPDATE_SLOT * 8;
    let cur = read::<usize>(slot);
    if cur != Some(upd) {
        warning!("InPlay vtable slot at RVA 0x{:X} holds {cur:X?}, not 0x{upd:X}: another hook is there, chaining", ms::INPLAY_VTABLE_RVA + 16);
    }
    let Some(nh) = super::hook(host, &ms::NEXT_HALF, ms::NEXT_HALF_STEAL, next_half_detour as *const (), &NEXT_NEXT_HALF) else {
        error!("end rules off (NextHalf)");
        return false;
    };
    // the previous value first: the slot goes live the moment it is written
    ORIG_INPLAY.store(cur.unwrap_or(upd), Ordering::Release);
    if let Err(e) = unsafe { host.hook_ptr(slot, inplay_detour as *const (), &ORIG_INPLAY) } {
        error!("InPlay vtable slot not hooked (code {e}): end rules off (NextHalf stays hooked, pass-through)");
        return false;
    }
    ACTIVE.store(true, Ordering::Release);
    info!("end rules on: NextHalf hooked at 0x{nh:X}, InPlay update slot 0x{slot:X} (0x{upd:X})");
    true
}
