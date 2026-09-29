//! Rulesets (reglamentos) of the match engine: one TOML file per ruleset, `<mod>\rules\<id>.toml` of every active mod
//! that requires / provides `match_engine` (plus the legacy `evt_loader\match_engine\`), docs/game/modes/match-engine.md
//! §12. **Every field is its own simple rule** (a boolean or a number) and the fields combine freely; a missing knob
//! (or `"row"`) keeps the value the match row gave (retail), so a ruleset only says what it changes. Sections of the
//! schema not implemented yet are accepted and ignored.
//!
//! The match is a sequence of optional phases, in this order (each later phase only runs when the score is still level
//! after the phases played before it; the first phase always runs):
//! **regular halves** (`[time] halves`, 0..=2) → **extra time** (`extra_time`) → **golden goal** (`golden_goal`: one
//! period that ends at the first goal) → **penalties** (`penalties`).
//!
//! ```toml
//! id = "cup"                             # unique; the file name when missing
//! name = "Copa"
//! [time]
//! half_minutes = 30                      # minutes of each period (0..=90; 0 = no time limit, only with halves = 0);
//!                                        # also the length of the extra halves and of the golden-goal period
//! halves = 2                             # regular halves 0 | 1 | 2 (0 = no regular time); "row"
//! [teams]
//! players = 11                           # on-pitch count of both teams (1..=11); "row" = 11 full / 5 small match
//! goalkeeper = true                      # false = no goalkeeper (all outfield; needs an all-outfield formation)
//! user  = { players = 3, formation = 0x5505CEAF }   # per side, wins over the fields above
//! rival = { players = 3, goalkeeper = false }
//! [play]
//! exrule = [5]                           # ExRule bits to switch on (mi+0x2BF4, bit n = type n)
//! exrule_off = []                        # ExRule bits to switch off
//! [end]
//! extra_time = true                      # level after the regular halves: extra time
//! extra_time_halves = 2                  # 1 | 2
//! extra_time_minutes = 15                # optional (1..=60): default = half_minutes
//! golden_goal = true                     # still level: a golden-goal period (half_minutes; 0 = until the goal)
//! penalties = true                       # still level: penalty shootout
//! penalties_kicks = 5                    # kicks per team before sudden death (1..=11)
//! penalties_sudden_death = true          # false = still level after the kicks = draw
//! penalties_order = "forwards_first"     # "forwards_first" (last formation positions first) | "lineup"
//! penalties_first = "user"               # "user" | "rival": who kicks first
//! penalties_final_score = "winner_plus_one" # "winner_plus_one" (3-3 -> 4-3 on the result screen) | "sum"
//! extended_vgoal = "row"                 # the story's own «V-goal after a drawn match» (mi+0 bit 17): true | false | "row"
//! ```
//!
//! Deprecated (29/09 first cut, still read with a warning): `[end] draw = "draw" | "extra_time" | "penalties" |
//! "extra_time_then_penalties"` and `golden_goal = "off" | "always" | "extra_time"`.

use serde::{Deserialize, Deserializer, Serialize};
use std::path::Path;

/// Highest on-pitch count: the match AI treats positions >= 11 as off the pitch.
pub const MAX_PLAYERS: u8 = 11;
/// Longest period a ruleset may set (the period field is a u16 of seconds).
pub const MAX_HALF_MINUTES: u16 = 90;

// ---------------------------------------------------------------- knobs ("row" = keep the match row's value)

#[derive(Deserialize)]
#[serde(untagged)]
enum RawKnob<T> {
    V(T),
    S(String),
}

fn row_or<'de, D: Deserializer<'de>, T: Deserialize<'de>>(d: D) -> Result<Option<T>, D::Error> {
    match RawKnob::<T>::deserialize(d)? {
        RawKnob::V(v) => Ok(Some(v)),
        RawKnob::S(s) if s.trim().eq_ignore_ascii_case("row") || s.trim().is_empty() => Ok(None),
        RawKnob::S(s) => Err(serde::de::Error::custom(format!("\"{s}\": expected a value or \"row\""))),
    }
}

/// A hash knob: integer, `"0x…"` hex, `"row"`, or a name (crc32 of the name, like every id of the game).
pub fn parse_hash(s: &str) -> Option<u32> {
    let t = s.trim();
    if t.is_empty() || t.eq_ignore_ascii_case("row") {
        return None;
    }
    if let Some(h) = t.strip_prefix("0x").or_else(|| t.strip_prefix("0X")) {
        return u32::from_str_radix(h, 16).ok();
    }
    if t.chars().all(|c| c.is_ascii_digit()) {
        return t.parse::<u64>().ok().map(|v| v as u32);
    }
    Some(crc32fast::hash(t.as_bytes()))
}

