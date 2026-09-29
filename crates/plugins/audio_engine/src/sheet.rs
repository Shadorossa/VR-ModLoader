//! `sound_queue_sheet` merge (docs/game/media/audio-engine.md §5): the sheets (banks) the game loads at boot, by group.
//!
//! `common/sound/sound_queue_sheet.cfg.bin` (RDBN): `m_queueSheetInfoList` rows `(queueSheetId = crc32(name),
//! queueSheetString = name, isVLG)` and `m_queueSheetGroupList` rows `(groupId, sheetList = (start, count) slice of the
//! info list)`. Retail v7.1.2: 38 rows in 4 groups — global `[0,12]` (bgm … bevent_stream), voice `[12,20]`
//! (event_stream_voice, c_common, partvoice_* …, `isVLG` 1 = `sound_asset/<ja|en>/`), battle `[32,5]` (common, ch,
//! battle, effect, event_stream_voice) and `[37,1]` (bevent_stream). A bank that is not listed is only loaded on demand
//! by name (character voices, events); a NEW bank whose cues are played by id from anywhere (menu SE, BGM) must be
//! listed, else its cues are never found.
//!
//! Before the audio engine every mod that added a bank shipped a whole modified sheet, and the last one won. Now a mod
//! declares its banks (`[[bank]]` of its `audio.toml`); [`Sheet::add`] appends each one at the end of its group and
//! shifts the slices of the later groups, exactly like `research/scripts/bgm_add_build.py::queue_sheet_add`. Whole
//! sheets that older mods still ship are read too: their extra rows ([`Sheet::extra_rows`]) are merged the same way.
//!
//! The merged file is served through the mods overlay at a FIXED size: the audio engine mod ships a placeholder of
//! [`SLOT_SIZE`] bytes (the base sheet + zero padding), and the plugin rewrites it in place at the early phase (exe entry
//! point, before any game code) with the merge padded to the same size ([`Sheet::to_bytes_padded`]). The padding is
//! an unreferenced tail of the RDBN string pool (counted in the header's `dataSize`), the same shape as the retail
//! `soccer_common_text.cfg.bin` (docs/formats/rdbn.md §9); the overlay's cpk_list record keeps the size it read at
//! DllMain, which stays right.

use l5_core::rdbn::{Rdbn, Value};

/// Game path of the sheet (overlay key form).
pub const GAME_PATH: &str = "data/common/sound/sound_queue_sheet.cfg.bin";
/// Size of the served placeholder (retail = 1 705 B, one bank row ≈ 20-30 B: room for ~1 000 banks).
pub const SLOT_SIZE: usize = 32 * 1024;
pub const LIST_INFO: &str = "m_queueSheetInfoList";
pub const LIST_GROUP: &str = "m_queueSheetGroupList";

/// A group of the sheet, by alias or id.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Group {
    /// The group holding `bgm` (retail groupId 3832920144): banks loaded at boot for the whole session.
    Global,
    /// The group holding `c_common` (isVLG banks: `sound_asset/<ja|en>/`).
    Voice,
    /// The group holding `battle` (match banks).
    Battle,
    /// A group by its `groupId`.
    Id(u32),
}

impl Group {
    /// `global` / `voice` / `battle` / a groupId (decimal or `0x…`).
    pub fn parse(s: &str) -> Result<Group, String> {
        let t = s.trim().to_ascii_lowercase();
        Ok(match t.as_str() {
            "" | "global" => Group::Global,
            "voice" | "voz" => Group::Voice,
            "battle" | "match" | "partido" => Group::Battle,
            _ => {
                let v = match t.strip_prefix("0x") {
                    Some(h) => u32::from_str_radix(h, 16),
                    None => t.parse::<u32>(),
                };
                Group::Id(v.map_err(|_| format!("group {s:?}: use global, voice, battle or a groupId number"))?)
            }
        })
    }

    /// The retail sheet that identifies an alias group.
    fn member(&self) -> Option<&'static str> {
        match self {
            Group::Global => Some("bgm"),
            Group::Voice => Some("c_common"),
            Group::Battle => Some("battle"),
            Group::Id(_) => None,
        }
    }

    pub fn label(&self) -> String {
        match self {
            Group::Global => "global".into(),
            Group::Voice => "voice".into(),
            Group::Battle => "battle".into(),
            Group::Id(v) => v.to_string(),
        }
    }
}

