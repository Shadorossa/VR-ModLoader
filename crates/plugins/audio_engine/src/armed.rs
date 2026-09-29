//! `armed_voice` of the audio engine: the armour shout per armour (docs/game/media/voice-packs.md «Grito de armadura
//! por armadura»). Port of the ModLoader's built-in module `armed_voice` (`crates/vr-loader/src/armed_voice`), same
//! behaviour and the same log lines; the built-in module yields when this plugin loads (the mod `provides`
//! `armed_voice`).
//!
//! Retail plays `c<chara>_armed` for every armour: the 6 armed cut-in events `ev81_000N0` are shared by all armed forms
//! and their voice command has the fixed suffix `armed`. `PlayCharaVoice(mgr, &handle, bank, suffix, params)`
//! `0x16FC7E0` formats `"%s_%s"`, takes crc32 and asks every loaded cue sheet; an unknown cue returns handle 0 with no
//! side effect. The plugin hooks it: for the suffix `armed` it finds the actor that is arming and tries
//! `<bank>_<armour id>` (exact armed key `was00630_b1`, then its base `was00630`) before the retail `armed`.
//!
//! Pure parts (cue names, actor choice, log lines) are here and unit-tested; the run time is `rt`.

use crate::aura::Auras;

/// The retail suffix of the armour shout.
pub const ARMED_SUFFIX: &str = "armed";
/// `PlayCharaVoice` formats the cue into a 0x40-byte buffer (`snprintf`, NUL included).
pub const CUE_BUF: usize = 0x40;
/// An exec seen within this window counts as "this is the armour being put on now".
pub const EXEC_RECENT_MS: u64 = 10_000;

/// Cue name the engine builds from bank and suffix.
pub fn cue_name(bank: &str, suffix: &str) -> String {
    format!("{bank}_{suffix}")
}

/// Does `<bank>_<suffix>` fit the engine's cue buffer (else snprintf would cut it and the crc would be wrong)?
pub fn fits(bank: &str, suffix: &str) -> bool {
    bank.len() + 1 + suffix.len() < CUE_BUF
}

/// Base armour id of an armed key: `was00630_b1` → `was00630`; a key without a variant tail is its own base.
pub fn base_id(key: &str) -> &str {
    match key.find('_') {
        Some(i) if i > 0 => &key[..i],
        _ => key,
    }
}

/// Suffixes to try, in order, for the armed form `key`: the exact key, then its base id. Only plain `[A-Za-z0-9_]`
/// keys, and only those that fit the cue buffer.
pub fn suffixes_for(bank: &str, key: &str) -> Vec<String> {
    let mut v: Vec<String> = Vec::new();
    for s in [key, base_id(key)] {
        let ok = !s.is_empty() && s.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_');
        if ok && s != ARMED_SUFFIX && fits(bank, s) && !v.iter().any(|x| x == s) {
            v.push(s.to_string());
        }
    }
    v
}

/// A soccer actor whose metamorphosis is (or is starting as) an armed form.
#[derive(Debug, Clone, PartialEq)]
pub struct Cand {
    /// Actor index (`hdl & 0xFF`).
    pub idx: usize,
    /// Armed skill id (crc32 of its key).
    pub skill: u32,
    /// Metamorphosis state `actor+0x3A0` (1..3 starting, 4 / 5 active, 0 idle).
    pub state: u8,
    /// Time left / full time of the metamorphosis (`+0x39C` / `+0x388`).
    pub left: f32,
    pub max: f32,
    /// Milliseconds since the executor ran this armed command for this actor (the plugin's own exec hook), if known.
    pub exec_age_ms: Option<u64>,
    /// crc32(bank) is an owner (`AURA_CMD_CHARA`) of the armed form: the actor is its native wearer.
    pub owner_match: bool,
    /// The armour is not on yet but the actor is putting it on: 2 = his Hiper button is firing it, 1 = it sits in one
    /// of his rows while its keshin is summoned, 0 = no ([`pending_armour`]).
    pub pending: u8,
}