fn hash_or_row<'de, D: Deserializer<'de>>(d: D) -> Result<Option<u32>, D::Error> {
    match RawKnob::<i64>::deserialize(d)? {
        RawKnob::V(v) => Ok(Some(v as u32)),
        RawKnob::S(s) => Ok(parse_hash(&s)),
    }
}

// ---------------------------------------------------------------- [time] [teams] [play]

/// `[time]`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct TimeRules {
    /// Minutes of each period (`mi+0x1C` = × 60; 0 = the clock stops: no time limit). None = the row's (c20). Also the
    /// length of the extra halves (unless `extra_time_minutes`) and of the golden-goal period.
    #[serde(deserialize_with = "row_or")]
    pub half_minutes: Option<u16>,
    /// Regular halves 0, 1 or 2 (`mi+0` bit 1; 0 = no regular time, the match starts with the next phase). None = the
    /// row's.
    #[serde(deserialize_with = "row_or")]
    pub halves: Option<u8>,
}

/// One side of `[teams]` (`user` / `rival`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct SideRules {
    #[serde(deserialize_with = "row_or")]
    pub players: Option<u8>,
    /// Formation id used to build the team (hash); None = the team's own.
    #[serde(deserialize_with = "hash_or_row")]
    pub formation: Option<u32>,
    /// false = no goalkeeper: every player is an outfield player (positions + 1, like the office Desafíos; needs an
    /// all-outfield formation). None / true = the retail keeper.
    #[serde(deserialize_with = "row_or")]
    pub goalkeeper: Option<bool>,
}

/// `[teams]`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct TeamsRules {
    /// On-pitch count of both teams (a side's own `players` wins). None = the engine's (11 / 5 / m_SoccerGameEx).
    #[serde(deserialize_with = "row_or")]
    pub players: Option<u8>,
    /// Goalkeeper of both teams (a side's own `goalkeeper` wins).
    #[serde(deserialize_with = "row_or")]
    pub goalkeeper: Option<bool>,
    pub user: SideRules,
    pub rival: SideRules,
}

impl TeamsRules {
    /// On-pitch count of the user side (`rival = false`) or the rival.
    pub fn players_of(&self, rival: bool) -> Option<u8> {
        let s = if rival { &self.rival } else { &self.user };
        s.players.or(self.players)
    }
    pub fn formation_of(&self, rival: bool) -> Option<u32> {
        if rival { self.rival.formation } else { self.user.formation }
    }
    pub fn goalkeeper_of(&self, rival: bool) -> Option<bool> {
        let s = if rival { &self.rival } else { &self.user };
        s.goalkeeper.or(self.goalkeeper)
    }
}

/// `[play]`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct PlayRules {
    /// ExRule bits switched on (`mi+0x2BF4`, bit n = type n, 1..=31).
    pub exrule: Vec<u8>,
    /// ExRule bits switched off.
    pub exrule_off: Vec<u8>,
}

impl PlayRules {
    /// New ExRule word from the row's.
    pub fn apply(&self, row: u32) -> u32 {
        let mut v = row;
        for &b in &self.exrule_off {
            v &= !(1u32 << (b & 31));
        }
        for &b in &self.exrule {
            v |= 1u32 << (b & 31);
        }
        v
    }
    pub fn is_row(&self) -> bool {
        self.exrule.is_empty() && self.exrule_off.is_empty()
    }
}

// ---------------------------------------------------------------- [end]

/// Deprecated `[end] draw` (29/09 first cut): read and mapped to `extra_time` / `penalties`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DrawRule {
    Draw,
    ExtraTime,
    Penalties,
    ExtraTimeThenPenalties,
}

impl DrawRule {
    pub fn extra_time(self) -> bool {
        matches!(self, DrawRule::ExtraTime | DrawRule::ExtraTimeThenPenalties)
    }
    pub fn penalties(self) -> bool {
        matches!(self, DrawRule::Penalties | DrawRule::ExtraTimeThenPenalties)
    }
}

