//! Which ruleset a match gets (docs/game/modes/match-engine.md §12): the assignment tables `<mod>\rules\matches.toml`
//! of every mod that uses the match engine, merged in load order, and the precedence
//!
//! `Lua SELECT` > `test_ruleset` (config, testing) > `[games]` (match row) > `[teams]` (rival team) > `[modes]` of the
//! mods > `[default]` of a mod (its own matches) > `[modes]` of the base mod (the built-in rulesets that reproduce
//! retail) > nothing (the row's rules, untouched).
//!
//! ```toml
//! [modes]                                  # mode of the match -> ruleset (keys: MODES)
//! free = "free_match"
//! free_small = "pachanga_5v5"
//!
//! [games]                                  # SOCCER_GAME_INFO row: name (crc32) or "0xHASH" -> ruleset
//! fbtl_st_0501 = "story"
//! "0x1234ABCD" = "my_final"
//!
//! [teams]                                  # rival team id (difficulty row c1): "0xHASH" / decimal / name -> ruleset
//! "0x9A3F0C11" = "boss_rules"
//!
//! [default]                                # this mod's own matches (below [modes] of other mods)
//! ruleset = "my_mode"
//! games = ["evt_rl_5v5_01", "evt_rl_11v11_01"]
//! ```
//!
//! A ruleset value `"retail"` = no ruleset: the match keeps every rule of its row (only useful to undo a mapping of an
//! earlier mod).

use crate::ruleset::parse_hash;
use serde::Deserialize;
use std::collections::{BTreeMap, HashMap};

/// Mode keys of `[modes]` ([`classify`]).
pub const MODES: &[&str] = &[
    "free",
    "free_small",
    "story",
    "story_small",
    "chronicle",
    "chronicle_small",
    "kizuna",
    "kizuna_small",
    "victory_road",
    "victory_road_small",
    "training",
];

/// Mode of a match from the engine's own fields: `ty` = `mi+0x16` (1 full, 2 short / small, 3 training, 4 dribble),
/// `play_mode` = `[[g_gameRoot]+0x69C8]+0x2CAC6F` (1 VS / friendlies, 2 and 6 story, 3 Kizuna town, 4 Chronicle,
/// 5 Victory Road; None = unreadable → free). Small matches (type 2) get the suffix `_small`.
pub fn classify(ty: u8, play_mode: Option<u8>) -> String {
    if matches!(ty, 3 | 4) {
        return "training".into();
    }
    let base = match play_mode {
        Some(2) | Some(6) => "story",
        Some(3) => "kizuna",
        Some(4) => "chronicle",
        Some(5) => "victory_road",
        _ => "free",
    };
    if ty == 2 { format!("{base}_small") } else { base.to_string() }
}

/// What the engine knows about a match at setup (all read from the match being built, no Lua needed).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct MatchKeys {
    /// `mi+4`: the SOCCER_GAME_INFO row the match was built from (crc32 of its name).
    pub game: u32,
    /// `mi+8`: the row asked for by `CMND_RESERVE_SOCCER` when the difficulty row redirected it (0 = none).
    pub orig_game: u32,
    /// Team id of the rival side (`TeamBuild` setup `+0`, the difficulty row's c1); None = unknown.
    pub rival_team: Option<u32>,
    /// [`classify`].
    pub mode: String,
}

/// Why a ruleset was picked.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Level {
    Game,
    Team,
    Mode,
    ModDefault,
    BaseMode,
}

impl Level {
    pub fn as_str(self) -> &'static str {
        match self {
            Level::Game => "match row",
            Level::Team => "rival team",
            Level::Mode => "mode",
            Level::ModDefault => "mod default",
            Level::BaseMode => "base mode",
        }
    }
}

