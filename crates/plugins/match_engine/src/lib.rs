//! Plugin `match_engine` (mod `mods\match_engine\`, `match_engine.dll`): the match engine as its own mod (ModLoader
//! phase 3, docs/app/modloader-roadmap.md). A **ruleset** (reglamento) decides the rules of **every offline match**:
//! the engine's per-match rule fields are taken from the ruleset the match gets, not trusted from the match row
//! (docs/game/modes/match-engine.md §12). The built-in base rulesets of the mod reproduce the retail rules of each mode;
//! mods add or replace rulesets and reassign them per mode, match row or rival team. Online and observer matches are
//! never touched (their rules come from the server / peer).
//!
//! Parts (same behaviour and log lines as the built-in loader modules `match_engine` and `match_rules` it replaces;
//! the built-ins yield because the mod `provides = ["match_engine", "match_rules"]`):
//! * [`ruleset`]: the ruleset format and loading (`<mod>\rules\*.toml` of every mod that uses the engine, the legacy
//!   `evt_loader\match_engine\`);
//! * [`assign`]: `<mod>\rules\matches.toml` (mode / match row / rival team / mod default → ruleset) and the precedence;
//! * setup ([`plan_setup`] pure, `rt::setup`): at `TeamRecordBuild` (the match is still being built) the ruleset's
//!   knobs are written into the match being built: period (`mi+0x1C`), halves (`mi+0` bit 1), story V-goal (bit 17),
//!   ExRule (`mi+0x2BF4`), on-pitch count and formation per team. `TeamBuild 0xE9C190` is only read (rival team id);
//! * end of match ([`end_logic`] pure, `rt::end`): extra time, golden goal, penalty shootout (prorroga-penaltis.md §8);
//! * [`match_rules`]: the Opciones «Duración de cada parte» (`evt_loader\match_rules.json`, `CMND_EVT_MATCH_RULES_*`).

pub mod assign;
pub mod end_logic;
pub mod match_rules;
pub mod ruleset;
pub mod sigs;

#[cfg(all(windows, target_arch = "x86_64"))]
mod rt;

use serde::{Deserialize, Serialize};

pub use ruleset::MAX_PLAYERS;

/// Rulesets / assignment table inside a mod folder.
pub const RULES_DIR: &str = "rules";
/// The assignment table inside [`RULES_DIR`].
pub const MATCHES_FILE: &str = "matches.toml";
/// Legacy folder of rulesets inside `evt_loader` (the built-in module's).
pub const LEGACY_DIR: &str = "match_engine";

/// Configuration: `mods\match_engine\config.toml`, the legacy `[match_engine]` section and `[mods.match_engine]` of
/// `evt_loader\config.toml` (merged by the ModLoader, the last one wins).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct MatchEngineCfg {
    /// Probe: on-pitch count per team `[team0, team1]` (team 0 = user side), 1..=11; 0 or missing = the ruleset's /
    /// engine's. Wins over the ruleset (testing).
    pub probe_players: Vec<u8>,
    /// Probe: formation id per team `[team0, team1]` (0 = keep). Wins over the ruleset (testing).
    pub probe_formation: Vec<u32>,
    /// Testing: id of a ruleset applied to every offline full / small match (type 1 / 2, story V-goal rows excluded)
    /// that has no Lua selection, above the assignment tables. Empty = off.
    pub test_ruleset: String,
    /// Opciones half length (`CMND_EVT_MATCH_RULES_*`). false = the commands are not registered.
    pub match_rules: bool,
    /// Rulesets of the legacy folder `evt_loader\match_engine\` too (loaded last: they win by id).
    pub legacy_folder: bool,
    /// Setup knobs (period, halves, players, ExRule…). false = only `[end]` rules act (the 29/09 behaviour).
    pub setup: bool,
}

impl Default for MatchEngineCfg {
    fn default() -> Self {
        MatchEngineCfg {
            probe_players: Vec::new(),
            probe_formation: Vec::new(),
            test_ruleset: String::new(),
            match_rules: true,
            legacy_folder: true,
            setup: true,
        }
    }
}

impl MatchEngineCfg {
    /// Probe count of `team`, None = keep.
    pub fn probe_count(&self, team: u8) -> Option<u32> {
        match self.probe_players.get(team as usize).copied() {
            Some(n) if (1..=MAX_PLAYERS).contains(&n) => Some(n as u32),
            _ => None,
        }
    }

    /// Probe formation of `team`, None = keep.
    pub fn probe_formation(&self, team: u8) -> Option<u32> {
        self.probe_formation.get(team as usize).copied().filter(|&f| f != 0)
    }