/// One bank to register.
#[derive(Debug, Clone, PartialEq)]
pub struct BankReq {
    pub name: String,
    pub group: Group,
    /// `isVLG` (None = 1 in the voice group, else 0).
    pub voice: Option<bool>,
    /// Mod that asked (for the log).
    pub source: String,
}

/// What [`Sheet::add`] did.
#[derive(Debug, Clone, PartialEq)]
pub enum Added {
    /// New row at `index` of the info list, inside group `group_id`.
    Added { index: usize, group_id: u32 },
    /// The group already lists the bank (retail, the base file or an earlier mod).
    Present { group_id: u32 },
}

/// Valid bank (sheet) name: the ACB file stem, `[A-Za-z0-9_]`, 1..=64.
pub fn valid_bank_name(s: &str) -> bool {
    !s.is_empty() && s.len() <= 64 && s.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
}

/// A parsed `sound_queue_sheet`.
#[derive(Debug, Clone)]
pub struct Sheet {
    doc: Rdbn,
    info: usize,
    group: usize,
    /// Field indices: info (id, name, vlg), group (id, slice).
    fi: (usize, usize, usize),
    fg: (usize, usize),
}

fn field(doc: &Rdbn, list: usize, name: &str) -> Result<usize, String> {
    let t = doc.list_type(&doc.lists[list]).ok_or("list without type")?;
    t.fields.iter().position(|f| f.name == name).ok_or_else(|| format!("field {name} not found"))
}

fn one(v: &[Value]) -> Option<&Value> {
    v.first()
}

impl Sheet {
    pub fn parse(bytes: &[u8]) -> Result<Sheet, String> {
        let doc = Rdbn::parse(bytes).map_err(|e| format!("RDBN: {e}"))?;
        let find = |n: &str| doc.lists.iter().position(|l| l.name == n).ok_or_else(|| format!("list {n} not found"));
        let (info, group) = (find(LIST_INFO)?, find(LIST_GROUP)?);
        let fi = (field(&doc, info, "queueSheetId")?, field(&doc, info, "queueSheetString")?, field(&doc, info, "isVLG")?);
        let fg = (field(&doc, group, "groupId")?, field(&doc, group, "sheetList")?);
        let s = Sheet { doc, info, group, fi, fg };
        s.check()?;
        Ok(s)
    }

    /// Every group slice inside the info list.
    fn check(&self) -> Result<(), String> {
        let n = self.doc.lists[self.info].rows.len();
        for (id, start, count) in self.groups() {
            if start < 0 || count < 0 || start as usize + count as usize > n {
                return Err(format!("group {id}: slice [{start},{count}] outside the {n} rows"));
            }
        }
        Ok(())
    }

    /// `(groupId, start, count)` of every group, in file order.
    pub fn groups(&self) -> Vec<(u32, i16, i16)> {
        self.doc.lists[self.group]
            .rows
            .iter()
            .map(|r| {
                let id = match r.get(self.fg.0).and_then(|v| one(v)) {
                    Some(Value::Hash(h)) => *h,
                    Some(Value::Int(i)) => *i as u32,
                    _ => 0,
                };
                let (s, c) = match r.get(self.fg.1).and_then(|v| one(v)) {
                    Some(Value::Tuple([s, c])) => (*s, *c),
                    _ => (-1, -1),
                };
                (id, s, c)
            })
            .collect()
    }

    /// Info rows: (name, isVLG).
    pub fn rows(&self) -> Vec<(String, u8)> {
        self.doc.lists[self.info]
            .rows
            .iter()
            .map(|r| {
                let name = match r.get(self.fi.1).and_then(|v| one(v)) {
                    Some(Value::String(Some(s))) => s.clone(),
                    _ => String::new(),
                };
                let vlg = match r.get(self.fi.2).and_then(|v| one(v)) {
                    Some(Value::Byte(b)) => *b,
                    Some(Value::Bool(b)) => *b as u8,
                    _ => 0,
                };
                (name, vlg)
            })
            .collect()
    }

