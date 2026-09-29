//! The name index the audio engine ships (`index\audio_index.json`, research/scripts/audio_index_build.py, v2):
//! characters → voice banks, techniques (→ shout suffix, → `ev60_#####_me` SE track), armours, the retail banks per
//! language, `se_cues` (every SE cue → its root bank) and `bgm` (bgm_config ids → cue / bank, contexts → ids).

use serde::Deserialize;
use std::collections::{BTreeSet, HashMap};

#[derive(Debug, Clone, Default, Deserialize)]
pub struct Character {
    pub id: String,
    #[serde(default)]
    pub es: Option<String>,
    #[serde(default)]
    pub en: Option<String>,
    pub bank: String,
    #[serde(default)]
    pub voice: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct Technique {
    pub key: String,
    #[serde(default)]
    pub es: Option<String>,
    #[serde(default)]
    pub en: Option<String>,
    /// Its SE track (`ev60_#####_me`), when waza_stream has one.
    #[serde(default)]
    pub se: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct Armour {
    pub key: String,
    #[serde(default)]
    pub es: Option<String>,
    #[serde(default)]
    pub en: Option<String>,
    #[serde(default)]
    pub keshin_es: Option<String>,
    #[serde(default)]
    pub keshin_en: Option<String>,
    #[serde(default)]
    pub owners: Vec<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct BgmId {
    pub id: String,
    pub crc: u32,
    #[serde(default)]
    pub cue: Option<String>,
    #[serde(default)]
    pub bank: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct Bgm {
    #[serde(default)]
    pub ids: Vec<BgmId>,
    #[serde(default)]
    pub contexts: HashMap<String, Vec<String>>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct Index {
    #[serde(default)]
    pub version: u32,
    #[serde(default)]
    pub characters: Vec<Character>,
    #[serde(default)]
    pub techniques: Vec<Technique>,
    #[serde(default)]
    pub armours: Vec<Armour>,
    #[serde(default)]
    pub banks: HashMap<String, BTreeSet<String>>,
    #[serde(default)]
    pub se_cues: HashMap<String, String>,
    #[serde(default)]
    pub bgm: Bgm,
}

/// Lower case, single spaces, accents folded (`Relámpago` = `relampago`).
fn norm(s: &str) -> String {
    let fold = |c: char| match c {
        'á' | 'à' | 'â' | 'ä' | 'ã' => 'a',
        'é' | 'è' | 'ê' | 'ë' => 'e',
        'í' | 'ì' | 'î' | 'ï' => 'i',
        'ó' | 'ò' | 'ô' | 'ö' | 'õ' => 'o',
        'ú' | 'ù' | 'û' | 'ü' => 'u',
        'ñ' => 'n',
        'ç' => 'c',
        c => c,
    };
    s.split_whitespace().collect::<Vec<_>>().join(" ").to_lowercase().chars().map(fold).collect()
}

fn is_key(s: &str, prefix: &str, digits: usize) -> bool {
    s.len() >= prefix.len() + 1 + digits && s.starts_with(prefix) && s[prefix.len() + 1..prefix.len() + 1 + digits].bytes().all(|b| b.is_ascii_digit())
}

impl Index {
    pub fn parse(text: &str) -> Result<Index, String> {
        serde_json::from_str(text).map_err(|e| format!("audio_index.json: {e}"))
    }

    pub fn load(path: &std::path::Path) -> Result<Index, String> {
        Index::parse(&std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?)
    }

    fn named<'a>(n: &str, names: impl IntoIterator<Item = &'a Option<String>>) -> bool {
        names.into_iter().any(|x| x.as_deref().is_some_and(|x| norm(x) == n))
    }

    /// Own voice banks of every character called `name` (es / en) and their character ids.
    pub fn character_banks(&self, name: &str) -> Result<(Vec<String>, Vec<String>), String> {
        let n = norm(name);
        let rows: Vec<&Character> = self.characters.iter().filter(|c| Self::named(&n, [&c.es, &c.en])).collect();
        if rows.is_empty() {
            return Err(format!("character {name:?} is not in the index (use the game's es / en name, or bank = \"c########\")"));
        }
        let own: BTreeSet<String> = rows.iter().filter(|c| c.voice.as_deref() == Some("own")).map(|c| c.bank.clone()).collect();
        let banks = if own.is_empty() { rows.iter().map(|c| c.bank.clone()).collect::<BTreeSet<_>>() } else { own };
        Ok((banks.into_iter().collect(), rows.iter().map(|c| c.id.clone()).collect::<BTreeSet<_>>().into_iter().collect()))
    }

    pub fn characters_of_bank(&self, bank: &str) -> Vec<String> {
        self.characters.iter().filter(|c| c.bank == bank).map(|c| c.id.clone()).collect::<BTreeSet<_>>().into_iter().collect()
    }

    fn technique_rows(&self, name: &str) -> Result<Vec<&Technique>, String> {
        let n = norm(name);
        let rows: Vec<&Technique> = self.techniques.iter().filter(|t| t.key == n || Self::named(&n, [&t.es, &t.en])).collect();
        if rows.is_empty() {
            return Err(format!("technique {name:?} is not in the index"));
        }
        Ok(rows)
    }

    /// Shout suffixes (`wh?#####`) of a technique name / key.
    pub fn technique_keys(&self, name: &str) -> Result<Vec<String>, String> {
        if is_key(&norm(name), "wh", 5) && norm(name).len() == 8 {
            return Ok(vec![norm(name)]);
        }
        Ok(self.technique_rows(name)?.into_iter().map(|t| t.key.clone()).collect::<BTreeSet<_>>().into_iter().collect())
    }

    /// The SE track cue of a technique (`ev60_#####_me`).
    pub fn technique_se(&self, name: &str) -> Result<String, String> {
        self.technique_rows(name)?.into_iter().find_map(|t| t.se.clone()).ok_or_else(|| format!("technique {name:?} has no ev60_#####_me SE track"))
    }

    /// Base armour ids named `name`; with `charas`, the ones they wear natively when there are any.
    pub fn armour_keys(&self, name: &str, charas: &[String]) -> Result<Vec<String>, String> {
        let n = norm(name);
        if n.len() >= 8 && n.starts_with("wa") && n[3..8].bytes().all(|b| b.is_ascii_digit()) {
            return Ok(vec![n[..8].to_string()]);
        }
        let rows: Vec<&Armour> = self.armours.iter().filter(|a| Self::named(&n, [&a.es, &a.en, &a.keshin_es, &a.keshin_en])).collect();
        if rows.is_empty() {
            return Err(format!("armour {name:?} is not in the index"));
        }
        let own: Vec<&&Armour> = rows.iter().filter(|a| a.owners.iter().any(|o| charas.contains(o))).collect();
        let pick: Vec<&Armour> = if own.is_empty() { rows } else { own.into_iter().copied().collect() };
        Ok(pick.into_iter().map(|a| a.key.split('_').next().unwrap_or(&a.key).to_string()).collect::<BTreeSet<_>>().into_iter().collect())
    }

    pub fn has_bank(&self, lang: &str, bank: &str) -> bool {
        self.banks.get(lang).is_some_and(|s| s.contains(bank))
    }

    /// Root bank of an SE cue.
    pub fn se_bank(&self, cue: &str) -> Option<&str> {
        self.se_cues.get(cue).map(|s| s.as_str())
    }

    /// bgm_config ids of a `[[music]] replace` target: an id (`bg00010`), a crc (`0x0151319`), or a context
    /// (`title`, `map:<map name | 0xHASH>`, `match:<set | 0xHASH>`). Returns (id label, crc).
    pub fn bgm_targets(&self, t: &str) -> Result<Vec<(String, u32)>, String> {
        let s = t.trim();
        let by_label = |x: &str| -> Option<(String, u32)> {
            if let Some(h) = x.strip_prefix("0x").and_then(|h| u32::from_str_radix(h, 16).ok()) {
                return Some((x.to_string(), h));
            }
            self.bgm.ids.iter().find(|i| i.id.eq_ignore_ascii_case(x)).map(|i| (i.id.clone(), i.crc))
        };
        if let Some((kind, what)) = s.split_once(':') {
            let kind = kind.to_ascii_lowercase();
            let key = match what.strip_prefix("0x") {
                Some(h) => format!("{kind}:0x{}", h.to_ascii_uppercase()),
                None => format!("{kind}:0x{:08X}", crc32fast::hash(what.as_bytes())),
            };
            let ids = self.bgm.contexts.get(&key).ok_or_else(|| format!("context {s:?} not in the index"))?;
            return ids.iter().map(|x| by_label(x).ok_or_else(|| format!("{s}: id {x} unknown"))).collect();
        }
        if let Some(ids) = self.bgm.contexts.get(&s.to_ascii_lowercase()) {
            return ids.iter().map(|x| by_label(x).ok_or_else(|| format!("{s}: id {x} unknown"))).collect();
        }
        by_label(s).map(|v| vec![v]).ok_or_else(|| format!("BGM {s:?}: not a bgm_config id (bg#####) or context (title, map:…, match:…)"))
    }
}

#[cfg(test)]
pub(crate) fn sample() -> Index {
    Index::parse(
        r#"{"version":2,
        "characters":[{"id":"c05020700","es":"Beta","en":"Beta","bank":"c05020700","voice":"own"},
                      {"id":"c11905050","es":"Beta","en":"Beta","bank":"c11905050","voice":"own"},
                      {"id":"c01000010","es":"Mark Evans","en":"Mark Evans","bank":"c01000010","voice":"own"}],
        "techniques":[{"key":"whs03380","es":"Ruptura relámpago CG","en":"Inazuma Break CG","se":"ev60_03380_me"},
                      {"key":"whk00010","es":"Mano celestial","en":"God Hand"}],
        "armours":[{"key":"was00630","es":"Atenea","owners":["c05020700"]},{"key":"was00631","es":"Atenea","owners":["c11905050"]}],
        "banks":{"ja":["c05020700","c11905050","c01000010"],"en":[],"root":["waza_stream","common","bgm_title"]},
        "se_cues":{"ev60_03380_me":"waza_stream","sy0006":"common"},
        "bgm":{"ids":[{"id":"bg00010","crc":22097913,"cue":"bg00010","bank":"bgm_title"},{"id":"bg20040","crc":1,"cue":"bg20040","bank":"bgm"}],
               "contexts":{"title":["bg00010"],"map:0x355BCB1A":["bg20040"]}}}"#,
    )
    .unwrap()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lookups() {
        let i = sample();
        assert_eq!(i.character_banks("beta").unwrap().0, ["c05020700", "c11905050"]);
        assert_eq!(i.technique_keys("God Hand").unwrap(), ["whk00010"]);
        assert_eq!(i.technique_se("Ruptura relámpago CG").unwrap(), "ev60_03380_me");
        assert_eq!(i.technique_se("ruptura  RELAMPAGO cg").unwrap(), "ev60_03380_me", "accents / case / spaces");
        assert!(i.technique_se("Mano celestial").is_err());
        assert_eq!(i.armour_keys("Atenea", &["c11905050".into()]).unwrap(), ["was00631"]);
        assert_eq!(i.se_bank("ev60_03380_me"), Some("waza_stream"));
        assert_eq!(i.bgm_targets("bg00010").unwrap(), [("bg00010".to_string(), 22097913)]);
        assert_eq!(i.bgm_targets("title").unwrap()[0].1, 22097913);
        assert_eq!(i.bgm_targets("map:0x355bcb1a").unwrap()[0].0, "bg20040");
        assert!(i.bgm_targets("map:nowhere").is_err());
        assert!(i.bgm_targets("bg77777").is_err());
    }

}
