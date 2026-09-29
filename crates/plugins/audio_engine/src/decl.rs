//! `audio.toml` of a mod (docs/game/media/audio-engine.md §3). The plugin reads every section at boot:
//! `[[bank]]` (registration in `sound_queue_sheet`), and `[[voice]]` / `[[sfx]]` / `[[music]]` (built at boot from the
//! mod's plain audio when the mod does not ship the built bank; research/scripts/audio_mod_build.py and the
//! `audio_build` tool build the same at pack time).
//!
//! ```toml
//! [[bank]]
//! name = "evt_fwa_se"     # files/data/common/sound_asset/evt_fwa_se.acb/.awb (ACB file stem)
//! group = "global"        # global (default) | voice | battle | a groupId
//!
//! [[voice]]
//! character = "Mark Evans"          # or bank = "c01000010" / ["c01000010", "c05024610"]
//! cue = "gl010"                     # or technique = "Mano celestial" / armour = "Atenea"
//! file = "audio/gol.wav"
//!
//! [[sfx]]
//! replace = "ev60_03380_me"         # a retail SE cue (bank from the index) — or technique = "…" (its ev60_#####_me)
//! bank = "waza_stream"              # optional
//! file = "audio/ev60_03380_me.hca"  # a .hca is used as it is
//!
//! [[sfx]]
//! add = "mi_se_ok"                  # a NEW SE, in the mod's own bank evt_se_<mod> (registered automatically)
//! category_template = "sy0006"      # retail SE whose category (sequence command) it copies
//! file = "audio/ok.wav"
//!
//! [[music]]
//! replace = "bg00010"               # a bgm_config id, or a context: "title" | "map:<map>" | "match:<set>" (index)
//! file = "audio/titulo.ogg"
//! loop = [12.5, 95.0]               # seconds (end 0 = to the end); no loop key = loops the whole track
//!
//! [[music]]
//! add = "mi_tema"                   # a NEW track playable by id crc32("mi_tema") (CMND_EXEC_CRAFT_OBJ_ID)
//! play_in = ["map:mr01b01"]         # optional: also take over those contexts' ids
//! file = "audio/tema.flac"
//! ```

use crate::sheet::{BankReq, Group};
use serde::Deserialize;
use std::path::Path;

/// File name of the declarations, in the mod folder (next to mod.toml).
pub const FILE: &str = "audio.toml";

/// A string or a list of strings.
#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(untagged)]
pub enum OneOrMany {
    One(String),
    Many(Vec<String>),
}

impl OneOrMany {
    pub fn list(&self) -> Vec<String> {
        match self {
            OneOrMany::One(s) => vec![s.clone()],
            OneOrMany::Many(v) => v.clone(),
        }
    }
}

#[derive(Debug, Clone, Deserialize, PartialEq)]
pub struct BankDecl {
    pub name: String,
    #[serde(default)]
    pub group: String,
    #[serde(default)]
    pub voice: Option<bool>,
    /// Build-time only: the retail SE whose sequence command (SE category) the new cues copy.
    #[serde(default)]
    pub category_template: Option<String>,
}

/// Common source options.
#[derive(Debug, Clone, Default, Deserialize, PartialEq)]
pub struct Src {
    #[serde(default)]
    pub file: String,
    #[serde(default)]
    pub volume: Option<f32>,
    #[serde(default)]
    pub normalize: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize, PartialEq)]
pub struct VoiceDecl {
    #[serde(default)]
    pub character: Option<String>,
    #[serde(default)]
    pub bank: Option<OneOrMany>,
    #[serde(default)]
    pub cue: Option<OneOrMany>,
    #[serde(default)]
    pub technique: Option<String>,
    #[serde(default)]
    pub armour: Option<String>,
    #[serde(default)]
    pub lang: Option<String>,
    #[serde(flatten)]
    pub src: Src,
}

#[derive(Debug, Clone, Default, Deserialize, PartialEq)]
pub struct SfxDecl {
    #[serde(default)]
    pub replace: Option<String>,
    #[serde(default)]
    pub technique: Option<String>,
    #[serde(default)]
    pub add: Option<String>,
    #[serde(default)]
    pub bank: Option<String>,
    #[serde(default)]
    pub category_template: Option<String>,
    #[serde(flatten)]
    pub src: Src,
}

#[derive(Debug, Clone, Default, Deserialize, PartialEq)]
pub struct MusicDecl {
    #[serde(default)]
    pub replace: Option<OneOrMany>,
    #[serde(default)]
    pub add: Option<String>,
    #[serde(default)]
    pub play_in: Option<OneOrMany>,
    /// `loop = [start, end]` seconds (end 0 = to the end), `loop = false` = play once; absent = the whole track loops.
    #[serde(default, rename = "loop")]
    pub loop_spec: Option<toml::Value>,
    #[serde(flatten)]
    pub src: Src,
}