    /// Names listed in group index `gi`.
    pub fn names_in(&self, gi: usize) -> Vec<(String, u8)> {
        let Some(&(_, s, c)) = self.groups().get(gi) else { return Vec::new() };
        self.rows().into_iter().skip(s as usize).take(c as usize).collect()
    }

    /// Index of a group (alias: the group whose slice lists its member sheet).
    pub fn find_group(&self, g: &Group) -> Option<usize> {
        let groups = self.groups();
        match g {
            Group::Id(id) => groups.iter().position(|x| x.0 == *id),
            _ => {
                let m = g.member()?;
                (0..groups.len()).find(|&gi| self.names_in(gi).iter().any(|(n, _)| n == m))
            }
        }
    }

    /// Register `req` at the end of its group (later groups' slices shifted). Idempotent per (group, name).
    pub fn add(&mut self, req: &BankReq) -> Result<Added, String> {
        if !valid_bank_name(&req.name) {
            return Err(format!("bank name {:?}: A-Z a-z 0-9 _ only (1-64)", req.name));
        }
        let gi = self.find_group(&req.group).ok_or_else(|| format!("group {} not in the sheet", req.group.label()))?;
        let groups = self.groups();
        let (gid, start, count) = groups[gi];
        if self.names_in(gi).iter().any(|(n, _)| n.eq_ignore_ascii_case(&req.name)) {
            return Ok(Added::Present { group_id: gid });
        }
        if count == i16::MAX {
            return Err("group full".into());
        }
        let vlg = req.voice.unwrap_or(req.group == Group::Voice || self.find_group(&Group::Voice) == Some(gi)) as u8;
        let tmpl = self.doc.lists[self.info].rows.get(start as usize).or_else(|| self.doc.lists[self.info].rows.first()).cloned();
        let mut row = tmpl.ok_or("empty info list")?;
        row[self.fi.0] = vec![Value::Hash(crc32fast::hash(req.name.as_bytes()))];
        row[self.fi.1] = vec![Value::String(Some(req.name.clone()))];
        row[self.fi.2] = vec![match row[self.fi.2].first() {
            Some(Value::Bool(_)) => Value::Bool(vlg != 0),
            _ => Value::Byte(vlg),
        }];
        let ins = (start + count) as usize;
        self.doc.lists[self.info].rows.insert(ins, row);
        let fg1 = self.fg.1;
        for (k, r) in self.doc.lists[self.group].rows.iter_mut().enumerate() {
            if let Some(Value::Tuple([s, c])) = r.get_mut(fg1).and_then(|v| v.first_mut()) {
                if k == gi {
                    *c += 1;
                } else if (*s as usize) >= ins {
                    *s += 1;
                }
            }
        }
        self.check()?;
        Ok(Added::Added { index: ins, group_id: gid })
    }

    /// Rows `other` lists in a group (same groupId) that this sheet's group does not: what an older mod's whole
    /// sheet added (group id, name, isVLG).
    pub fn extra_rows(&self, other: &Sheet) -> Vec<(u32, String, u8)> {
        let mine = self.groups();
        let mut out = Vec::new();
        for (ogi, (gid, _, _)) in other.groups().into_iter().enumerate() {
            let have: Vec<String> = mine
                .iter()
                .position(|g| g.0 == gid)
                .map(|gi| self.names_in(gi).into_iter().map(|(n, _)| n.to_ascii_lowercase()).collect())
                .unwrap_or_default();
            for (n, v) in other.names_in(ogi) {
                if !n.is_empty() && !have.contains(&n.to_ascii_lowercase()) && !out.iter().any(|(g, x, _)| *g == gid && x == &n) {
                    out.push((gid, n, v));
                }
            }
        }
        out
    }

    /// The file bytes (byte-exact writer of l5-core, without padding).
    pub fn to_bytes(&self) -> Result<Vec<u8>, String> {
        let mut d = self.doc.clone();
        d.trailing_strings.clear();
        d.to_bytes().map_err(|e| format!("RDBN write: {e}"))
    }

