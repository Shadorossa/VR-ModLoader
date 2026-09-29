//! The aura commands of `aura_skill_config` (the loose file the game loads when a mod adds armours, then the player's
//! own retail file): parser plus the lookups `armed_voice` needs. No table of the game's armours is built in: every
//! row comes from the player's own data at run time.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// One `AURA_CMD_INFO` row (the fields `armed_voice` uses).
#[derive(Debug, Clone, PartialEq)]
pub struct AuraRow {
    pub id: u32,
    pub key: String,
    /// col 7: aura character (shared by a keshin and its armed forms).
    pub aura_chara: u32,
    /// col 10: auraType (0 keshin, 1 armed, …).
    pub ty: u8,
    /// `AURA_CMD_CHARA` rows of the command (chara_base owners).
    pub owners: Vec<u32>,
}

/// Parse the rows of a T2B `aura_skill_config`.
pub fn parse(bytes: &[u8]) -> Result<Vec<AuraRow>, String> {
    let doc = l5_core::T2b::parse(bytes).map_err(|e| format!("T2B parse: {e}"))?;
    let named = |e: &l5_core::t2b::Entry, n: &str| match &e.name {
        Some(x) => x == n,
        None => e.hash == crc32fast::hash(n.as_bytes()),
    };
    let int = |e: &l5_core::t2b::Entry, i: usize| e.values.get(i).and_then(|v| v.as_int()).map(|v| v as u32);
    let charas: Vec<u32> = doc.entries.iter().filter(|e| named(e, "AURA_CMD_CHARA")).filter_map(|e| int(e, 0)).collect();
    let refs: Vec<(u32, u32)> = doc
        .entries
        .iter()
        .filter(|e| named(e, "AURA_CMD_INFO_REF_CHARA"))
        .map(|e| (int(e, 0).unwrap_or(0), int(e, 1).unwrap_or(0)))
        .collect();
    let mut out = Vec::new();
    for (i, e) in doc.entries.iter().filter(|e| named(e, "AURA_CMD_INFO")).enumerate() {
        let (s, n) = refs.get(i).copied().unwrap_or((0, 0));
        let owners = charas.get(s as usize..(s + n) as usize).map(|v| v.to_vec()).unwrap_or_default();
        out.push(AuraRow {
            id: int(e, 0).unwrap_or(0),
            key: e.values.get(1).and_then(|v| v.as_str()).unwrap_or("").to_string(),
            aura_chara: int(e, 7).unwrap_or(0),
            ty: int(e, 10).unwrap_or(255) as u8,
            owners,
        });
    }
    Ok(out)
}

/// The newest loose `aura_skill_config_<ver>.cfg.bin` of `<game>\data\common\gamedata\skill\`.
pub fn loose_path(game_dir: &Path) -> Option<PathBuf> {
    let dir = game_dir.join("data").join("common").join("gamedata").join("skill");
    std::fs::read_dir(&dir)
        .ok()?
        .filter_map(|e| e.ok())
        .filter_map(|e| e.file_name().into_string().ok())
        .filter(|n| {
            let l = n.to_ascii_lowercase();
            l.starts_with("aura_skill_config_") && l.ends_with(".cfg.bin")
        })
        .max()
        .map(|n| dir.join(n))
}

/// The loose rows (armours added by mods) + the retail rows: what `armed_voice` asks.
#[derive(Debug, Default, Clone)]
pub struct Auras {
    rows: Vec<AuraRow>,
    by_id: HashMap<u32, usize>,
}

impl Auras {
    pub fn new(rows: Vec<AuraRow>) -> Auras {
        let by_id = rows.iter().enumerate().map(|(i, r)| (r.id, i)).collect();
        Auras { rows, by_id }
    }

    /// Load the loose file of `game_dir` (empty table when there is none or it does not parse).
    pub fn load(game_dir: &Path) -> Auras {
        Auras::new(loose_path(game_dir).and_then(|p| std::fs::read(p).ok()).and_then(|b| parse(&b).ok()).unwrap_or_default())
    }

