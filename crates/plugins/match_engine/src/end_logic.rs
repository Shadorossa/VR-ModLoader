//! Pure logic of the flow of a match (docs/game/modes/match-engine.md §12.5, prorroga-penaltis.md §8): the sequence of
//! periods a ruleset builds ([`Flow`]), what each period end gives ([`next_half`]), the penalty shootout
//! ([`Shootout`]), the kicker order and the watch that decides when a kick is over ([`KickWatch`]). The run time part
//! is `rt::end`.
//!
//! Halves (`mi+0x2BFB`, [code] `NextHalf 0x16CBDE0`): 1 / 2 regular, 3 / 4 the engine's own extra halves (it gives 3
//! after a drawn 2nd half when `mi+0` bit 0 is set, then 4, then 6 = over; no retail match sets that bit), 5 = a
//! period the engine already ends like 4 (`5 -> 6`), 6 = match over. The flow maps its phases onto those numbers:
//! regular halves 1-2, extra time 3-4, the golden-goal period on the next free number of 3-4 (4 again after two extra
//! halves: a second period 4), the shootout 5. With no regular halves the engine still starts at half 1: the period is
//! ended on its first frame of play ([`Flow::skip_first`]).

use crate::ruleset::{EndRules, PenFinalScore, PenOrder};

pub const HALF_OVER: u8 = 6;
pub const HALF_SHOOTOUT: u8 = 5;

/// Phase of the flow (also what `CMND_EVT_MATCH_ENGINE_STATE` shows).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    Regulation,
    ExtraTime,
    GoldenGoal,
    Penalties,
    Over,
}

impl Phase {
    /// Phase of a half number without a flow (retail matches).
    pub fn of_half(half: u8) -> Phase {
        match half {
            3 | 4 => Phase::ExtraTime,
            HALF_SHOOTOUT => Phase::Penalties,
            h if h >= HALF_OVER => Phase::Over,
            _ => Phase::Regulation,
        }
    }
    pub fn as_str(self) -> &'static str {
        match self {
            Phase::Regulation => "regulation",
            Phase::ExtraTime => "extra_time",
            Phase::GoldenGoal => "golden_goal",
            Phase::Penalties => "penalties",
            Phase::Over => "over",
        }
    }
}

/// One period of the flow: the engine's half number and its phase.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Period {
    pub half: u8,
    pub phase: Phase,
}

/// The periods of a match, built from the ruleset's independent fields (regular halves → extra time → golden goal →
/// penalties).
#[derive(Debug, Clone, PartialEq)]
pub struct Flow {
    pub periods: Vec<Period>,
    /// No regular halves: the engine's half 1 is ended on its first frame of play (not a period of the flow).
    pub skip_first: bool,
}

impl Flow {
    /// `regular` = regular halves (0..=2; the ruleset's `halves`, else the row's: 2 when `mi+0` bit 1 is set, else 1).
    pub fn build(regular: u8, e: &EndRules) -> Result<Flow, String> {
        if regular > 2 {
            return Err(format!("{regular} regular halves (0, 1 or 2)"));
        }
        let mut p: Vec<Period> = (1..=regular).map(|h| Period { half: h, phase: Phase::Regulation }).collect();
        if e.extra_time {
            for k in 0..e.extra_time_halves.clamp(1, 2) {
                p.push(Period { half: 3 + k, phase: Phase::ExtraTime });
            }
        }
        if e.golden_goal {
            let last = p.last().map_or(0, |x| x.half);
            p.push(Period { half: (last.max(2) + 1).min(4), phase: Phase::GoldenGoal });
        }
        if e.penalties {
            p.push(Period { half: HALF_SHOOTOUT, phase: Phase::Penalties });
        }
        if p.is_empty() {
            return Err("no regular halves and no extra time, golden goal or penalties: nothing to play".into());
        }
        Ok(Flow { periods: p, skip_first: regular == 0 })
    }

    /// The retail flow of a row (regular halves only).
    pub fn is_regular_only(&self) -> bool {
        !self.skip_first && self.periods.iter().all(|p| p.phase == Phase::Regulation)
    }