    /// The file padded to exactly `size` bytes (zero tail of the string pool). Err when the merge does not fit.
    pub fn to_bytes_padded(&self, size: usize) -> Result<Vec<u8>, String> {
        let bare = self.to_bytes()?;
        if bare.len() > size {
            return Err(format!("merged sheet is {} bytes, the slot holds {size}", bare.len()));
        }
        let mut d = self.doc.clone();
        d.trailing_strings = vec![0u8; size - bare.len()];
        let b = d.to_bytes().map_err(|e| format!("RDBN write: {e}"))?;
        if b.len() != size {
            return Err(format!("padding gave {} bytes instead of {size}", b.len()));
        }
        Ok(b)
    }
}

/// Outcome of a whole merge, for the log.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct MergeReport {
    /// (source mod, bank, group label, index) of every new row.
    pub added: Vec<(String, String, String, usize)>,
    /// (source mod, bank) already listed.
    pub present: Vec<(String, String)>,
    /// (source mod, message) of refused requests.
    pub errors: Vec<(String, String)>,
}

/// Merge `reqs` (load order) into `base`.
pub fn merge(base: &[u8], reqs: &[BankReq]) -> Result<(Sheet, MergeReport), String> {
    let mut s = Sheet::parse(base)?;
    let mut rep = MergeReport::default();
    for r in reqs {
        match s.add(r) {
            Ok(Added::Added { index, .. }) => rep.added.push((r.source.clone(), r.name.clone(), r.group.label(), index)),
            Ok(Added::Present { .. }) => rep.present.push((r.source.clone(), r.name.clone())),
            Err(e) => rep.errors.push((r.source.clone(), format!("{}: {e}", r.name))),
        }
    }
    Ok((s, rep))
}

#[cfg(test)]
mod tests {
    use super::*;

    const DUMP: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../../assets/v7.1.2/data/common/sound/sound_queue_sheet.cfg.bin");

    fn retail() -> Option<Vec<u8>> {
        std::fs::read(DUMP).ok()
    }

    fn req(name: &str, g: Group, src: &str) -> BankReq {
        BankReq { name: name.into(), group: g, voice: None, source: src.into() }
    }

    #[test]
    fn groups_parse() {
        assert_eq!(Group::parse("global").unwrap(), Group::Global);
        assert_eq!(Group::parse("Voice").unwrap(), Group::Voice);
        assert_eq!(Group::parse("battle").unwrap(), Group::Battle);
        assert_eq!(Group::parse("3832920144").unwrap(), Group::Id(3832920144));
        assert_eq!(Group::parse("0xE4755A50").unwrap(), Group::Id(0xE475_5A50));
        assert!(Group::parse("nope").is_err());
        assert!(valid_bank_name("evt_fwa_se") && !valid_bank_name("a b") && !valid_bank_name(""));
    }

    #[test]
    fn retail_layout_and_bgm_add_equivalent() {
        let Some(b) = retail() else { return };
        let s = Sheet::parse(&b).unwrap();
        assert_eq!(s.to_bytes().unwrap(), b, "byte-exact round trip");
        assert_eq!(s.groups(), vec![(3832920144, 0, 12), (1137430653, 12, 20), (3001745064, 32, 5), (3837758303, 37, 1)]);
        assert_eq!(s.find_group(&Group::Global), Some(0));
        assert_eq!(s.find_group(&Group::Voice), Some(1));
        assert_eq!(s.find_group(&Group::Battle), Some(2));
        // the custom-bgm.md §4 change: evt_bgm at index 12, groups [0,13] [13,20] [33,5] [38,1]
        let (m, rep) = merge(&b, &[req("evt_bgm", Group::Global, "m")]).unwrap();
        assert_eq!(rep.added, vec![("m".into(), "evt_bgm".into(), "global".into(), 12)]);
        assert_eq!(m.groups(), vec![(3832920144, 0, 13), (1137430653, 13, 20), (3001745064, 33, 5), (3837758303, 38, 1)]);
        assert_eq!(m.rows()[12], ("evt_bgm".into(), 0));
        let bytes = m.to_bytes().unwrap();
        assert_eq!(bytes.len(), 1725, "custom-bgm.md: 1 705 -> 1 725 B");
        // idempotent, and the file parses back to the same rows
        let again = Sheet::parse(&bytes).unwrap();
        assert_eq!(again.rows(), m.rows());
        let (m2, rep2) = merge(&bytes, &[req("evt_bgm", Group::Global, "m2")]).unwrap();
        assert_eq!(rep2.present, vec![("m2".into(), "evt_bgm".into())]);
        assert_eq!(m2.to_bytes().unwrap(), bytes);
    }