    /// `loose` rows first, then every `retail` row whose id the loose file does not have.
    pub fn with_fallback(loose: Vec<AuraRow>, retail: Vec<AuraRow>) -> Auras {
        let mut rows = loose;
        for r in retail {
            if !rows.iter().any(|x| x.id == r.id) {
                rows.push(r);
            }
        }
        Auras::new(rows)
    }

    pub fn len(&self) -> usize {
        self.rows.len()
    }

    pub fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }

    /// Row `i` in table order.
    pub fn row_at(&self, i: usize) -> Option<&AuraRow> {
        self.rows.get(i)
    }

    fn row(&self, id: u32) -> Option<&AuraRow> {
        self.by_id.get(&id).map(|&i| &self.rows[i])
    }

    fn loose_keshin_of(&self, id: u32) -> Option<String> {
        let a = self.row(id)?;
        self.rows.iter().find(|r| r.ty == 0 && r.aura_chara != 0 && r.aura_chara == a.aura_chara).map(|r| r.key.clone())
    }

    /// Key and keshin key of an armed id. `None` when `id` is not a known armed form.
    pub fn armed_keys(&self, id: u32) -> Option<(String, String)> {
        let r = self.row(id)?;
        if r.ty != 1 {
            return None;
        }
        Some((r.key.clone(), self.loose_keshin_of(id).unwrap_or_else(|| "?".into())))
    }

    /// Is `bank_crc` (crc32 of the voice bank name = the chara_base id of a character's own bank) an owner of `id`?
    pub fn is_owner(&self, id: u32, bank_crc: u32) -> bool {
        self.row(id).is_some_and(|r| r.owners.contains(&bank_crc))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_the_dump_when_present() {
        let Ok(dump) = std::env::var("EVT_DUMP") else { return };
        let p = std::path::Path::new(&dump).join("data/common/gamedata/skill/aura_skill_config_1.04.09.00.cfg.bin");
        let Ok(b) = std::fs::read(p) else { return };
        let rows = parse(&b).unwrap();
        assert_eq!(rows.len(), 443);
        let a = rows.iter().find(|r| r.key == "was00020").unwrap();
        assert_eq!((a.id, a.ty), (0xDD08BEB4, 1));
        assert_eq!(a.owners, vec![crc32fast::hash(b"c04000100")]);
        let t = Auras::new(rows);
        assert_eq!(t.armed_keys(0xDD08BEB4).unwrap().0, "was00020");
        assert!(t.armed_keys(crc32fast::hash(b"wks00020")).is_none());
    }

    #[test]
    fn loose_rows_win_over_the_retail_rows() {
        let was = crc32fast::hash(b"was00630");
        let custom = crc32fast::hash(b"was09999");
        let loose = vec![
            AuraRow { id: custom, key: "was09999".into(), aura_chara: 77, ty: 1, owners: vec![5] },
            AuraRow { id: 9, key: "wks09999".into(), aura_chara: 77, ty: 0, owners: vec![] },
            AuraRow { id: 10, key: "wks00001".into(), aura_chara: 0, ty: 0, owners: vec![] },
            AuraRow { id: 11, key: "was00011".into(), aura_chara: 0, ty: 1, owners: vec![6] },
        ];
        let retail = vec![
            AuraRow { id: was, key: "was00630".into(), aura_chara: 3, ty: 1, owners: vec![crc32fast::hash(b"c05020700")] },
            AuraRow { id: 12, key: "wks00630".into(), aura_chara: 3, ty: 0, owners: vec![] },
            AuraRow { id: 11, key: "retail_row".into(), aura_chara: 0, ty: 1, owners: vec![7] },
        ];
        let t = Auras::with_fallback(loose, retail);
        assert_eq!(t.armed_keys(custom), Some(("was09999".into(), "wks09999".into())));
        assert!(t.is_owner(custom, 5));
        assert!(t.armed_keys(10).is_none(), "a keshin row is not an armour");
        // not in the loose rows: the retail row
        assert_eq!(t.armed_keys(was), Some(("was00630".into(), "wks00630".into())));
        assert!(t.is_owner(was, crc32fast::hash(b"c05020700")));
        // same id in both: the loose row wins
        assert_eq!(t.armed_keys(11).unwrap().0, "was00011");
        assert!(t.is_owner(11, 6) && !t.is_owner(11, 7));
    }
}