    /// Index of the period that starts when the engine enters `half` after period `cur` (None = the skipped half 1 or
    /// before the first period). A half repeated by the flow (a golden-goal period 4 after two extra halves) advances
    /// by one.
    pub fn index_on_enter(&self, cur: Option<usize>, half: u8) -> Option<usize> {
        let from = cur.map_or(0, |c| c + 1);
        (from..self.periods.len()).find(|&i| self.periods[i].half == half).or_else(|| cur.filter(|&c| self.periods.get(c).is_some_and(|p| p.half == half)))
    }

    /// Phase of period `cur` (None = the skipped half 1).
    pub fn phase_of(&self, cur: Option<usize>) -> Phase {
        cur.and_then(|c| self.periods.get(c)).map_or(Phase::Regulation, |p| p.phase)
    }

    /// The periods for the log: `"half 1 skipped, 3 golden_goal, 5 penalties"`.
    pub fn describe(&self) -> String {
        let mut v: Vec<String> = Vec::new();
        if self.skip_first {
            v.push("half 1 skipped".into());
        }
        v.extend(self.periods.iter().map(|p| format!("{} {}", p.half, p.phase.as_str())));
        v.join(", ")
    }

    /// Log label of period `i`: `"EXTRA TIME, second half"`, `"GOLDEN GOAL period"`, `"PENALTY SHOOTOUT"`.
    pub fn label(&self, i: usize) -> String {
        let Some(p) = self.periods.get(i) else { return "?".into() };
        let nth = self.periods[..i].iter().filter(|x| x.phase == p.phase).count();
        match p.phase {
            Phase::Regulation => format!("REGULAR half {}", nth + 1),
            Phase::ExtraTime => format!("EXTRA TIME, {} half", if nth == 0 { "first" } else { "second" }),
            Phase::GoldenGoal => "GOLDEN GOAL period".into(),
            Phase::Penalties => "PENALTY SHOOTOUT".into(),
            Phase::Over => "over".into(),
        }
    }

    /// Is the golden goal live in period `cur` (the first goal ends the match)?
    pub fn golden_live(&self, cur: Option<usize>) -> bool {
        self.phase_of(cur) == Phase::GoldenGoal
    }
}

/// What the engine's period end (`NextHalf(half)` = `orig`) must give in period `cur` of `flow` (`tied` = the score is
/// level now, `shootout_done` = a shootout of this match has a result). Also answers the engine's queries (HUD, V-goal
/// checks): it never changes state.
///
/// * the skipped half 1 (no regular halves): the first period of the flow;
/// * a golden-goal period with somebody ahead: 6 (the goal ended it);
/// * inside a phase (regular 1 → 2, extra 3 → 4): the next period of the phase (the retail answer for the regular
///   halves, so an early end the engine decides stays);
/// * the end of a phase: the next phase when the score is level, else 6; the last phase: 6 (the shootout: the
///   retail 6).
pub fn next_half(orig: u8, half: u8, tied: bool, flow: &Flow, cur: Option<usize>, shootout_done: bool) -> u8 {
    if orig == 0 {
        return orig;
    }
    let Some(c) = cur.filter(|&c| flow.periods.get(c).is_some_and(|p| p.half == half)) else {
        // the skipped half 1 (or a half the flow does not know: retail)
        return if flow.skip_first && half == 1 && cur.is_none() { flow.periods[0].half } else { orig };
    };
    let p = flow.periods[c];
    if p.phase == Phase::GoldenGoal && !tied {
        return HALF_OVER;
    }
    let next = flow.periods.get(c + 1);
    match next {
        // inside the regular halves: retail (half time, or the engine's own early end)
        Some(n) if p.phase == Phase::Regulation && n.phase == Phase::Regulation => orig,
        // inside the extra time: always the second extra half
        Some(n) if p.phase == Phase::ExtraTime && n.phase == Phase::ExtraTime => n.half,
        Some(n) if n.phase == Phase::Penalties && shootout_done => HALF_OVER,
        Some(n) if tied => n.half,
        Some(_) => HALF_OVER,
        // the last phase: over (the regular halves of a retail flow keep the engine's answer)
        None if p.phase == Phase::Regulation => orig,
        None => HALF_OVER,
    }
}

/// Result of a shootout.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    Running,
    /// The team index (0 / 1) that won.
    Winner(u8),
    /// Level after the kicks and no sudden death.
    Draw,
}

/// Hard limit of sudden-death rounds (then the shootout is a draw): 29 members per team, so every member twice.
pub const MAX_SUDDEN_ROUNDS: u8 = 58;