    /// Probe rewrites something.
    pub fn probe_active(&self) -> bool {
        (0..2).any(|t| self.probe_count(t).is_some() || self.probe_formation(t).is_some())
    }

    /// The test ruleset id (None = off).
    pub fn test_ruleset(&self) -> Option<&str> {
        let t = self.test_ruleset.trim();
        (!t.is_empty()).then_some(t)
    }

    /// Config problems for the log (ignored entries).
    pub fn problems(&self) -> Vec<String> {
        let mut v = Vec::new();
        if self.probe_players.len() > 2 {
            v.push(format!("probe_players has {} entries: only the first 2 (team 0, team 1) are used", self.probe_players.len()));
        }
        if self.probe_formation.len() > 2 {
            v.push(format!("probe_formation has {} entries: only the first 2 are used", self.probe_formation.len()));
        }
        for (t, &n) in self.probe_players.iter().take(2).enumerate() {
            if n > MAX_PLAYERS {
                v.push(format!("probe_players[{t}] = {n} ignored (1..={MAX_PLAYERS}, 0 = keep)"));
            }
        }
        v
    }

    /// One summary line of the probe.
    pub fn probe_summary(&self) -> String {
        let c = |t: u8| self.probe_count(t).map_or("keep".to_string(), |n| n.to_string());
        let f = |t: u8| self.probe_formation(t).map_or("keep".to_string(), |x| format!("0x{x:08X}"));
        if self.probe_active() {
            format!("players team0 {} team1 {}, formation team0 {} team1 {}", c(0), c(1), f(0), f(1))
        } else {
            "off (retail counts, log only)".to_string()
        }
    }
}

/// One member of a built team record.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Member {
    pub slot: u8,
    pub position: u8,
    pub chara: u32,
    pub flags: u16,
}

impl Member {
    /// Same rule as EstáEnCampo 0xEB2900 (outside training matches).
    pub fn on_pitch(&self, count: u32) -> bool {
        (self.position as u32) < count && self.flags & 0x808 == 0
    }
}

/// Post-hook log line: count, formation, members ordered by position (`pos:slot:chara` + `*` when on the pitch).
pub fn team_line(team: u8, before: u32, after: u32, formation: u32, form_forced: bool, members: &[Member]) -> String {
    let mut m: Vec<&Member> = members.iter().filter(|m| m.flags & sigs::MEM_FLAG_EMPTY == 0 && m.position != 0xFF).collect();
    m.sort_by_key(|m| (m.position, m.slot));
    let on = m.iter().filter(|m| m.on_pitch(after)).count();
    let list: Vec<String> =
        m.iter().map(|m| format!("{}:{}:{:08X}{}", m.position, m.slot, m.chara, if m.on_pitch(after) { "*" } else { "" })).collect();
    format!(
        "probe: team {team} on-pitch {before} -> {after} ({on} members on the pitch), formation 0x{formation:08X}{}, members pos:slot:chara [{}]",
        if form_forced { " (forced)" } else { "" },
        list.join(" ")
    )
}

// ---------------------------------------------------------------- setup plan (pure)

/// The rule fields of the match being built, as the row left them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RowRules {
    /// `mi+0x1C` seconds of each half.
    pub period: u16,
    /// `mi+0` rule bits.
    pub bits: u32,
    /// `mi+0x2BF4` ExRule bits.
    pub exrule: u32,
}

/// `mi+0` bit 1: two halves (NextHalf 0x16CBDE0); bit 17: story V-goal after a drawn match (0x16B72A0).
pub const BIT_TWO_HALVES: u32 = 1 << 1;
pub const BIT_EXTENDED_VGOAL: u32 = 1 << 17;