/// One assignment: ruleset id + the mod whose table said it.
#[derive(Debug, Clone, PartialEq)]
pub struct Entry {
    pub ruleset: String,
    pub mod_id: String,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct DefaultSec {
    ruleset: String,
    games: Vec<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct MatchesFile {
    modes: BTreeMap<String, String>,
    games: BTreeMap<String, String>,
    teams: BTreeMap<String, String>,
    default: Option<DefaultSec>,
}

/// The merged assignment tables.
#[derive(Debug, Default, Clone)]
pub struct Table {
    /// Id of the mod whose `[modes]` are the built-in fallback (the match engine's own mod).
    pub base_mod: String,
    pub base_modes: HashMap<String, Entry>,
    pub modes: HashMap<String, Entry>,
    pub games: HashMap<u32, Entry>,
    pub teams: HashMap<u32, Entry>,
    pub defaults: HashMap<u32, Entry>,
}

fn put<K: std::hash::Hash + Eq + Clone + std::fmt::Debug>(
    map: &mut HashMap<K, Entry>,
    k: K,
    e: Entry,
    what: &str,
    shown: &str,
    msgs: &mut Vec<String>,
) {
    if let Some(old) = map.get(&k) {
        if old.mod_id != e.mod_id && old.ruleset != e.ruleset {
            msgs.push(format!(
                "{what} {shown}: \"{}\" of mod {} replaced by \"{}\" of mod {} (later in the load order)",
                old.ruleset, old.mod_id, e.ruleset, e.mod_id
            ));
        }
    }
    map.insert(k, e);
}

impl Table {
    pub fn new(base_mod: &str) -> Table {
        Table { base_mod: base_mod.to_string(), ..Default::default() }
    }

    /// Merge one `matches.toml` of `mod_id` (call in load order: a later mod wins). Returns the problems.
    pub fn add(&mut self, mod_id: &str, text: &str) -> Vec<String> {
        let mut msgs = Vec::new();
        let f: MatchesFile = match toml::from_str(text) {
            Ok(f) => f,
            Err(e) => return vec![format!("matches.toml of mod {mod_id} not used: {}", e.to_string().trim())],
        };
        let ent = |r: &str| Entry { ruleset: r.trim().to_string(), mod_id: mod_id.to_string() };
        for (k, r) in &f.modes {
            let k = k.trim().to_ascii_lowercase();
            if !MODES.contains(&k.as_str()) {
                msgs.push(format!("matches.toml of mod {mod_id}: unknown mode \"{k}\" (known: {})", MODES.join(", ")));
                continue;
            }
            if mod_id == self.base_mod {
                put(&mut self.base_modes, k.clone(), ent(r), "mode", &k, &mut msgs);
            } else {
                put(&mut self.modes, k.clone(), ent(r), "mode", &k, &mut msgs);
            }
        }
        for (k, r) in &f.games {
            match parse_hash(k) {
                Some(h) => put(&mut self.games, h, ent(r), "match row", k, &mut msgs),
                None => msgs.push(format!("matches.toml of mod {mod_id}: [games] key \"{k}\" is not a row name / hash")),
            }
        }
        for (k, r) in &f.teams {
            match parse_hash(k) {
                Some(h) => put(&mut self.teams, h, ent(r), "rival team", k, &mut msgs),
                None => msgs.push(format!("matches.toml of mod {mod_id}: [teams] key \"{k}\" is not a team id")),
            }
        }
        if let Some(d) = f.default {
            if d.ruleset.trim().is_empty() {
                if !d.games.is_empty() {
                    msgs.push(format!("matches.toml of mod {mod_id}: [default] has games but no ruleset"));
                }
            } else {
                for g in &d.games {
                    match parse_hash(g) {
                        Some(h) => put(&mut self.defaults, h, ent(&d.ruleset), "mod default for row", g, &mut msgs),
                        None => msgs.push(format!("matches.toml of mod {mod_id}: [default] game \"{g}\" is not a row name / hash")),
                    }
                }
            }
        }
        msgs
    }

    /// Every ruleset id the tables name (to check they exist).
    pub fn referenced(&self) -> Vec<(&Entry, String)> {
        let mut v: Vec<(&Entry, String)> = Vec::new();
        for (k, e) in self.base_modes.iter().chain(self.modes.iter()) {
            v.push((e, format!("mode {k}")));
        }
        for (k, e) in &self.games {
            v.push((e, format!("match row 0x{k:08X}")));
        }
        for (k, e) in &self.teams {
            v.push((e, format!("rival team 0x{k:08X}")));
        }
        for (k, e) in &self.defaults {
            v.push((e, format!("mod default for row 0x{k:08X}")));
        }
        v
    }

    /// Every entry that applies to a match, in precedence order (the caller takes the first whose ruleset is loaded,
    /// so a missing ruleset falls back to the next level, down to the base mode).
    pub fn resolve_all(&self, k: &MatchKeys) -> Vec<(&Entry, Level)> {
        fn game<'a>(m: &'a HashMap<u32, Entry>, k: &MatchKeys) -> Option<&'a Entry> {
            m.get(&k.game).or_else(|| (k.orig_game != 0).then(|| m.get(&k.orig_game)).flatten())
        }
        let mut v = Vec::new();
        if let Some(e) = game(&self.games, k) {
            v.push((e, Level::Game));
        }
        if let Some(e) = k.rival_team.and_then(|t| self.teams.get(&t)) {
            v.push((e, Level::Team));
        }
        if let Some(e) = self.modes.get(&k.mode) {
            v.push((e, Level::Mode));
        }
        if let Some(e) = game(&self.defaults, k) {
            v.push((e, Level::ModDefault));
        }
        if let Some(e) = self.base_modes.get(&k.mode) {
            v.push((e, Level::BaseMode));
        }
        v
    }