/// The armed form an actor is about to put on, before the engine executes it (the cut-in voice plays ~1.5 s before
/// the exec): the Hiper armour being fired (`hiper`, level 2), else, with keshin `keshin` summoned (crc, 0 = none), an
/// armed form of that keshin in one of the unit's `rows` (level 1). `keys` = [`Auras::armed_keys`].
pub fn pending_armour(hiper: u32, keshin: u32, rows: &[u32], keys: impl Fn(u32) -> Option<(String, String)>) -> Option<(u32, u8)> {
    if hiper != 0 && keys(hiper).is_some() {
        return Some((hiper, 2));
    }
    if keshin == 0 {
        return None;
    }
    rows.iter()
        .copied()
        .find(|&r| r != 0 && keys(r).is_some_and(|(_, k)| k == "?" || crc32fast::hash(k.as_bytes()) == keshin))
        .map(|r| (r, 1))
}

fn rank(c: &Cand) -> (bool, u8, u64, bool, u8, u8, u32) {
    let recent = c.exec_age_ms.filter(|&a| a <= EXEC_RECENT_MS);
    let starting = matches!(c.state, 1..=3) as u8;
    let active = matches!(c.state, 4 | 5) as u8;
    // fraction of the time still left: an armour that just started is (near) full
    let fresh = if c.max > 0.0 { ((c.left / c.max).clamp(0.0, 1.0) * 1000.0) as u32 } else { 0 };
    (
        c.pending >= 2, // a Hiper button firing an armour that is not on yet: the shout is his
        recent.is_some() as u8,
        u64::MAX - recent.unwrap_or(u64::MAX), // newer exec first
        c.pending == 1,
        c.owner_match as u8,
        starting * 2 + active,
        fresh,
    )
}

/// The actor that is arming: Hiper firing an armour not on yet, else the most recent armed exec, else an armour in
/// his rows while its keshin is out, else the native wearer, else the one still starting, else the freshest timer.
pub fn pick(cands: &[Cand]) -> Option<&Cand> {
    cands
        .iter()
        .filter(|c| c.state != 0 || c.pending != 0 || c.exec_age_ms.is_some_and(|a| a <= EXEC_RECENT_MS))
        .max_by_key(|c| rank(c))
}