/// A penalty shootout between team indices 0 and 1.
#[derive(Debug, Clone, PartialEq)]
pub struct Shootout {
    pub kicks: u8,
    pub sudden_death: bool,
    /// Team index that kicks first in every round.
    pub first: u8,
    pub taken: [u8; 2],
    pub goals: [u8; 2],
    pub outcome: Outcome,
    /// Kick log `(team, scored)` in order.
    pub log: Vec<(u8, bool)>,
}

impl Shootout {
    pub fn new(kicks: u8, sudden_death: bool, first: u8) -> Shootout {
        Shootout { kicks: kicks.max(1), sudden_death, first: first & 1, taken: [0, 0], goals: [0, 0], outcome: Outcome::Running, log: Vec::new() }
    }

    /// Team index of the next kick (the first team whenever both have taken the same number).
    pub fn next_team(&self) -> u8 {
        if self.taken[0] == self.taken[1] { self.first } else { 1 - self.first }
    }

    /// Kick number (0-based) of `team`'s next kick: picks the kicker.
    pub fn next_kick_of(&self, team: u8) -> u8 {
        self.taken[team as usize & 1]
    }

    /// Record the next kick (of [`Self::next_team`]); returns the outcome after it.
    pub fn record(&mut self, scored: bool) -> Outcome {
        if self.outcome != Outcome::Running {
            return self.outcome;
        }
        let t = self.next_team() as usize;
        self.taken[t] += 1;
        if scored {
            self.goals[t] += 1;
        }
        self.log.push((t as u8, scored));
        self.outcome = self.decide();
        self.outcome
    }

    /// Early decision in the regulation kicks (the other team can no longer catch up), then sudden death by rounds.
    pub fn decide(&self) -> Outcome {
        let n = self.kicks;
        let [t0, t1] = self.taken;
        let [g0, g1] = self.goals;
        if t0 <= n && t1 <= n {
            let (left0, left1) = (n - t0, n - t1);
            if g0 > g1 + left1 {
                return Outcome::Winner(0);
            }
            if g1 > g0 + left0 {
                return Outcome::Winner(1);
            }
            if t0 < n || t1 < n {
                return Outcome::Running;
            }
            // n kicks each, level
            return if self.sudden_death { Outcome::Running } else { Outcome::Draw };
        }
        // sudden death: decided at the end of every round (both teams took the same number)
        if t0 == t1 {
            if g0 != g1 {
                return Outcome::Winner(if g0 > g1 { 0 } else { 1 });
            }
            if t0.saturating_sub(n) >= MAX_SUDDEN_ROUNDS {
                return Outcome::Draw;
            }
        }
        Outcome::Running
    }

    /// "4-2" from the point of view of `user`.
    pub fn score_text(&self, user: u8) -> String {
        let u = user as usize & 1;
        format!("{}-{}", self.goals[u], self.goals[1 - u])
    }
}

/// Kicker order of a team: `members` = `(slot, formation position)` of the players on the pitch. Keepers (position 0)
/// go last; the rest by [`PenOrder`]. Returns the slots.
pub fn kicker_order(members: &[(u8, u8)], order: PenOrder) -> Vec<u8> {
    let mut field: Vec<(u8, u8)> = members.iter().copied().filter(|&(_, p)| p != 0).collect();
    match order {
        PenOrder::ForwardsFirst => field.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0))),
        PenOrder::Lineup => field.sort_by(|a, b| a.1.cmp(&b.1).then(a.0.cmp(&b.0))),
    }
    let mut v: Vec<u8> = field.iter().map(|m| m.0).collect();
    v.extend(members.iter().filter(|m| m.1 == 0).map(|m| m.0));
    v
}

/// Score of the result screen after a shootout: `base` = the score (per team index) before the shootout, `now` = the
/// engine's score now (shootout goals included).
pub fn final_score(base: [u8; 2], now: [u8; 2], outcome: Outcome, mode: PenFinalScore) -> [u8; 2] {
    match (mode, outcome) {
        (PenFinalScore::Sum, _) => now,
        (PenFinalScore::WinnerPlusOne, Outcome::Winner(t)) => {
            let mut s = base;
            s[t as usize & 1] = s[t as usize & 1].saturating_add(1);
            s
        }
        (PenFinalScore::WinnerPlusOne, _) => base,
    }
}