/// The rule fields a ruleset leaves (`new`) and one note per knob for the log (`"period 1800 = row"`, `"period 1800 ->
/// 900"`, knobs left to the row are not listed).
pub fn plan_setup(r: &ruleset::Ruleset, row: RowRules) -> (RowRules, Vec<String>) {
    let mut new = row;
    let mut notes = Vec::new();
    let note = |notes: &mut Vec<String>, what: &str, a: String, b: String| {
        notes.push(if a == b { format!("{what} {a} = row") } else { format!("{what} {a} -> {b}") });
    };
    if let Some(m) = r.time.half_minutes {
        new.period = m.saturating_mul(60);
        note(&mut notes, "period", format!("{}s", row.period), format!("{}s", new.period));
    }
    if let Some(h) = r.time.halves {
        // 0 regular halves: one engine period (ended on its first frame by the flow)
        new.bits = if h >= 2 { new.bits | BIT_TWO_HALVES } else { new.bits & !BIT_TWO_HALVES };
        let n = |b: u32| if b & BIT_TWO_HALVES != 0 { "2" } else { "1" }.to_string();
        let to = if h == 0 { "0 (skipped)".to_string() } else { n(new.bits) };
        note(&mut notes, "halves", n(row.bits), to);
    }
    if let Some(v) = r.end.extended_vgoal {
        new.bits = if v { new.bits | BIT_EXTENDED_VGOAL } else { new.bits & !BIT_EXTENDED_VGOAL };
        let n = |b: u32| if b & BIT_EXTENDED_VGOAL != 0 { "on" } else { "off" }.to_string();
        note(&mut notes, "story V-goal", n(row.bits), n(new.bits));
    }
    if !r.play.is_row() {
        new.exrule = r.play.apply(row.exrule);
        note(&mut notes, "ExRule", format!("0x{:X}", row.exrule), format!("0x{:X}", new.exrule));
    }
    (new, notes)
}

/// On-pitch count of one team at `TeamRecordBuild`: the probe (testing) wins, then the ruleset; None = keep the
/// engine's `engine` (11 full / 5 small / m_SoccerGameEx).
pub fn team_count(cfg: &MatchEngineCfg, r: Option<&ruleset::Ruleset>, team: u8, user: u8) -> Option<u32> {
    cfg.probe_count(team).or_else(|| r.and_then(|r| r.teams.players_of(team != user)).map(u32::from))
}

/// Formation id of one team at `TeamRecordBuild` (probe first, then the ruleset); None = the team's own.
pub fn team_formation(cfg: &MatchEngineCfg, r: Option<&ruleset::Ruleset>, team: u8, user: u8) -> Option<u32> {
    cfg.probe_formation(team).or_else(|| r.and_then(|r| r.teams.formation_of(team != user)))
}

/// Goalkeeper of one team at `TeamRecordBuild` (the ruleset; None = the retail keeper).
pub fn team_goalkeeper(r: Option<&ruleset::Ruleset>, team: u8, user: u8) -> Option<bool> {
    r.and_then(|r| r.teams.goalkeeper_of(team != user))
}

/// «No goalkeeper» as the office Desafíos do it (fichajes-ie3.md 5.2m): every member position + 1 (bench included, so
/// nobody collides; the keeper is the member at position 0, `GetTeamKeeper 0xEB66E0`) and one more on the pitch.
/// None when the team is not plain (not exactly one member at position 0, a position >= 0xFE, or already 11 on the
/// pitch: the match AI treats positions >= 11 as off the pitch).
pub fn outfield_shift(positions: &[u8], on_pitch: u32) -> Option<(Vec<u8>, u32)> {
    if on_pitch == 0
        || on_pitch >= MAX_PLAYERS as u32
        || positions.iter().filter(|&&p| p == 0).count() != 1
        || positions.iter().any(|&p| p >= 0xFE)
    {
        return None;
    }
    Some((positions.iter().map(|p| p + 1).collect(), on_pitch + 1))
}

/// `test_ruleset` only applies to offline full / small matches outside the story V-goal rows.
pub fn test_ruleset_eligible(ty: u8, bits: u32) -> bool {
    matches!(ty, 1 | 2) && bits & BIT_EXTENDED_VGOAL == 0
}

// ---------------------------------------------------------------- patterns

/// Bytes of an IDA-style pattern (`"48 8B ?? 05"`, None per wildcard); None = malformed.
pub fn pattern_bytes(p: &str) -> Option<Vec<Option<u8>>> {
    p.split_whitespace()
        .map(|t| if t == "??" || t == "?" { Some(None) } else { u8::from_str_radix(t, 16).ok().map(Some) })
        .collect()
}