    /// The table's choice for a match (None = no entry: the row's rules).
    pub fn resolve(&self, k: &MatchKeys) -> Option<(&Entry, Level)> {
        self.resolve_all(k).into_iter().next()
    }

    pub fn is_empty(&self) -> bool {
        self.base_modes.is_empty() && self.modes.is_empty() && self.games.is_empty() && self.teams.is_empty() && self.defaults.is_empty()
    }

    /// One line for the log.
    pub fn summary(&self) -> String {
        format!(
            "{} base mode(s), {} mode(s) of mods, {} match row(s), {} rival team(s), {} mod-default row(s)",
            self.base_modes.len(),
            self.modes.len(),
            self.games.len(),
            self.teams.len(),
            self.defaults.len()
        )
    }
}

/// Does the mod with `mod.toml` text `manifest` use the match engine (it is `own_id`, or `requires` / `provides`
/// names `match_engine`, with or without a version: `"match_engine>=1.0"`)?
pub fn uses_match_engine(own_id: &str, id: &str, manifest: &str) -> bool {
    vr_framework::discover::uses_engine("match_engine", own_id, id, manifest)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn keys(game: &str, team: Option<u32>, mode: &str) -> MatchKeys {
        MatchKeys { game: crc32fast::hash(game.as_bytes()), orig_game: 0, rival_team: team, mode: mode.into() }
    }

    #[test]
    fn modes_from_engine_fields() {
        assert_eq!(classify(1, Some(1)), "free");
        assert_eq!(classify(2, Some(1)), "free_small");
        assert_eq!(classify(1, None), "free");
        assert_eq!(classify(1, Some(2)), "story");
        assert_eq!(classify(1, Some(6)), "story");
        assert_eq!(classify(2, Some(4)), "chronicle_small");
        assert_eq!(classify(1, Some(3)), "kizuna");
        assert_eq!(classify(1, Some(5)), "victory_road");
        assert_eq!(classify(3, Some(2)), "training");
        assert_eq!(classify(4, Some(1)), "training");
        for m in ["free", "free_small", "story", "chronicle_small", "training"] {
            assert!(MODES.contains(&m));
        }
    }

    #[test]
    fn precedence_game_team_mode_default_base() {
        let mut t = Table::new("match_engine");
        assert!(t.add("match_engine", "[modes]\nfree = \"free_match\"\nfree_small = \"pachanga_5v5\"\nstory = \"story\"\n").is_empty());
        let m = t.add(
            "my_mod",
            "[modes]\nstory = \"hard_story\"\n[games]\nevt_final = \"final\"\n[teams]\n\"0x10\" = \"boss\"\n[default]\nruleset = \"mine\"\ngames = [\"evt_a\", \"evt_final\"]\n",
        );
        assert!(m.is_empty(), "{m:?}");
        let r = |k: MatchKeys| t.resolve(&k).map(|(e, l)| (e.ruleset.clone(), l));
        // match row beats everything
        assert_eq!(r(keys("evt_final", Some(0x10), "story")), Some(("final".into(), Level::Game)));
        // rival team beats the mode
        assert_eq!(r(keys("x", Some(0x10), "story")), Some(("boss".into(), Level::Team)));
        // a mod's mode beats the base mode
        assert_eq!(r(keys("x", None, "story")), Some(("hard_story".into(), Level::Mode)));
        // mod default: its own rows, below modes of mods, above the base
        assert_eq!(r(keys("evt_a", None, "free")), Some(("mine".into(), Level::ModDefault)));
        // base mode
        assert_eq!(r(keys("x", Some(0x99), "free_small")), Some(("pachanga_5v5".into(), Level::BaseMode)));
        // nothing: the row's rules
        assert_eq!(r(keys("x", None, "training")), None);
        // the redirected row (mi+8) is looked up too
        let k = MatchKeys { game: 1, orig_game: crc32fast::hash(b"evt_final"), rival_team: None, mode: "free".into() };
        assert_eq!(t.resolve(&k).map(|(e, _)| e.ruleset.as_str()), Some("final"));
        assert_eq!(t.referenced().len(), 3 + 1 + 1 + 1 + 2);
        // the whole chain, for the fallback when a ruleset is missing
        let all: Vec<Level> = t.resolve_all(&keys("evt_final", Some(0x10), "story")).into_iter().map(|x| x.1).collect();
        assert_eq!(all, vec![Level::Game, Level::Team, Level::Mode, Level::ModDefault, Level::BaseMode]);
    }

    #[test]
    fn later_mod_wins_and_problems_are_reported() {
        let mut t = Table::new("match_engine");
        t.add("a", "[games]\nevt_x = \"one\"\n");
        let m = t.add("b", "[games]\nevt_x = \"two\"\n[modes]\nvolta = \"v\"\n");
        assert_eq!(t.resolve(&keys("evt_x", None, "free")).unwrap().0.ruleset, "two");
        assert_eq!(m.len(), 2, "{m:?}"); // replaced + unknown mode
        assert!(m.iter().any(|x| x.contains("unknown mode \"volta\"")));
        let bad = t.add("c", "[games\n");
        assert_eq!(bad.len(), 1);
        let d = t.add("d", "[default]\ngames = [\"evt_y\"]\n");
        assert_eq!(d.len(), 1, "{d:?}");
    }

    #[test]
    fn mods_that_use_the_engine() {
        assert!(uses_match_engine("match_engine", "match_engine", ""));
        assert!(uses_match_engine("match_engine", "x", "id = \"x\"\nrequires = [\"match_engine>=1.0\"]\n"));
        assert!(uses_match_engine("match_engine", "x", "requires = [\"match_engine\"]\n"));
        assert!(uses_match_engine("match_engine", "x", "provides = [\"match_engine\"]\n"));
        assert!(!uses_match_engine("match_engine", "x", "requires = [\"match_engine_extra\"]\n"));
        assert!(!uses_match_engine("match_engine", "x", "requires = [\"other\"]\n"));
        assert!(!uses_match_engine("match_engine", "x", "not toml ["));
    }
}