/// One frame of live play seen by [`KickWatch`] (only InPlay frames reach the match engine).
#[derive(Debug, Clone, Copy)]
pub struct KickFrame {
    /// Team index owning the ball, None = loose / in flight.
    pub owner: Option<u8>,
    /// The kicking team's score is above its score before the kick.
    pub scored: bool,
    /// Real seconds since the previous InPlay frame.
    pub dt: f32,
}

/// Seconds of loose ball after the kick before it counts as missed (no goal).
pub const LOOSE_MISS_S: f32 = 4.0;
/// Seconds of live play after the penalty restart without a shot before it counts as missed.
pub const NO_SHOT_MISS_S: f32 = 30.0;

/// Decides when one penalty is over, from the InPlay frames after the penalty restart: a goal (the Goal state and the
/// kick-off come in between) = scored; the keeper / a defender holding the ball, the kicker's team touching it again
/// after the shot (rebound), [`LOOSE_MISS_S`] of loose ball or [`NO_SHOT_MISS_S`] without a shot = missed.
#[derive(Debug, Clone, Default)]
pub struct KickWatch {
    pub kicker_team: u8,
    /// The ball has left the kicker (seen loose once).
    pub released: bool,
    pub loose_s: f32,
    pub live_s: f32,
}

impl KickWatch {
    pub fn new(kicker_team: u8) -> KickWatch {
        KickWatch { kicker_team, ..Default::default() }
    }