/// The first `n` bytes of a pattern when none of them is a wildcard (the stolen bytes of an inline hook).
pub fn fixed_prefix(p: &str, n: usize) -> Option<Vec<u8>> {
    let b = pattern_bytes(p)?;
    if b.len() < n {
        return None;
    }
    b[..n].iter().copied().collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use ruleset::Ruleset;

    #[test]
    fn config_defaults_and_toml() {
        let c = MatchEngineCfg::default();
        assert!(!c.probe_active() && c.match_rules && c.legacy_folder && c.setup);
        assert_eq!(c.test_ruleset(), None);
        assert!(c.problems().is_empty());
        assert!(c.probe_summary().starts_with("off"));
        // the merged plugin config is flat (the legacy [match_engine] section merged by the loader)
        let c: MatchEngineCfg =
            toml::from_str("probe_players = [4, 0, 9]\nprobe_formation = [0xB37F899F]\ntest_ruleset = \" test_et_pens \"\nmatch_rules = false\n").unwrap();
        assert_eq!((c.probe_count(0), c.probe_count(1)), (Some(4), None));
        assert_eq!((c.probe_formation(0), c.probe_formation(1)), (Some(0xB37F899F), None));
        assert_eq!(c.test_ruleset(), Some("test_et_pens"));
        assert!(!c.match_rules && c.setup);
        assert_eq!(c.problems().len(), 1);
        let c = MatchEngineCfg { probe_players: vec![12, 3], ..Default::default() };
        assert_eq!((c.probe_count(0), c.probe_count(1)), (None, Some(3)));
        assert_eq!(c.problems().len(), 1);
    }

    #[test]
    fn team_line_marks_on_pitch() {
        let mk = |slot, position, flags| Member { slot, position, chara: 0x100 + slot as u32, flags };
        let members = [mk(0, 0, 0), mk(1, 1, 0), mk(2, 2, 0), mk(3, 3, 0), mk(4, 11, 0x08), mk(5, 0xFF, 0x800)];
        let l = team_line(1, 11, 3, 0x5505CEAF, true, &members);
        assert!(l.starts_with("probe: team 1 on-pitch 11 -> 3 (3 members on the pitch)"), "{l}");
        assert!(l.contains("formation 0x5505CEAF (forced)"), "{l}");
        assert!(l.contains("0:0:00000100* 1:1:00000101* 2:2:00000102* 3:3:00000103 11:4:00000104]"), "{l}");
        assert!(!l.contains("00000105"), "{l}");
    }

    #[test]
    fn setup_plan_writes_only_the_knobs() {
        let row = RowRules { period: 1800, bits: BIT_TWO_HALVES | 1 << 2, exrule: 0 };
        // an empty ruleset (base rulesets that keep the row) changes nothing
        let (n, notes) = plan_setup(&Ruleset::default(), row);
        assert_eq!((n, notes.len()), (row, 0));
        // retail values stated explicitly: "= row", same fields
        let free = Ruleset::parse("[time]\nhalf_minutes = 30\nhalves = 2\n", "free_match").unwrap();
        let (n, notes) = plan_setup(&free, row);
        assert_eq!(n, row);
        assert_eq!(notes, vec!["period 1800s = row", "halves 2 = row"]);
        // real changes
        let r = Ruleset::parse("[time]\nhalf_minutes = 10\nhalves = 1\n[play]\nexrule = [4]\n[end]\nextended_vgoal = true\n", "x").unwrap();
        let (n, notes) = plan_setup(&r, row);
        assert_eq!(n, RowRules { period: 600, bits: 1 << 2 | BIT_EXTENDED_VGOAL, exrule: 1 << 4 });
        assert_eq!(notes, vec!["period 1800s -> 600s", "halves 2 -> 1", "story V-goal off -> on", "ExRule 0x0 -> 0x10"]);
        let z = Ruleset::parse("[time]\nhalves = 0\nhalf_minutes = 0\n[end]\ngolden_goal = true\n", "z").unwrap();
        let (n, notes) = plan_setup(&z, row);
        assert_eq!((n.period, n.bits & BIT_TWO_HALVES), (0, 0));
        assert_eq!(notes, vec!["period 1800s -> 0s", "halves 2 -> 0 (skipped)"]);
    }

    #[test]
    fn team_counts_probe_then_ruleset() {
        let r = Ruleset::parse("[teams]\nplayers = 5\nrival = { players = 3, formation = 7 }\n", "x").unwrap();
        let cfg = MatchEngineCfg::default();
        // user side 1: team 1 = user (5), team 0 = rival (3)
        assert_eq!((team_count(&cfg, Some(&r), 1, 1), team_count(&cfg, Some(&r), 0, 1)), (Some(5), Some(3)));
        assert_eq!((team_formation(&cfg, Some(&r), 0, 1), team_formation(&cfg, Some(&r), 1, 1)), (Some(7), None));
        assert_eq!(team_count(&cfg, None, 0, 0), None);
        let probe = MatchEngineCfg { probe_players: vec![2, 0], probe_formation: vec![0, 9], ..Default::default() };
        assert_eq!((team_count(&probe, Some(&r), 0, 0), team_count(&probe, Some(&r), 1, 0)), (Some(2), Some(3)));
        assert_eq!(team_formation(&probe, Some(&r), 1, 0), Some(9));
        assert!(test_ruleset_eligible(1, 0) && test_ruleset_eligible(2, 2) && !test_ruleset_eligible(3, 0));
        let k = Ruleset::parse("[teams]\nrival = { goalkeeper = false }\n", "k").unwrap();
        assert_eq!((team_goalkeeper(Some(&k), 0, 1), team_goalkeeper(Some(&k), 1, 1), team_goalkeeper(None, 0, 0)), (Some(false), None, None));
        // the office shift: 0-4 -> 1-5 (bench too), 5 -> 6 on the pitch
        assert_eq!(outfield_shift(&[0, 1, 2, 3, 4, 7], 5), Some((vec![1, 2, 3, 4, 5, 8], 6)));
        assert_eq!(outfield_shift(&[1, 2, 3], 3), None, "no keeper to move");
        assert_eq!(outfield_shift(&[0, 1, 2], 11), None, "already 11");
        assert_eq!(outfield_shift(&[0, 0xFE], 3), None);
        assert!(!test_ruleset_eligible(1, BIT_EXTENDED_VGOAL));
    }

    #[test]
    fn patterns() {
        assert_eq!(fixed_prefix("48 8B ?? 05", 2), Some(vec![0x48, 0x8B]));
        assert_eq!(fixed_prefix("48 8B ?? 05", 3), None);
        assert_eq!(fixed_prefix("48", 2), None);
        assert!(pattern_bytes("48 ZZ").is_none());
        for (s, n) in sigs::HOOKS {
            assert_eq!(fixed_prefix(s.pattern, *n).map(|v| v.len()), Some(*n), "{}", s.name);
        }
    }

    /// The shipped rulesets and assignment table of mods/match_engine parse, every assignment names a
    /// loaded ruleset (or "retail") and the base rulesets reproduce the row (no knob changes a retail value).
    #[test]
    fn staged_mod_folder_is_valid() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../mods/match_engine").join(RULES_DIR);
        if !dir.is_dir() {
            eprintln!("mods/match_engine/rules missing: skipped");
            return;
        }
        let (rs, msgs) = ruleset::load_sources(&[("match_engine".into(), dir.clone())]);
        assert!(msgs.is_empty(), "{msgs:?}");
        let mut t = assign::Table::new("match_engine");
        let text = std::fs::read_to_string(dir.join(MATCHES_FILE)).expect("matches.toml");
        let p = t.add("match_engine", &text);
        assert!(p.is_empty(), "{p:?}");
        for (e, what) in t.referenced() {
            assert!(e.ruleset == "retail" || rs.iter().any(|r| r.id == e.ruleset), "{what} -> {} not loaded", e.ruleset);
        }
        for m in assign::MODES {
            assert!(t.base_modes.contains_key(*m), "base mode {m} not mapped");
        }
        for id in ["test_et_pens", "test_pens", "test_golden", "free_match", "pachanga_5v5"] {
            assert!(rs.iter().any(|r| r.id == id), "{id} missing");
        }
        // retail rows: full matches 30' in two halves (c20 = 30 in all 2018 full rows, VS CPU 6-9 too; c22 = 0 -> bit 1),
        // small matches one period of 15' (c20 = 15 and c22 = 1 in all 204 small rows -> bit 1 clear)
        let full = RowRules { period: 1800, bits: BIT_TWO_HALVES, exrule: 0 };
        let small = RowRules { period: 900, bits: 0, exrule: 0 };
        for r in rs.iter().filter(|r| t.base_modes.values().any(|e| e.ruleset == r.id)) {
            let row = if r.id == "pachanga_5v5" || r.id.ends_with("_small") { small } else { full };
            let (n, _) = plan_setup(r, row);
            assert_eq!(n, row, "base ruleset {} changes a retail value", r.id);
            assert!(r.end.is_retail(), "base ruleset {} has end rules", r.id);
        }
        let pach = rs.iter().find(|r| r.id == "pachanga_5v5").unwrap();
        assert_eq!((pach.teams.players_of(false), pach.teams.players_of(true)), (Some(5), Some(5)));
        let tp = rs.iter().find(|r| r.id == "test_et_pens").unwrap();
        assert!(tp.end.extra_time && tp.end.penalties && !tp.end.golden_goal);
        for r in &rs {
            assert!(r.warnings.is_empty(), "{}: {:?}", r.id, r.warnings);
            let regular = r.time.halves.unwrap_or(2);
            assert!(end_logic::Flow::build(regular, &r.end).is_ok(), "{} has no playable flow", r.id);
        }
    }
}