    #[test]
    fn voice_and_battle_groups_and_errors() {
        let Some(b) = retail() else { return };
        let reqs = [
            req("evt_voice_x", Group::Voice, "a"),
            req("evt_match_se", Group::Battle, "b"),
            req("bad name", Group::Global, "c"),
            req("x", Group::Id(42), "d"),
            req("ch", Group::Battle, "e"),
        ];
        let (m, rep) = merge(&b, &reqs).unwrap();
        assert_eq!(rep.added.len(), 2);
        assert_eq!(rep.present, vec![("e".into(), "ch".into())]);
        assert_eq!(rep.errors.len(), 2);
        let g = m.groups();
        assert_eq!(g, vec![(3832920144, 0, 12), (1137430653, 12, 21), (3001745064, 33, 6), (3837758303, 39, 1)]);
        assert_eq!(m.rows()[32], ("evt_voice_x".into(), 1), "voice group: isVLG 1");
        assert_eq!(m.rows()[38], ("evt_match_se".into(), 0));
    }

    #[test]
    fn padded_slot_round_trips() {
        let Some(b) = retail() else { return };
        let (m, _) = merge(&b, &[req("evt_fwa_se", Group::Global, "m")]).unwrap();
        let p = m.to_bytes_padded(SLOT_SIZE).unwrap();
        assert_eq!(p.len(), SLOT_SIZE);
        // header dataSize = file length - 0x50
        assert_eq!(u32::from_le_bytes(p[0x0C..0x10].try_into().unwrap()) as usize, SLOT_SIZE - 0x50);
        let back = Sheet::parse(&p).unwrap();
        assert_eq!(back.rows(), m.rows());
        assert_eq!(back.groups(), m.groups());
        // the padded file is also a valid base (its tail is dropped on the next merge)
        let (m2, rep) = merge(&p, &[req("evt_fwa_se", Group::Global, "m")]).unwrap();
        assert_eq!(rep.present.len(), 1);
        assert_eq!(m2.to_bytes_padded(SLOT_SIZE).unwrap(), p);
        assert!(m.to_bytes_padded(100).is_err());
    }

    #[test]
    fn staged_placeholder_parses() {
        // research/scripts/audio_engine_stage.py pads the retail file with zeros and patches dataSize
        let Some(b) = retail() else { return };
        let mut p = b.clone();
        p.resize(SLOT_SIZE, 0);
        p[0x0C..0x10].copy_from_slice(&((SLOT_SIZE - 0x50) as u32).to_le_bytes());
        let s = Sheet::parse(&p).unwrap();
        assert_eq!(s.rows(), Sheet::parse(&b).unwrap().rows());
        assert_eq!(s.to_bytes().unwrap(), b, "the tail is dropped on write");
        // with nothing to add the plugin's padded output is exactly the staged placeholder
        assert_eq!(s.to_bytes_padded(SLOT_SIZE).unwrap(), p);
    }

    #[test]
    fn legacy_sheets_contribute_their_extra_rows() {
        let Some(b) = retail() else { return };
        let base = Sheet::parse(&b).unwrap();
        let (legacy, _) = merge(&b, &[req("evt_bgm", Group::Global, "old"), req("evt_vc", Group::Voice, "old")]).unwrap();
        let extra = base.extra_rows(&legacy);
        assert_eq!(extra, vec![(3832920144, "evt_bgm".to_string(), 0), (1137430653, "evt_vc".to_string(), 1)]);
        assert!(legacy.extra_rows(&legacy).is_empty());
    }
}