    /// Some(scored) when the kick is over; the reason for the log in the second value.
    pub fn step(&mut self, f: &KickFrame) -> Option<(bool, &'static str)> {
        if f.scored {
            return Some((true, "goal"));
        }
        self.live_s += f.dt.max(0.0);
        match f.owner {
            Some(t) if t != self.kicker_team => return Some((false, "keeper / defender has the ball")),
            Some(_) if self.released => return Some((false, "rebound to the kicker's team")),
            Some(_) => {}
            None => {
                self.released = true;
                self.loose_s += f.dt.max(0.0);
                if self.loose_s >= LOOSE_MISS_S {
                    return Some((false, "loose ball, no goal"));
                }
            }
        }
        if self.live_s >= NO_SHOT_MISS_S {
            return Some((false, "no shot"));
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn end(et: bool, et_halves: u8, gg: bool, pens: bool) -> EndRules {
        EndRules { extra_time: et, extra_time_halves: et_halves, golden_goal: gg, penalties: pens, ..Default::default() }
    }

    fn halves(f: &Flow) -> Vec<(u8, Phase)> {
        f.periods.iter().map(|p| (p.half, p.phase)).collect()
    }

    use Phase::{ExtraTime as Et, GoldenGoal as Gg, Penalties as Pk, Regulation as Rg};

    #[test]
    fn flows_from_independent_fields() {
        let b = |r, e: EndRules| Flow::build(r, &e).map(|f| (halves(&f), f.skip_first));
        assert_eq!(b(2, end(false, 2, false, false)), Ok((vec![(1, Rg), (2, Rg)], false)));
        assert_eq!(b(1, end(false, 2, false, false)), Ok((vec![(1, Rg)], false)));
        assert_eq!(b(2, end(true, 2, false, true)), Ok((vec![(1, Rg), (2, Rg), (3, Et), (4, Et), (5, Pk)], false)));
        assert_eq!(b(2, end(true, 1, false, false)), Ok((vec![(1, Rg), (2, Rg), (3, Et)], false)));
        // golden goal: the next free half of 3-4
        assert_eq!(b(2, end(false, 2, true, false)), Ok((vec![(1, Rg), (2, Rg), (3, Gg)], false)));
        assert_eq!(b(2, end(true, 1, true, true)), Ok((vec![(1, Rg), (2, Rg), (3, Et), (4, Gg), (5, Pk)], false)));
        // classic cup with two extra halves: the golden-goal period repeats half 4
        assert_eq!(b(2, end(true, 2, true, true)), Ok((vec![(1, Rg), (2, Rg), (3, Et), (4, Et), (4, Gg), (5, Pk)], false)));
        // no regular halves
        assert_eq!(b(0, end(false, 2, true, false)), Ok((vec![(3, Gg)], true)));
        assert_eq!(b(0, end(false, 2, false, true)), Ok((vec![(5, Pk)], true)));
        assert_eq!(b(0, end(false, 2, true, true)), Ok((vec![(3, Gg), (5, Pk)], true)));
        assert_eq!(b(0, end(true, 2, false, false)), Ok((vec![(3, Et), (4, Et)], true)));
        assert!(b(0, end(false, 2, false, false)).is_err());
        assert!(b(3, end(false, 2, false, false)).is_err());
        assert!(Flow::build(2, &EndRules::default()).unwrap().is_regular_only());
        let c = Flow::build(0, &end(true, 2, true, true)).unwrap();
        assert_eq!(c.describe(), "half 1 skipped, 3 extra_time, 4 extra_time, 4 golden_goal, 5 penalties");
        assert_eq!((c.label(1).as_str(), c.label(2).as_str(), c.label(3).as_str()), ("EXTRA TIME, second half", "GOLDEN GOAL period", "PENALTY SHOOTOUT"));
    }

    #[test]
    fn retail_flow_changes_nothing() {
        for regular in [1, 2] {
            let f = Flow::build(regular, &EndRules::default()).unwrap();
            for half in 0..=6 {
                for orig in [0, 2, 3, 4, 6] {
                    for tied in [false, true] {
                        let cur = f.index_on_enter(None, half);
                        assert_eq!(next_half(orig, half, tied, &f, cur, false), orig, "regular {regular} half {half} orig {orig}");
                    }
                }
            }
        }
    }

    #[test]
    fn extra_time_then_penalties() {
        let f = Flow::build(2, &end(true, 2, false, true)).unwrap();
        let at = |h: u8| f.index_on_enter(None, h);
        assert_eq!(next_half(2, 1, true, &f, at(1), false), 2, "half time untouched");
        assert_eq!(next_half(6, 1, false, &f, at(1), false), 6, "the engine's early end stays");
        assert_eq!(next_half(6, 2, true, &f, at(2), false), 3, "level after the regular time: extra time");
        assert_eq!(next_half(6, 2, false, &f, at(2), false), 6, "a winner: over");
        assert_eq!(next_half(6, 3, false, &f, at(3), false), 4, "the second extra half is always played");
        assert_eq!(next_half(6, 4, true, &f, at(4), false), 5, "still level: shootout");
        assert_eq!(next_half(6, 4, false, &f, at(4), false), 6);
        assert_eq!(next_half(6, 4, true, &f, at(4), true), 6, "shootout done");
        assert_eq!(next_half(6, 5, true, &f, at(5), true), 6);
        let one = Flow::build(2, &end(true, 1, false, false)).unwrap();
        assert_eq!(next_half(4, 3, true, &one, one.index_on_enter(None, 3), false), 6, "a single extra half, no penalties: draw");
        let pens = Flow::build(2, &end(false, 2, false, true)).unwrap();
        assert_eq!(next_half(6, 2, true, &pens, pens.index_on_enter(None, 2), false), 5, "straight to the shootout");
        // a small match (one regular period) with extra time
        let small = Flow::build(1, &end(true, 1, false, false)).unwrap();
        assert_eq!(next_half(6, 1, true, &small, small.index_on_enter(None, 1), false), 3);
    }

    #[test]
    fn golden_goal_is_a_phase() {
        // normal match, then sudden death when level
        let f = Flow::build(2, &end(false, 2, true, false)).unwrap();
        let at = |h: u8| f.index_on_enter(None, h);
        assert_eq!(next_half(6, 2, true, &f, at(2), false), 3);
        assert_eq!(next_half(6, 2, false, &f, at(2), false), 6);
        assert!(f.golden_live(at(3)) && !f.golden_live(at(2)));
        assert_eq!(next_half(6, 3, false, &f, at(3), false), 6, "the golden goal ends it");
        assert_eq!(next_half(4, 3, true, &f, at(3), false), 6, "time up in the golden-goal period: draw");
        // cup: extra time, golden goal, penalties (two extra halves: the golden-goal period repeats half 4)
        let c = Flow::build(2, &end(true, 2, true, true)).unwrap();
        let et2 = c.index_on_enter(c.index_on_enter(c.index_on_enter(None, 2), 3), 4);
        assert_eq!(c.phase_of(et2), Et);
        assert_eq!(next_half(6, 4, true, &c, et2, false), 4, "level after extra time: golden-goal period");
        let gg = c.index_on_enter(et2, 4);
        assert_eq!(c.phase_of(gg), Gg);
        assert_eq!(next_half(6, 4, true, &c, gg, false), 5, "still level: shootout");
        assert_eq!(next_half(6, 4, false, &c, gg, false), 6);
    }

    #[test]
    fn no_regular_time() {
        // only «first goal wins»
        let f = Flow::build(0, &end(false, 2, true, false)).unwrap();
        assert!(f.skip_first);
        assert_eq!(next_half(6, 1, true, &f, None, false), 3, "the skipped half 1 leads to the golden-goal period");
        assert_eq!(f.phase_of(None), Rg);
        let gg = f.index_on_enter(None, 3);
        assert_eq!(next_half(6, 3, false, &f, gg, false), 6);
        // only a shootout: no draw check (0-0)
        let p = Flow::build(0, &end(false, 2, false, true)).unwrap();
        assert_eq!(next_half(6, 1, true, &p, None, false), 5);
        // 5' sudden death, then penalties
        let s = Flow::build(0, &end(false, 2, true, true)).unwrap();
        assert_eq!(next_half(6, 1, true, &s, None, false), 3);
        let g = s.index_on_enter(None, 3);
        assert_eq!(next_half(6, 3, true, &s, g, false), 5);
        assert_eq!(next_half(6, 3, false, &s, g, false), 6);
        // only extra time
        let e = Flow::build(0, &end(true, 2, false, false)).unwrap();
        assert_eq!(next_half(6, 1, true, &e, None, false), 3);
        assert_eq!(next_half(6, 3, false, &e, e.index_on_enter(None, 3), false), 4);
    }

    #[test]
    fn phases() {
        assert_eq!(Phase::of_half(1), Phase::Regulation);
        assert_eq!(Phase::of_half(2), Phase::Regulation);
        assert_eq!(Phase::of_half(3), Phase::ExtraTime);
        assert_eq!(Phase::of_half(4), Phase::ExtraTime);
        assert_eq!(Phase::of_half(5), Phase::Penalties);
        assert_eq!(Phase::of_half(6), Phase::Over);
        assert_eq!(Phase::GoldenGoal.as_str(), "golden_goal");
    }

    #[test]
    fn shootout_early_decision() {
        // 5 kicks: 3-0 after 3 each -> the other team cannot catch up with 2 left
        let mut s = Shootout::new(5, true, 0);
        for (a, b) in [(true, false), (true, false)] {
            assert_eq!(s.record(a), Outcome::Running);
            assert_eq!(s.record(b), Outcome::Running);
        }
        assert_eq!(s.record(true), Outcome::Running, "3-0 with team 1 still to kick its 3rd: 3 left for 1");
        assert_eq!(s.record(false), Outcome::Winner(0), "3-0, 2 left each: decided");
        assert_eq!(s.taken, [3, 3]);
        // decided in the middle of a round: 4-2 after 4 v 5? team 1 misses its 4th leaving 1 kick while 2 behind
        let mut s = Shootout::new(5, true, 0);
        for (a, b) in [(true, true), (true, false), (true, true)] {
            s.record(a);
            s.record(b);
        }
        assert_eq!(s.goals, [3, 2]);
        assert_eq!(s.record(true), Outcome::Running, "4-2, team 1 has 2 left");
        assert_eq!(s.record(false), Outcome::Winner(0), "4-2 with 1 left for team 1");
        // decided in the middle of a round, before the second team kicks: 4-1 after team 0's 4th (team 1 has 2 left)
        let mut s = Shootout::new(5, true, 0);
        for (a, b) in [(true, false), (true, true), (true, false)] {
            s.record(a);
            s.record(b);
        }
        assert_eq!(s.outcome, Outcome::Running, "3-1 after 3 each, 2 left each");
        assert_eq!(s.record(true), Outcome::Winner(0), "4-1 with 2 left for team 1: decided before it kicks");
        assert_eq!(s.record(true), Outcome::Winner(0), "no kick after the decision");
        assert_eq!(s.taken, [4, 3]);
        // 0-3 after 3 each: team 0 can reach 2 at most
        let mut s = Shootout::new(5, true, 0);
        s.record(false);
        s.record(true);
        s.record(false);
        s.record(true);
        s.record(false);
        assert_eq!(s.record(true), Outcome::Winner(1));
    }

    #[test]
    fn shootout_sudden_death() {
        let mut s = Shootout::new(5, true, 1);
        assert_eq!(s.next_team(), 1);
        for _ in 0..5 {
            s.record(true);
            s.record(true);
        }
        assert_eq!(s.outcome, Outcome::Running, "5-5: sudden death");
        assert_eq!(s.next_team(), 1);
        assert_eq!(s.record(false), Outcome::Running, "team 1 misses: team 0 still kicks");
        assert_eq!(s.next_team(), 0);
        assert_eq!(s.record(true), Outcome::Winner(0), "6-5 at the end of the round");
        assert_eq!(s.score_text(0), "6-5");
        assert_eq!(s.score_text(1), "5-6");
        // both score in a sudden-death round: continue
        let mut s = Shootout::new(1, true, 0);
        s.record(true);
        s.record(true);
        assert_eq!(s.outcome, Outcome::Running);
        s.record(true);
        assert_eq!(s.outcome, Outcome::Running, "round not finished");
        s.record(true);
        s.record(false);
        assert_eq!(s.record(false), Outcome::Running, "both miss");
        s.record(true);
        assert_eq!(s.record(false), Outcome::Winner(0));
    }

    #[test]
    fn shootout_without_sudden_death_and_limit() {
        let mut s = Shootout::new(3, false, 0);
        for _ in 0..3 {
            s.record(true);
            s.record(true);
        }
        assert_eq!(s.outcome, Outcome::Draw);
        let mut s = Shootout::new(1, true, 0);
        for _ in 0..=MAX_SUDDEN_ROUNDS {
            s.record(true);
            s.record(true);
        }
        assert_eq!(s.outcome, Outcome::Draw, "endless sudden death is cut");
    }

    #[test]
    fn kickers_skip_the_keeper() {
        let m = [(0, 0), (1, 1), (2, 2), (3, 3), (4, 4), (7, 10), (5, 9)];
        assert_eq!(kicker_order(&m, PenOrder::ForwardsFirst), vec![7, 5, 4, 3, 2, 1, 0]);
        assert_eq!(kicker_order(&m, PenOrder::Lineup), vec![1, 2, 3, 4, 5, 7, 0]);
        assert_eq!(kicker_order(&[(3, 0)], PenOrder::Lineup), vec![3], "only the keeper left");
    }

    #[test]
    fn final_scores() {
        assert_eq!(final_score([3, 3], [7, 5], Outcome::Winner(0), PenFinalScore::WinnerPlusOne), [4, 3]);
        assert_eq!(final_score([3, 3], [7, 5], Outcome::Winner(1), PenFinalScore::WinnerPlusOne), [3, 4]);
        assert_eq!(final_score([3, 3], [7, 5], Outcome::Winner(0), PenFinalScore::Sum), [7, 5]);
        assert_eq!(final_score([1, 1], [4, 4], Outcome::Draw, PenFinalScore::WinnerPlusOne), [1, 1]);
    }

    #[test]
    fn kick_watch() {
        let f = |owner, scored, dt| KickFrame { owner, scored, dt };
        let mut w = KickWatch::new(0);
        assert_eq!(w.step(&f(Some(0), false, 0.1)), None, "kicker holds the ball before the shot");
        assert_eq!(w.step(&f(None, false, 0.1)), None, "shot in flight");
        assert_eq!(w.step(&f(Some(1), false, 0.1)), Some((false, "keeper / defender has the ball")));
        let mut w = KickWatch::new(1);
        w.step(&f(None, false, 0.1));
        assert_eq!(w.step(&f(Some(1), false, 0.1)), Some((false, "rebound to the kicker's team")));
        let mut w = KickWatch::new(1);
        assert_eq!(w.step(&f(Some(0), true, 0.1)), Some((true, "goal")), "goal seen after the kick-off");
        let mut w = KickWatch::new(0);
        for _ in 0..7 {
            assert_eq!(w.step(&f(None, false, 0.5)), None);
        }
        assert_eq!(w.step(&f(None, false, 0.5)).map(|r| r.0), Some(false), "4 s loose");
        let mut w = KickWatch::new(0);
        assert_eq!(w.step(&f(Some(0), false, NO_SHOT_MISS_S)).map(|r| r.1), Some("no shot"));
    }

}