impl MusicDecl {
    /// Loop in seconds: None = no loop, Some((0, 0)) = whole track.
    pub fn loop_secs(&self) -> Result<Option<(f64, f64)>, String> {
        let num = |v: &toml::Value| v.as_float().or_else(|| v.as_integer().map(|i| i as f64));
        match &self.loop_spec {
            None | Some(toml::Value::Boolean(true)) => Ok(Some((0.0, 0.0))),
            Some(toml::Value::Boolean(false)) => Ok(None),
            Some(toml::Value::Array(a)) if a.len() == 2 => match (num(&a[0]), num(&a[1])) {
                (Some(s), Some(e)) if s >= 0.0 && (e == 0.0 || e > s) => Ok(Some((s, e))),
                _ => Err("loop = [start, end] in seconds (end 0 = to the end)".into()),
            },
            _ => Err("loop = [start, end] in seconds, or loop = false".into()),
        }
    }
}

#[derive(Debug, Clone, Default, Deserialize, PartialEq)]
pub struct AudioToml {
    #[serde(default)]
    pub bank: Vec<BankDecl>,
    #[serde(default)]
    pub voice: Vec<VoiceDecl>,
    #[serde(default)]
    pub sfx: Vec<SfxDecl>,
    #[serde(default)]
    pub music: Vec<MusicDecl>,
}

/// Parse an `audio.toml` text; unknown keys are ignored (older / newer tools).
pub fn parse(text: &str) -> Result<AudioToml, String> {
    toml::from_str::<AudioToml>(text).map_err(|e| e.message().to_string())
}

/// The bank requests of one mod (`mod_id`, its `audio.toml` text). Err = the file does not parse; bad entries are
/// returned as messages next to the good ones.
pub fn bank_requests(mod_id: &str, text: &str) -> Result<(Vec<BankReq>, Vec<String>), String> {
    let t = parse(text)?;
    let mut out = Vec::new();
    let mut bad = Vec::new();
    for b in t.bank {
        match Group::parse(&b.group) {
            Ok(g) => out.push(BankReq { name: b.name, group: g, voice: b.voice, source: mod_id.to_string() }),
            Err(e) => bad.push(format!("[[bank]] {}: {e}", b.name)),
        }
    }
    Ok((out, bad))
}

/// `<mod dir>\audio.toml` text, None when the mod has none.
pub fn read(mod_dir: &Path) -> Option<String> {
    std::fs::read_to_string(mod_dir.join(FILE)).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn banks_and_build_sections() {
        let text = r#"
            [[voice]]
            character = "Mark Evans"
            cue = "gl010"
            file = "audio/mark.wav"
            volume = -3.0

            [[sfx]]
            replace = "ev60_03380_me"
            bank = "waza_stream"
            file = "audio/ev60_03380_me.hca"

            [[music]]
            replace = ["bg00010", "title"]
            file = "audio/t.ogg"
            loop = [1.5, 0]

            [[bank]]
            name = "evt_fwa_se"
            category_template = "sy0006"

            [[bank]]
            name = "evt_vc"
            group = "voice"

            [[bank]]
            name = "x"
            group = "somewhere"
        "#;
        let (reqs, bad) = bank_requests("fwa", text).unwrap();
        assert_eq!(reqs.len(), 2);
        assert_eq!((reqs[0].name.as_str(), &reqs[0].group, reqs[0].voice), ("evt_fwa_se", &Group::Global, None));
        assert_eq!(reqs[1].group, Group::Voice);
        assert_eq!(bad.len(), 1);
        let t = parse(text).unwrap();
        assert_eq!(t.voice[0].src.volume, Some(-3.0));
        assert_eq!(t.voice[0].cue, Some(OneOrMany::One("gl010".into())));
        assert_eq!(t.sfx[0].bank.as_deref(), Some("waza_stream"));
        assert_eq!(t.music[0].replace.as_ref().unwrap().list(), ["bg00010", "title"]);
        assert_eq!(t.music[0].loop_secs(), Ok(Some((1.5, 0.0))));
        let m = parse("[[music]]\nadd = \"x\"\nloop = false\n").unwrap();
        assert_eq!(m.music[0].loop_secs(), Ok(None));
        let m = parse("[[music]]\nadd = \"x\"\n").unwrap();
        assert_eq!(m.music[0].loop_secs(), Ok(Some((0.0, 0.0))));
        let m = parse("[[music]]\nadd = \"x\"\nloop = [5, 2]\n").unwrap();
        assert!(m.music[0].loop_secs().is_err());
        assert!(bank_requests("m", "[[bank]]\ngroup = 1\n").is_err());
        assert_eq!(bank_requests("m", "").unwrap().0.len(), 0);
    }
}