/// Result of one `armed` request (for the log line).
#[derive(Debug, Clone, PartialEq)]
pub enum Outcome {
    /// `<bank>_<suffix>` played.
    Found(String),
    /// Nothing better: `<bank>_armed` played (reason).
    Fallback(&'static str),
}

/// The log line of one decision (identical to the built-in module's).
pub fn log_line(bank: &str, hdl: Option<u32>, keshin: &str, armour: &str, tried: &[String], out: &Outcome) -> String {
    let who = match hdl {
        Some(h) => format!("chara {bank} (actor 0x{h:X})"),
        None => format!("chara {bank}"),
    };
    match out {
        Outcome::Found(s) => format!("armed_voice: {who} keshin {keshin} armour {armour} -> cue {} (found)", cue_name(bank, s)),
        Outcome::Fallback(why) => {
            let t = if tried.is_empty() {
                String::new()
            } else {
                format!("; tried {}", tried.iter().map(|s| cue_name(bank, s)).collect::<Vec<_>>().join(", "))
            };
            format!("armed_voice: {who} keshin {keshin} armour {armour} -> cue {} (fallback: {why}{t})", cue_name(bank, ARMED_SUFFIX))
        }
    }
}

/// What the detour does for one `armed` request (everything but the engine calls).
#[derive(Debug, Clone, PartialEq)]
pub struct Plan {
    pub bank: String,
    pub hdl: Option<u32>,
    pub keshin: String,
    pub armour: String,
    pub suffixes: Vec<String>,
    pub why: &'static str,
}

/// The plan for bank `bank` given the candidate actors (`(cand, actor handle)`).
pub fn plan(bank: &str, cands: &[(Cand, u32)], auras: &Auras) -> Plan {
    let mut p = Plan {
        bank: bank.to_string(),
        hdl: None,
        keshin: "?".into(),
        armour: "?".into(),
        suffixes: Vec::new(),
        why: "no arming actor",
    };
    let plain: Vec<Cand> = cands.iter().map(|(c, _)| c.clone()).collect();
    let Some((c, hdl)) = pick(&plain).and_then(|best| cands.iter().find(|(c, _)| c.idx == best.idx).cloned()) else {
        return p;
    };
    p.hdl = Some(hdl);
    let Some((armour, keshin)) = auras.armed_keys(c.skill) else {
        p.why = "armour id unknown";
        return p;
    };
    p.suffixes = suffixes_for(bank, &armour);
    p.armour = armour;
    p.keshin = keshin;
    p.why = if p.suffixes.is_empty() { "armour id does not fit a cue name" } else { "cue not in any loaded bank" };
    p
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cand(idx: usize, state: u8, left: f32, exec: Option<u64>, owner: bool) -> Cand {
        Cand { idx, skill: 0xC09EF347, state, left, max: 60.0, exec_age_ms: exec, owner_match: owner, pending: 0 }
    }

    /// A small table shaped like the game's rows (armour was00630 + its variant _b1 of keshin wks00630, owner
    /// c05020700).
    fn builtin() -> Auras {
        use crate::aura::AuraRow;
        let h = |s: &str| crc32fast::hash(s.as_bytes());
        Auras::new(vec![
            AuraRow { id: h("wks00630"), key: "wks00630".into(), aura_chara: 630, ty: 0, owners: vec![] },
            AuraRow { id: h("was00630"), key: "was00630".into(), aura_chara: 630, ty: 1, owners: vec![h("c05020700")] },
            AuraRow { id: h("was00630_b1"), key: "was00630_b1".into(), aura_chara: 630, ty: 1, owners: vec![h("c05020700")] },
        ])
    }

    #[test]
    fn pending_armour_before_the_exec() {
        // loader.log 29/09: Beta c11905050 arms Athena from a Hiper (was00630 lent into row 1, keshin wks00630 out)
        let t = builtin();
        let keys = |id| t.armed_keys(id);
        let (was, wks) = (crc32fast::hash(b"was00630"), crc32fast::hash(b"wks00630"));
        let rows = [0x1111_1111, was, 0, 0, 0, 0];
        assert_eq!(pending_armour(was, wks, &rows, keys), Some((was, 2)));
        assert_eq!(pending_armour(0, wks, &rows, keys), Some((was, 1)));
        assert_eq!(pending_armour(wks, 0, &rows, keys), None);
        assert_eq!(pending_armour(0, 0, &rows, keys), None);
        assert_eq!(pending_armour(0, crc32fast::hash(b"wks00020"), &rows, keys), None);
        assert_eq!(pending_armour(0, wks, &[wks, 0, 0, 0, 0, 0], keys), None);

        let mut arming = cand(10, 0, 0.0, None, false);
        arming.pending = 2;
        assert!(pick(&[cand(3, 0, 0.0, None, true)]).is_none());
        assert_eq!(pick(&[cand(3, 4, 30.0, None, true), arming.clone()]).unwrap().idx, 10);
        assert_eq!(pick(&[cand(3, 4, 60.0, Some(500), false), arming.clone()]).unwrap().idx, 10);
        arming.pending = 1;
        assert_eq!(pick(&[cand(3, 4, 60.0, Some(500), false), arming.clone()]).unwrap().idx, 3);
        assert_eq!(pick(&[cand(3, 4, 60.0, None, true), arming.clone()]).unwrap().idx, 10);
    }

    #[test]
    fn beta_athena_cue() {
        let t = builtin();
        let id = crc32fast::hash(b"was00630");
        assert_eq!(id, 0xC09EF347);
        let (armour, keshin) = t.armed_keys(id).unwrap();
        assert_eq!((armour.as_str(), keshin.as_str()), ("was00630", "wks00630"));
        assert!(t.is_owner(id, crc32fast::hash(b"c05020700")));
        assert!(!t.is_owner(id, crc32fast::hash(b"c01000010")));
        assert_eq!(suffixes_for("c05020700", &armour), vec!["was00630"]);
        let (v, _) = t.armed_keys(crc32fast::hash(b"was00630_b1")).unwrap();
        assert_eq!(suffixes_for("c05020700", &v), vec!["was00630_b1", "was00630"]);
    }

    #[test]
    fn keshin_and_unknown_ids_are_not_armours() {
        let t = builtin();
        assert!(t.armed_keys(crc32fast::hash(b"wks00630")).is_none());
        assert!(t.armed_keys(0x1234_5678).is_none());
    }

    #[test]
    fn names_and_limits() {
        assert_eq!(base_id("wad00650_h2"), "wad00650");
        assert_eq!(base_id("wak00110"), "wak00110");
        assert_eq!(base_id("_x"), "_x");
        assert!(fits("c05020700", "was00630_h1"));
        let long = "x".repeat(60);
        assert!(!fits("c05020700", &long));
        assert!(suffixes_for("c05020700", &long).is_empty());
        assert!(suffixes_for("c05020700", "bad key").is_empty());
        assert!(suffixes_for("c05020700", "armed").is_empty());
        assert!(suffixes_for("scoutMAK01", "wao00010").contains(&"wao00010".to_string()));
    }

    #[test]
    fn pick_prefers_recent_exec_then_owner_then_starting() {
        assert!(pick(&[]).is_none());
        assert!(pick(&[cand(1, 0, 0.0, None, true)]).is_none());
        assert_eq!(pick(&[cand(1, 4, 20.0, None, true), cand(2, 4, 60.0, Some(300), false)]).unwrap().idx, 2);
        assert_eq!(pick(&[cand(1, 2, 60.0, Some(900), false), cand(2, 2, 60.0, Some(100), false)]).unwrap().idx, 2);
        assert_eq!(pick(&[cand(1, 2, 60.0, None, false), cand(2, 4, 10.0, None, true)]).unwrap().idx, 2);
        assert_eq!(pick(&[cand(1, 4, 60.0, None, false), cand(2, 1, 0.0, None, false)]).unwrap().idx, 2);
        assert_eq!(pick(&[cand(1, 4, 10.0, None, false), cand(2, 4, 55.0, None, false)]).unwrap().idx, 2);
        let v = [cand(1, 4, 60.0, Some(EXEC_RECENT_MS + 1), false), cand(2, 4, 60.0, None, true)];
        assert_eq!(pick(&v).unwrap().idx, 2);
    }

    #[test]
    fn log_lines_match_the_builtin_module() {
        let tried = vec!["was00630".to_string()];
        assert_eq!(
            log_line("c05020700", Some(0x103), "wks00630", "was00630", &tried, &Outcome::Found("was00630".into())),
            "armed_voice: chara c05020700 (actor 0x103) keshin wks00630 armour was00630 -> cue c05020700_was00630 (found)"
        );
        assert_eq!(
            log_line("c05020700", None, "?", "?", &[], &Outcome::Fallback("no arming actor")),
            "armed_voice: chara c05020700 keshin ? armour ? -> cue c05020700_armed (fallback: no arming actor)"
        );
        assert_eq!(
            log_line("c05020700", Some(3), "wks00630", "was00631", &["was00631".into()], &Outcome::Fallback("cue not in the bank")),
            "armed_voice: chara c05020700 (actor 0x3) keshin wks00630 armour was00631 -> cue c05020700_armed \
             (fallback: cue not in the bank; tried c05020700_was00631)"
        );
    }

    #[test]
    fn plan_for_the_29_09_log_case() {
        // Beta c11905050 (actor 0x8000000A) fires his Hiper armour was00630: the full cue first
        let t = builtin();
        let was = crc32fast::hash(b"was00630");
        let mut c = cand(10, 0, 0.0, None, false);
        c.skill = was;
        c.pending = 2;
        let p = plan("c11905050", &[(c, 0x8000_000A)], &t);
        assert_eq!(p.hdl, Some(0x8000_000A));
        assert_eq!((p.armour.as_str(), p.keshin.as_str()), ("was00630", "wks00630"));
        assert_eq!(p.suffixes, vec!["was00630"]);
        assert_eq!(p.why, "cue not in any loaded bank");
        // nobody arming
        let p = plan("c11905050", &[], &t);
        assert_eq!((p.hdl, p.why, p.suffixes.len()), (None, "no arming actor", 0));
        // an unknown skill
        let mut c = cand(4, 4, 30.0, None, false);
        c.skill = 0x1234_5678;
        assert_eq!(plan("c01000010", &[(c, 4)], &t).why, "armour id unknown");
    }
}
