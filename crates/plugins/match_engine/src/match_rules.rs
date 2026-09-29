//! `match_rules` (part of the match engine plugin since ModLoader phase 3): match rules chosen in Opciones
//! (docs/game/modes/prorroga-penaltis.md). Same behaviour, file and Lua commands as the old built-in loader module.
//!
//! Slice 1: **half length** ("Duración de cada parte", 30' / 45'). The value lives in `evt_loader\match_rules.json`
//! (never in the save) and is applied to every match by the match_rules Lua patches through
//! `CMND_EVT_MATCH_RULES_APPLY`.
//!
//! Engine facts (v7.1.2, static, `docs/game/modes/prorroga-penaltis.md` §2):
//! * the soccer manager is `[[g_gameRoot]+0x6A58]`; its **period length in seconds** is the `u16` at `+0x1C`.
//!   Written by the Lua command `CMND_SET_SOCCER_GAME_CONFIG 0x6854F711` sub 2 (`SetSoccerConfig_PeriodTime(min)`,
//!   handler `0xC33BD0`: `imul edx, eax, 60; mov word [mgr+0x1C], dx`), read by the clock ratio (`0x1688840`,
//!   `0x16888C0`), the half-time state (`0x1430D29`), the period-end checks (`0x16831AA`, `0x1433369`) and about
//!   25 more sites. Retail full matches: `SOCCER_GAME_DIFFICULTY` c20 = 30 → **1800 s per half** (c20 = 15 for
//!   small matches, 60 for 3 test rows). Story scripts extend it mid-match (`PeriodTime(35..45)`) and put 30 back at
//!   half time (`fbtl_cro09_020_010`, `fbtl_st_0501`), so the field is **not** re-initialised between halves.
//! * the value is only rewritten when it holds the retail 1800 and the Lua has just armed the module (match start,
//!   half time), so story scripts that set their own period keep it, and small matches (900 s) are never touched.
//!
//! | Command | Args | Returns |
//! |---|---|---|
//! | `CMND_EVT_MATCH_RULES_GET` | – | `half_minutes` (30 retail), `min`, `max` |
//! | `CMND_EVT_MATCH_RULES_SET` | `half_minutes` | `ok`: saved to `match_rules.json` (refused outside `MIN_HALF..=MAX_HALF`) |
//! | `CMND_EVT_MATCH_RULES_APPLY` | `mode` 1 match start, 2 half time (both arm), 0 tick | `-1` no soccer manager, `0` nothing, `1` written (period = setting), `2` restored (period back to 1800) |

use serde::{Deserialize, Serialize};
use std::path::Path;

/// Retail period length of a full match (c20 = 30 minutes).
pub const RETAIL_HALF_SECS: u16 = 1800;
/// Setting range accepted by `SET` (the menu cycles 30 / 45).
pub const MIN_HALF: u16 = 5;
pub const MAX_HALF: u16 = 90;
/// Soccer manager pointer in the game root and the period field in it.
pub const OFF_SOCCER_MANAGER: usize = 0x6A58;
pub const OFF_PERIOD_SECS: usize = 0x1C;
/// Ticks (`APPLY(0)` calls) an arm stays valid: the soccer_menu ticks every 30 frames (~2/s), so about 20 s.
pub const ARM_TTL: u32 = 40;

/// `evt_loader\match_rules.json`. Written atomically (tmp + rename).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct State {
    /// Game minutes of each half (retail 30).
    pub half_minutes: u16,
    /// Informational: when it was last changed.
    pub changed: String,
}

impl Default for State {
    fn default() -> Self {
        State { half_minutes: 30, changed: String::new() }
    }
}

impl State {
    pub fn load(path: &Path) -> State {
        let mut s: State = std::fs::read_to_string(path)
            .ok()
            .and_then(|t| serde_json::from_str(&t).ok())
            .unwrap_or_default();
        if valid_half(s.half_minutes as i64).is_none() {
            s.half_minutes = 30;
        }
        s
    }

    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, serde_json::to_string_pretty(self).unwrap())?;
        std::fs::rename(&tmp, path)
    }
}

/// A half length accepted by `SET` (`MIN_HALF..=MAX_HALF` minutes).
pub fn valid_half(minutes: i64) -> Option<u16> {
    (minutes >= MIN_HALF as i64 && minutes <= MAX_HALF as i64).then_some(minutes as u16)
}

/// What `APPLY` does with the period field.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    /// Leave the field alone.
    Nothing,
    /// Write this many seconds (the setting) over the retail 1800.
    Write(u16),
    /// The setting is back to 30' and the field still holds what we wrote: put the retail value back.
    Restore,
}

/// Pure rule of `APPLY`: `half_minutes` = the setting, `cur` = the period field now, `armed` = a match start /
/// half time was signalled and not consumed, `last_written` = the last value this module wrote (0 = none).
pub fn decide(half_minutes: u16, cur: u16, armed: bool, last_written: u16) -> Action {
    let want = half_minutes.saturating_mul(60);
    if want == RETAIL_HALF_SECS {
        // setting back to retail: undo our own write only (a story script's own period is never touched)
        return if last_written != 0 && cur == last_written { Action::Restore } else { Action::Nothing };
    }
    if armed && cur == RETAIL_HALF_SECS {
        Action::Write(want)
    } else {
        Action::Nothing
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn state_roundtrip_and_defaults() {
        let dir = std::env::temp_dir().join(format!("evt-plugin-match-rules-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("match_rules.json");
        assert_eq!(State::load(&p).half_minutes, 30); // missing
        std::fs::write(&p, "{ not json").unwrap();
        assert_eq!(State::load(&p).half_minutes, 30); // corrupt
        std::fs::write(&p, r#"{"half_minutes": 200}"#).unwrap();
        assert_eq!(State::load(&p).half_minutes, 30); // out of range
        State { half_minutes: 45, changed: "test".into() }.save(&p).unwrap();
        assert_eq!(State::load(&p), State { half_minutes: 45, changed: "test".into() });
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn valid_range() {
        assert_eq!(valid_half(30), Some(30));
        assert_eq!(valid_half(45), Some(45));
        assert_eq!(valid_half(90), Some(90));
        assert_eq!(valid_half(4), None);
        assert_eq!(valid_half(91), None);
        assert_eq!(valid_half(-1), None);
        assert!(MAX_HALF as u32 * 60 <= u16::MAX as u32, "period field is a u16 of seconds");
    }

    #[test]
    fn decide_writes_only_armed_retail_periods() {
        // 45' chosen: a retail 1800 is rewritten once the Lua armed the module (match start / half time)
        assert_eq!(decide(45, 1800, true, 0), Action::Write(2700));
        assert_eq!(decide(45, 1800, false, 0), Action::Nothing);
        // a story script's own period (35', 40'...) and small matches (15' = 900 s) are never touched
        assert_eq!(decide(45, 2400, true, 0), Action::Nothing);
        assert_eq!(decide(45, 900, true, 0), Action::Nothing);
        // already ours
        assert_eq!(decide(45, 2700, true, 2700), Action::Nothing);
        // retail setting: nothing, except undoing our own earlier write
        assert_eq!(decide(30, 1800, true, 0), Action::Nothing);
        assert_eq!(decide(30, 2700, true, 2700), Action::Restore);
        assert_eq!(decide(30, 2700, false, 2700), Action::Restore);
        assert_eq!(decide(30, 2700, true, 0), Action::Nothing); // a retail PeriodTime(45) of fbtl_cro08_100_010
        assert_eq!(decide(30, 2400, true, 2700), Action::Nothing);
    }
}