/// `golden_goal` as written: a boolean, or the deprecated string form.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(untagged)]
pub enum BoolOrStr {
    B(bool),
    S(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum PenOrder {
    /// Highest formation positions first (forwards, then midfielders, then defenders), keeper last.
    #[default]
    ForwardsFirst,
    /// Formation position order (1, 2, 3 …), keeper last.
    Lineup,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum PenFirst {
    #[default]
    User,
    Rival,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum PenFinalScore {
    /// The score of the result screen = the score before the shootout with one more goal for the shootout winner.
    #[default]
    WinnerPlusOne,
    /// The shootout goals stay in the score (they are real goals of the engine).
    Sum,
}

/// `[end]` of a ruleset: the phases after the regular halves.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(default)]
pub struct EndRules {
    /// Extra time when level after the regular halves.
    pub extra_time: bool,
    /// Extra halves 1 or 2.
    pub extra_time_halves: u8,
    /// Minutes of each extra half (1..=60); None = `[time] half_minutes` (the match's period).
    pub extra_time_minutes: Option<u16>,
    /// A golden-goal period when still level (the first goal ends the match; it lasts `half_minutes`, 0 = no limit).
    #[serde(skip)]
    pub golden_goal: bool,
    /// Penalty shootout when still level.
    pub penalties: bool,
    pub penalties_kicks: u8,
    pub penalties_sudden_death: bool,
    pub penalties_order: PenOrder,
    pub penalties_first: PenFirst,
    pub penalties_final_score: PenFinalScore,
    /// The story's «V-goal after a drawn match» (`mi+0` bit 17, SOCCER_GAME_INFO c8). None = the row's.
    #[serde(deserialize_with = "row_or")]
    pub extended_vgoal: Option<bool>,
    /// `golden_goal` as written (normalised into [`Self::golden_goal`] by [`Ruleset::parse`]).
    #[serde(rename = "golden_goal")]
    pub golden_goal_raw: Option<BoolOrStr>,
    /// Deprecated `draw` (normalised into `extra_time` / `penalties`).
    #[serde(rename = "draw")]
    pub draw_raw: Option<DrawRule>,
}

impl Default for EndRules {
    fn default() -> Self {
        EndRules {
            extra_time: false,
            extra_time_halves: 2,
            extra_time_minutes: None,
            golden_goal: false,
            penalties: false,
            penalties_kicks: 5,
            penalties_sudden_death: true,
            penalties_order: PenOrder::ForwardsFirst,
            penalties_first: PenFirst::User,
            penalties_final_score: PenFinalScore::WinnerPlusOne,
            extended_vgoal: None,
            golden_goal_raw: None,
            draw_raw: None,
        }
    }
}

impl EndRules {
    /// Nothing different from retail.
    pub fn is_retail(&self) -> bool {
        !self.extra_time && !self.golden_goal && !self.penalties && self.extended_vgoal.is_none()
    }
    /// Seconds of one extra half, `period` = the match's period in seconds.
    pub fn extra_time_secs(&self, period: u16) -> u16 {
        self.extra_time_minutes.map_or(period, |m| m.saturating_mul(60))
    }
    /// Maps the deprecated keys and the `golden_goal` string form (`explicit` = keys written in the file's `[end]`).
    /// Returns the warnings; Err = an unknown string.
    fn normalize(&mut self, explicit: &dyn Fn(&str) -> bool) -> Result<Vec<String>, String> {
        let mut w = Vec::new();
        if let Some(d) = self.draw_raw.take() {
            w.push("end.draw is deprecated: use extra_time = true/false and penalties = true/false".to_string());
            if !explicit("extra_time") {
                self.extra_time = d.extra_time();
            }
            if !explicit("penalties") {
                self.penalties = d.penalties();
            }
        }
        match self.golden_goal_raw.take() {
            None => {}
            Some(BoolOrStr::B(b)) => self.golden_goal = b,
            Some(BoolOrStr::S(s)) => match s.trim() {
                "off" | "" => {
                    w.push("golden_goal = \"off\" is deprecated: golden_goal = false".into());
                    self.golden_goal = false;
                }
                "always" => {
                    w.push(
                        "golden_goal = \"always\" is deprecated: the golden goal is now its own phase after the others (golden_goal = true); \
                         for «first goal wins» from the kick-off use [time] halves = 0, half_minutes = 0 + golden_goal = true"
                            .into(),
                    );
                    self.golden_goal = true;
                }
                "extra_time" => {
                    w.push(
                        "golden_goal = \"extra_time\" is deprecated: an extra time that ends at the first goal is extra_time = false + \
                         golden_goal = true (the golden-goal period lasts half_minutes)"
                            .into(),
                    );
                    if self.extra_time {
                        self.extra_time = false;
                        self.golden_goal = true;
                    }
                }
                o => return Err(format!("golden_goal = \"{o}\": expected true or false")),
            },
        }
        Ok(w)
    }
    fn problems(&self) -> Vec<String> {
        let mut v = Vec::new();
        if let Some(m) = self.extra_time_minutes {
            if !(1..=60).contains(&m) {
                v.push(format!("end.extra_time_minutes = {m} (1..=60)"));
            }
        }
        if !(1..=2).contains(&self.extra_time_halves) {
            v.push(format!("end.extra_time_halves = {} (1 or 2)", self.extra_time_halves));
        }
        if !(1..=11).contains(&self.penalties_kicks) {
            v.push(format!("end.penalties_kicks = {} (1..=11)", self.penalties_kicks));
        }
        v
    }
    /// The phases after the regular halves, for the log: `"extra time 2x15', golden goal, penalties 5 + sudden death"`.
    pub fn phases_text(&self) -> String {
        let mut p = Vec::new();
        if self.extra_time {
            p.push(format!(
                "extra time {}x{}",
                self.extra_time_halves,
                self.extra_time_minutes.map_or("half_minutes".to_string(), |m| format!("{m}'"))
            ));
        }
        if self.golden_goal {
            p.push("golden goal".to_string());
        }
        if self.penalties {
            p.push(format!(
                "penalties {}{}{}",
                self.penalties_kicks,
                if self.penalties_sudden_death { " + sudden death" } else { " (no sudden death)" },
                match self.penalties_order {
                    PenOrder::ForwardsFirst => ", forwards first",
                    PenOrder::Lineup => ", lineup order",
                }
            ));
        }
        if p.is_empty() { "draw stands".into() } else { p.join(", ") }
    }
}

// ---------------------------------------------------------------- ruleset

/// One ruleset. Unknown sections / keys are ignored (later phases of the schema).
#[derive(Debug, Clone, PartialEq, Deserialize, Default)]
#[serde(default)]
pub struct Ruleset {
    pub id: String,
    pub name: String,
    pub version: u32,
    pub time: TimeRules,
    pub teams: TeamsRules,
    pub play: PlayRules,
    #[serde(rename = "end")]
    pub end: EndRules,
    /// Mod the file came from (`""` = legacy `evt_loader\match_engine\`).
    #[serde(skip)]
    pub source_mod: String,
    /// Deprecated keys found (the ruleset is used; the loader logs them).
    #[serde(skip)]
    pub warnings: Vec<String>,
}

impl Ruleset {
    /// Parse one file; `file_stem` is the id when the file has none. Err = the reason (the ruleset is not used).
    pub fn parse(text: &str, file_stem: &str) -> Result<Ruleset, String> {
        let mut r: Ruleset = toml::from_str(text).map_err(|e| e.to_string())?;
        let table: toml::Table = text.parse().map_err(|e: toml::de::Error| e.to_string())?;
        let end_keys = table.get("end").and_then(|e| e.as_table()).cloned().unwrap_or_default();
        r.warnings = r.end.normalize(&|k| end_keys.contains_key(k))?;
        r.id = r.id.trim().to_string();
        if r.id.is_empty() {
            r.id = file_stem.to_string();
        }
        if r.id.is_empty() || r.id.eq_ignore_ascii_case("retail") {
            return Err(format!("invalid id \"{}\" (empty or the reserved \"retail\")", r.id));
        }
        if r.name.trim().is_empty() {
            r.name = r.id.clone();
        }
        let p = r.problems();
        if !p.is_empty() {
            return Err(p.join(", "));
        }
        Ok(r)
    }

    fn problems(&self) -> Vec<String> {
        let mut v = self.end.problems();
        let e = &self.end;
        if let Some(m) = self.time.half_minutes {
            if m > MAX_HALF_MINUTES {
                v.push(format!("time.half_minutes = {m} (0..={MAX_HALF_MINUTES})"));
            }
            if m == 0 && self.time.halves != Some(0) {
                v.push("time.half_minutes = 0 (no time limit) needs time.halves = 0: regular halves would never end".into());
            }
            if m == 0 && e.extra_time && e.extra_time_minutes.is_none() {
                v.push("time.half_minutes = 0 with extra_time: set end.extra_time_minutes, else the extra time never ends".into());
            }
        }
        if let Some(h) = self.time.halves {
            if h > 2 {
                v.push(format!("time.halves = {h} (0, 1 or 2)"));
            }
            if h == 0 && !(e.extra_time || e.golden_goal || e.penalties) {
                v.push("time.halves = 0 with no extra_time, golden_goal or penalties: nothing to play".into());
            }
        }
        let pl = [("teams.players", self.teams.players), ("teams.user.players", self.teams.user.players), ("teams.rival.players", self.teams.rival.players)];
        for (k, n) in pl {
            if let Some(n) = n {
                if !(1..=MAX_PLAYERS).contains(&n) {
                    v.push(format!("{k} = {n} (1..={MAX_PLAYERS})"));
                }
            }
        }
        for &b in self.play.exrule.iter().chain(&self.play.exrule_off) {
            if !(1..=31).contains(&b) {
                v.push(format!("play.exrule bit {b} (1..=31)"));
            }
        }
        v
    }

    /// Nothing of the row is rewritten at setup (only the `[end]` phases act).
    pub fn setup_is_row(&self) -> bool {
        self.time == TimeRules::default()
            && self.teams.players_of(false).is_none()
            && self.teams.players_of(true).is_none()
            && self.teams.formation_of(false).is_none()
            && self.teams.formation_of(true).is_none()
            && self.teams.goalkeeper_of(false).is_none()
            && self.teams.goalkeeper_of(true).is_none()
            && self.play.is_row()
            && self.end.extended_vgoal.is_none()
    }

    /// One line for the log.
    pub fn summary(&self) -> String {
        let mut parts = Vec::new();
        let row = |o: Option<String>| o.unwrap_or_else(|| "row".into());
        if self.time != TimeRules::default() {
            parts.push(format!(
                "halves {} of {}",
                row(self.time.halves.map(|h| h.to_string())),
                row(self.time.half_minutes.map(|m| if m == 0 { "no limit".to_string() } else { format!("{m}'") }))
            ));
        }
        let (pu, pr) = (self.teams.players_of(false), self.teams.players_of(true));
        if pu.is_some() || pr.is_some() {
            parts.push(format!("players {}v{}", row(pu.map(|n| n.to_string())), row(pr.map(|n| n.to_string()))));
        }
        match (self.teams.goalkeeper_of(false), self.teams.goalkeeper_of(true)) {
            (Some(false), Some(false)) => parts.push("no goalkeepers".into()),
            (Some(false), _) => parts.push("user without goalkeeper".into()),
            (_, Some(false)) => parts.push("rival without goalkeeper".into()),
            _ => {}
        }
        if !self.play.is_row() {
            parts.push(format!("exrule +{:?} -{:?}", self.play.exrule, self.play.exrule_off));
        }
        parts.push(format!("then: {}", self.end.phases_text()));
        if let Some(v) = self.end.extended_vgoal {
            parts.push(format!("story V-goal {}", if v { "on" } else { "off" }));
        }
        parts.join(", ")
    }

    /// Value of one key for `CMND_EVT_MATCH_ENGINE_GET` (None = unknown key). Knobs left to the row give `"row"`.
    pub fn get(&self, key: &str) -> Option<Value> {
        let e = &self.end;
        let int_or_row = |o: Option<i64>| o.map_or(Value::Str("row".into()), Value::Int);
        Some(match key {
            "id" => Value::Str(self.id.clone()),
            "name" => Value::Str(self.name.clone()),
            "mod" => Value::Str(self.source_mod.clone()),
            "time.half_minutes" => int_or_row(self.time.half_minutes.map(i64::from)),
            "time.halves" => int_or_row(self.time.halves.map(i64::from)),
            "teams.user.players" => int_or_row(self.teams.players_of(false).map(i64::from)),
            "teams.rival.players" => int_or_row(self.teams.players_of(true).map(i64::from)),
            "teams.user.goalkeeper" => Value::Bool(self.teams.goalkeeper_of(false) != Some(false)),
            "teams.rival.goalkeeper" => Value::Bool(self.teams.goalkeeper_of(true) != Some(false)),
            "end.extra_time" => Value::Bool(e.extra_time),
            "end.extra_time_halves" => Value::Int(e.extra_time_halves as i64),
            "end.extra_time_minutes" => e.extra_time_minutes.map_or(Value::Str("half_minutes".into()), |m| Value::Int(m as i64)),
            "end.golden_goal" => Value::Bool(e.golden_goal),
            "end.penalties" => Value::Bool(e.penalties),
            "end.penalties_kicks" => Value::Int(e.penalties_kicks as i64),
            "end.penalties_sudden_death" => Value::Bool(e.penalties_sudden_death),
            "end.extended_vgoal" => e.extended_vgoal.map_or(Value::Str("row".into()), Value::Bool),
            // deprecated (29/09 first cut)
            "end.draw" => Value::Str(
                match (e.extra_time, e.penalties) {
                    (false, false) => "draw",
                    (true, false) => "extra_time",
                    (false, true) => "penalties",
                    (true, true) => "extra_time_then_penalties",
                }
                .into(),
            ),
            _ => return None,
        })
    }
}

/// A value returned to the Lua.
#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Str(String),
    Int(i64),
    Bool(bool),
}

/// Files of a rules folder that are not rulesets: `matches.toml` (the assignment table) and `_*.toml`.
pub fn is_ruleset_file(stem: &str) -> bool {
    !(stem.starts_with('_') || stem.eq_ignore_ascii_case("matches"))
}

/// All rulesets of a folder (`*.toml` except [`is_ruleset_file`] false, sorted by file name; a later duplicate id
/// replaces the earlier one, with a warning). Returns the rulesets and one message per problem (deprecated keys
/// included: those rulesets are used).
pub fn load_dir(dir: &Path) -> (Vec<Ruleset>, Vec<String>) {
    let mut out: Vec<Ruleset> = Vec::new();
    let mut msgs = Vec::new();
    let Ok(rd) = std::fs::read_dir(dir) else {
        return (out, msgs);
    };
    let mut files: Vec<std::path::PathBuf> =
        rd.flatten().map(|e| e.path()).filter(|p| p.extension().is_some_and(|x| x.eq_ignore_ascii_case("toml"))).collect();
    files.sort();
    for f in files {
        let stem = f.file_stem().and_then(|s| s.to_str()).unwrap_or("").to_string();
        if !is_ruleset_file(&stem) {
            continue;
        }
        let text = match std::fs::read_to_string(&f) {
            Ok(t) => t,
            Err(e) => {
                msgs.push(format!("{}: unreadable: {e}", f.display()));
                continue;
            }
        };
        match Ruleset::parse(&text, &stem) {
            Ok(r) => {
                for w in &r.warnings {
                    msgs.push(format!("{}: {w}", f.display()));
                }
                if let Some(i) = out.iter().position(|o| o.id == r.id) {
                    msgs.push(format!("{}: id \"{}\" repeated: this file replaces the earlier one", f.display(), r.id));
                    out[i] = r;
                } else {
                    out.push(r);
                }
            }
            Err(e) => msgs.push(format!("{}: not used: {e}", f.display())),
        }
    }
    (out, msgs)
}

/// Rulesets of several folders `(mod id, dir)` in order: a later folder's ruleset replaces an earlier one with the
/// same id (message: which mod replaced which).
pub fn load_sources(sources: &[(String, std::path::PathBuf)]) -> (Vec<Ruleset>, Vec<String>) {
    let mut out: Vec<Ruleset> = Vec::new();
    let mut msgs = Vec::new();
    for (m, dir) in sources {
        let (rs, ms) = load_dir(dir);
        msgs.extend(ms);
        for mut r in rs {
            r.source_mod = m.clone();
            if let Some(i) = out.iter().position(|o| o.id == r.id) {
                msgs.push(format!(
                    "ruleset \"{}\" of {} replaced by the one of {} (later in the load order)",
                    r.id,
                    label(&out[i].source_mod),
                    label(m)
                ));
                out[i] = r;
            } else {
                out.push(r);
            }
        }
    }
    (out, msgs)
}

fn label(m: &str) -> String {
    if m.is_empty() { "evt_loader\\match_engine".into() } else { format!("mod {m}") }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn full_end_section() {
        let r = Ruleset::parse(
            r#"
id = "cup"
name = "Copa"
[teams]
user = { players = 3 }
source = "5v5"              # later phase: ignored
[end]
extra_time = true
extra_time_minutes = 10
extra_time_halves = 1
golden_goal = true
penalties = true
penalties_kicks = 3
penalties_sudden_death = false
penalties_order = "lineup"
penalties_first = "rival"
penalties_final_score = "sum"
first_to = 3                # later phase: ignored
"#,
            "file",
        )
        .unwrap();
        assert_eq!(r.id, "cup");
        assert!(r.warnings.is_empty());
        let e = &r.end;
        assert!(e.extra_time && e.golden_goal && e.penalties);
        assert_eq!((e.extra_time_minutes, e.extra_time_halves, e.extra_time_secs(1800)), (Some(10), 1, 600));
        assert_eq!((e.penalties_kicks, e.penalties_sudden_death), (3, false));
        assert_eq!((e.penalties_order, e.penalties_first, e.penalties_final_score), (PenOrder::Lineup, PenFirst::Rival, PenFinalScore::Sum));
        assert_eq!(r.get("end.golden_goal"), Some(Value::Bool(true)));
        assert_eq!(r.get("end.draw"), Some(Value::Str("extra_time_then_penalties".into())));
        assert_eq!(r.get("end.penalties_kicks"), Some(Value::Int(3)));
        assert_eq!(r.teams.players_of(false), Some(3));
        assert_eq!(r.teams.players_of(true), None);
        assert_eq!(r.get("nope"), None);
        assert_eq!(e.phases_text(), "extra time 1x10', golden goal, penalties 3 (no sudden death), lineup order");
    }

    #[test]
    fn defaults_and_id_from_file() {
        let r = Ruleset::parse("[end]\npenalties = true\n", "pens_only").unwrap();
        assert_eq!(r.id, "pens_only");
        assert_eq!(r.name, "pens_only");
        assert_eq!(r.end.penalties_kicks, 5);
        assert!(r.end.penalties_sudden_death);
        assert!(!r.end.golden_goal && !r.end.extra_time && r.end.penalties);
        // the extra time lasts the match's period unless extra_time_minutes
        assert_eq!(r.end.extra_time_secs(900), 900);
        let empty = Ruleset::parse("", "x").unwrap();
        assert!(empty.end.is_retail() && empty.setup_is_row());
        assert_eq!(empty.end.phases_text(), "draw stands");
    }

    #[test]
    fn deprecated_forms_are_mapped_with_a_warning() {
        let r = Ruleset::parse("[end]\ndraw = \"extra_time_then_penalties\"\n", "x").unwrap();
        assert!(r.end.extra_time && r.end.penalties && !r.end.golden_goal);
        assert_eq!(r.warnings.len(), 1);
        // explicit new keys win over the alias
        let r = Ruleset::parse("[end]\ndraw = \"extra_time_then_penalties\"\npenalties = false\n", "x").unwrap();
        assert!(r.end.extra_time && !r.end.penalties);
        let g = |v: &str| Ruleset::parse(&format!("[end]\ndraw = \"extra_time\"\ngolden_goal = {v}\n"), "g").map(|r| (r.end.extra_time, r.end.golden_goal, r.warnings.len()));
        assert_eq!(g("true"), Ok((true, true, 1)));
        assert_eq!(g("false"), Ok((true, false, 1)));
        assert_eq!(g("\"off\""), Ok((true, false, 2)));
        assert_eq!(g("\"always\""), Ok((true, true, 2)));
        // «extra time ends at the first goal» = no extra time + a golden-goal period
        assert_eq!(g("\"extra_time\""), Ok((false, true, 2)));
        assert!(g("\"sometimes\"").is_err());
    }

    #[test]
    fn invalid_values_refused() {
        assert!(Ruleset::parse("[end]\nextra_time_halves = 3\n", "x").is_err());
        assert!(Ruleset::parse("[end]\npenalties_kicks = 0\n", "x").is_err());
        assert!(Ruleset::parse("[end]\nextra_time_minutes = 0\n", "x").is_err());
        assert!(Ruleset::parse("[end]\ndraw = \"coin_toss\"\n", "x").is_err());
        assert!(Ruleset::parse("[end]\nextra_time = \"yes\"\n", "x").is_err());
        assert!(Ruleset::parse("id = \"retail\"\n", "x").is_err());
        assert!(Ruleset::parse("[time]\nhalf_minutes = 91\n", "x").is_err());
        assert!(Ruleset::parse("[time]\nhalves = 3\n", "x").is_err());
        assert!(Ruleset::parse("[teams]\nplayers = 12\n", "x").is_err());
        assert!(Ruleset::parse("[teams]\nrival = { players = 0 }\n", "x").is_err());
        assert!(Ruleset::parse("[play]\nexrule = [0]\n", "x").is_err());
        assert!(Ruleset::parse("[time]\nhalf_minutes = \"long\"\n", "x").is_err());
        // combinations that cannot be played
        let e = Ruleset::parse("[time]\nhalves = 0\n", "x").unwrap_err();
        assert!(e.contains("nothing to play"), "{e}");
        let e = Ruleset::parse("[time]\nhalf_minutes = 0\n", "x").unwrap_err();
        assert!(e.contains("never end"), "{e}");
        let e = Ruleset::parse("[time]\nhalves = 0\nhalf_minutes = 0\n[end]\nextra_time = true\n", "x").unwrap_err();
        assert!(e.contains("extra_time_minutes"), "{e}");
    }

    #[test]
    fn combinations_that_play() {
        let ok = |t: &str| Ruleset::parse(t, "x").map(|r| r.end.phases_text());
        // only «first goal wins», no time limit
        assert_eq!(ok("[time]\nhalves = 0\nhalf_minutes = 0\n[end]\ngolden_goal = true\n"), Ok("golden goal".into()));
        // only a shootout
        assert_eq!(ok("[time]\nhalves = 0\n[end]\npenalties = true\n").map(|s| s.starts_with("penalties 5")), Ok(true));
        // 5' sudden death then penalties
        assert!(ok("[time]\nhalves = 0\nhalf_minutes = 5\n[end]\ngolden_goal = true\npenalties = true\n").is_ok());
        // only extra time (unlimited period needs its own length)
        assert!(ok("[time]\nhalves = 0\nhalf_minutes = 0\n[end]\nextra_time = true\nextra_time_minutes = 10\ngolden_goal = true\n").is_ok());
    }

    #[test]
    fn setup_knobs_and_row() {
        let r = Ruleset::parse(
            "[time]\nhalf_minutes = 15\nhalves = \"row\"\n[teams]\nplayers = 5\nrival = { players = 3, formation = \"0x5505CEAF\" }\n[play]\nexrule = [3, 5]\nexrule_off = [1]\n[end]\nextended_vgoal = false\n",
            "k",
        )
        .unwrap();
        assert_eq!((r.time.half_minutes, r.time.halves), (Some(15), None));
        assert_eq!((r.teams.players_of(false), r.teams.players_of(true)), (Some(5), Some(3)));
        assert_eq!((r.teams.formation_of(false), r.teams.formation_of(true)), (None, Some(0x5505CEAF)));
        assert_eq!(r.play.apply(0b10), 0b10_1000);
        assert_eq!(r.end.extended_vgoal, Some(false));
        assert!(!r.setup_is_row());
        assert_eq!(r.get("time.halves"), Some(Value::Str("row".into())));
        assert_eq!(r.get("time.half_minutes"), Some(Value::Int(15)));
        let k = Ruleset::parse("[teams]\ngoalkeeper = false\nuser = { goalkeeper = true }\n", "k").unwrap();
        assert_eq!((k.teams.goalkeeper_of(false), k.teams.goalkeeper_of(true)), (Some(true), Some(false)));
        assert_eq!(k.get("teams.rival.goalkeeper"), Some(Value::Bool(false)));
        assert!(k.summary().starts_with("rival without goalkeeper"), "{}", k.summary());
        assert!(r.summary().starts_with("halves row of 15', players 5v3, exrule +[3, 5] -[1], then: draw stands"), "{}", r.summary());
        let f = Ruleset::parse("[teams]\nuser = { formation = \"f_442\" }\n", "f").unwrap();
        assert_eq!(f.teams.formation_of(false), Some(crc32fast::hash(b"f_442")));
        assert_eq!(parse_hash("123"), Some(123));
        assert_eq!(parse_hash("row"), None);
    }

    #[test]
    fn folder_load_with_duplicates_errors_and_skipped_files() {
        let dir = std::env::temp_dir().join(format!("evt-plugin-match-engine-rs-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("a.toml"), "id = \"x\"\n[end]\nextra_time = true\n").unwrap();
        std::fs::write(dir.join("b.toml"), "id = \"x\"\n[end]\npenalties = true\n").unwrap();
        std::fs::write(dir.join("c.toml"), "[end]\npenalties = 3\n").unwrap();
        std::fs::write(dir.join("d.txt"), "ignored").unwrap();
        std::fs::write(dir.join("e.toml"), "[end]\ndraw = \"penalties\"\n").unwrap();
        std::fs::write(dir.join("matches.toml"), "[modes]\nfree = \"x\"\n").unwrap();
        std::fs::write(dir.join("_notes.toml"), "whatever = 1\n").unwrap();
        let (r, m) = load_dir(&dir);
        assert_eq!(r.len(), 2);
        assert!(r[0].end.penalties && !r[0].end.extra_time);
        assert_eq!(m.len(), 3, "{m:?}"); // repeated id, c.toml invalid, e.toml deprecated key
        let (r, m) = load_dir(&dir.join("missing"));
        assert!(r.is_empty() && m.is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn later_source_replaces_by_id() {
        let base = std::env::temp_dir().join(format!("evt-plugin-match-engine-src-{}", std::process::id()));
        let (a, b) = (base.join("a"), base.join("b"));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&a).unwrap();
        std::fs::create_dir_all(&b).unwrap();
        std::fs::write(a.join("free_match.toml"), "[time]\nhalf_minutes = 30\n").unwrap();
        std::fs::write(a.join("keep.toml"), "").unwrap();
        std::fs::write(b.join("free_match.toml"), "[time]\nhalf_minutes = 45\n").unwrap();
        let (r, m) = load_sources(&[("match_engine".into(), a), ("my_mod".into(), b)]);
        assert_eq!(r.len(), 2);
        let f = r.iter().find(|x| x.id == "free_match").unwrap();
        assert_eq!((f.time.half_minutes, f.source_mod.as_str()), (Some(45), "my_mod"));
        assert_eq!(m.len(), 1, "{m:?}");
        assert!(m[0].contains("mod match_engine") && m[0].contains("mod my_mod"), "{m:?}");
        let _ = std::fs::remove_dir_all(&base);
    }
}
