//! Plugin `audio_engine` (mod `mods\audio_engine\`, `audio_engine.dll`): the audio layer of the ModLoader
//! (docs/game/media/audio-engine.md).
//!
//! * [`armed`]: the armour shout per armour (`<bank>_<armour id>` before `<bank>_armed`), port of the ModLoader's
//!   built-in module `armed_voice`, which yields to this plugin (`provides = ["armed_voice"]`).
//! * [`build`]: every mod's `audio.toml` (`[[voice]]`, `[[sfx]]`, `[[music]]`) built at the early phase from the
//!   player's own retail banks + the mods' plain audio (or ready `.hca`), per-cue merge across mods, hash cache in
//!   `evt_loader\cache\audio_engine`, served with the ModLoader's `file_serve`.
//! * [`sheet`]: the `sound_queue_sheet` merge (`[[bank]]` + the new banks); [`build::bgm_config`]: new BGM ids and
//!   redirects.
//! * [`framework`]: the generic pieces (mod discovery, overlay winner, merge rule, cache), from the shared
//!   `vr-framework` crate.
//!
//! * [`voice_row`]: the «Pack de voces» row of Opciones > «Ajustes del juego» (settings list row + texts built at the
//!   early phase and served; its Lua is `lua/setting_menu/110_voice_pack.lua` of the mod).
//!
//! The same build runs at pack time with the `audio_build` tool (`src/bin/audio_build.rs`, the fast path: a mod that
//! ships its built banks is not rebuilt at boot). Voice packs are still served by the ModLoader's `mods` module.

pub mod armed;
pub mod aura;
pub mod build;
pub mod decl;
pub mod framework;
pub mod index;
pub mod sheet;
pub mod source;
pub mod voice_row;

pub use framework::ModDir;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// Configuration (`mods\audio_engine\config.toml`, `[audio_engine]` and `[mods.audio_engine]` of
/// `evt_loader\config.toml`, merged by the ModLoader).
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Cfg {
    pub armed_voice: ArmedVoiceCfg,
    pub queue_sheet: QueueSheetCfg,
    pub build: BuildCfg,
    pub voice_row: VoiceRowCfg,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct VoiceRowCfg {
    /// Add the «Pack de voces» row to Opciones > «Ajustes del juego» (settings list row + its texts, served at boot).
    pub enabled: bool,
}

impl Default for VoiceRowCfg {
    fn default() -> Self {
        VoiceRowCfg { enabled: true }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct ArmedVoiceCfg {
    /// Hook `PlayCharaVoice`: `<bank>_armed` → `<bank>_<armour id>` when that cue exists.
    pub enabled: bool,
}

impl Default for ArmedVoiceCfg {
    fn default() -> Self {
        ArmedVoiceCfg { enabled: true }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct QueueSheetCfg {
    /// Merge the `[[bank]]` declarations (and the new banks of `[[sfx]] add` / `[[music]]`) into `sound_queue_sheet`.
    pub merge: bool,
}

impl Default for QueueSheetCfg {
    fn default() -> Self {
        QueueSheetCfg { merge: true }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct BuildCfg {
    /// Build the mods' `[[voice]]` / `[[sfx]]` / `[[music]]` at boot (cached).
    pub enabled: bool,
}

impl Default for BuildCfg {
    fn default() -> Self {
        BuildCfg { enabled: true }
    }
}

impl Cfg {
    /// Parse the merged configuration text (defaults on error, with the message).
    pub fn from_text(t: &str) -> (Cfg, Option<String>) {
        match toml::from_str::<Cfg>(t) {
            Ok(c) => (c, None),
            Err(e) => (Cfg::default(), Some(e.message().to_string())),
        }
    }
}

// ---------------------------------------------------------------- sound_queue_sheet (file side)

/// Relative path of the sheet inside a game / mod `files` root.
pub fn sheet_rel() -> PathBuf {
    ["data", "common", "sound", "sound_queue_sheet.cfg.bin"].iter().collect()
}

/// The fixed-size placeholder of the audio_engine mod (served by the overlay on a ModLoader without `file_serve`).
pub fn served_path(self_dir: &Path) -> PathBuf {
    self_dir.join("files").join(sheet_rel())
}

/// The retail copy shipped with the mod (base when the game's own sheet cannot be read).
pub fn shipped_base(self_dir: &Path) -> PathBuf {
    self_dir.join("sound").join("sound_queue_sheet.base.cfg.bin")
}

/// The merged sheet and what went in.
#[derive(Debug, Clone)]
pub struct SheetBuild {
    pub sheet: sheet::Sheet,
    pub report: sheet::MergeReport,
    pub warnings: Vec<String>,
}

/// Merge onto `base`: for every active mod in load order, the extra rows of a whole sheet it still ships
/// (`files\data\common\sound\…`, imported with a warning) and its `[[bank]]` declarations; then `extra` (the new banks
/// of the audio build).
pub fn sheet_build(base: &[u8], self_id: &str, mods: &[ModDir], extra: &[sheet::BankReq], legacy_wins_note: bool) -> Result<SheetBuild, String> {
    let base_sheet = sheet::Sheet::parse(base).map_err(|e| format!("base sheet: {e}"))?;
    let self_index = mods.iter().find(|m| m.id == self_id).map(|m| m.load_index);
    let mut warnings = Vec::new();
    let mut reqs = Vec::new();
    let mut order: Vec<&ModDir> = mods.iter().collect();
    order.sort_by_key(|m| m.load_index);
    for m in order {
        if m.id != self_id {
            if let Ok(b) = std::fs::read(m.dir.join("files").join(sheet_rel())) {
                match sheet::Sheet::parse(&b) {
                    Ok(legacy) => {
                        let extra = base_sheet.extra_rows(&legacy);
                        for (gid, name, vlg) in &extra {
                            reqs.push(sheet::BankReq { name: name.clone(), group: sheet::Group::Id(*gid), voice: Some(*vlg != 0), source: format!("{} (whole sheet)", m.id) });
                        }
                        let wins = legacy_wins_note && self_index.is_some_and(|s| m.load_index > s);
                        warnings.push(format!(
                            "mod {} ships a whole sound_queue_sheet ({} extra bank(s) merged){}: declare its banks in audio.toml instead",
                            m.id,
                            extra.len(),
                            if wins { " and loads after audio_engine, so ITS file is the one served (the merge is not used)" } else { "" }
                        ));
                    }
                    Err(e) => warnings.push(format!("mod {}: its sound_queue_sheet does not parse ({e}): ignored", m.id)),
                }
            }
        }
        if let Some(text) = decl::read(&m.dir) {
            match decl::bank_requests(&m.id, &text) {
                Ok((r, bad)) => {
                    reqs.extend(r);
                    warnings.extend(bad.into_iter().map(|b| format!("mod {}: {b}", m.id)));
                }
                Err(e) => warnings.push(format!("mod {}: {} does not parse ({e}): its banks are not registered", m.id, decl::FILE)),
            }
        }
    }
    reqs.extend(extra.iter().cloned());
    let (merged, report) = sheet::merge(base, &reqs)?;
    Ok(SheetBuild { sheet: merged, report, warnings })
}

/// Result of [`boot_merge`] (the fixed-size placeholder path).
#[derive(Debug, Clone, Default)]
pub struct BootMerge {
    pub base: String,
    pub target: PathBuf,
    pub report: sheet::MergeReport,
    /// The new file content; None = the served file is already this merge.
    pub write: Option<Vec<u8>>,
    pub warnings: Vec<String>,
}

/// The placeholder path (ModLoader without `file_serve`): base = the game's loose sheet or the shipped retail copy,
/// merged, padded to the size of the served placeholder (registered by the overlay at DllMain).
pub fn boot_merge(game_dir: &Path, self_id: &str, self_dir: &Path, mods: &[ModDir]) -> Result<BootMerge, String> {
    let target = served_path(self_dir);
    let current = std::fs::read(&target).map_err(|e| format!("served placeholder {} missing ({e})", target.display()))?;
    let loose = game_dir.join(sheet_rel());
    let (base_src, base_bytes) = match std::fs::read(&loose) {
        Ok(b) => ("game data (loose file installed by the app)".to_string(), b),
        Err(_) => {
            let p = shipped_base(self_dir);
            let b = std::fs::read(&p).map_err(|e| format!("no base sheet: neither {} nor {} ({e})", loose.display(), p.display()))?;
            ("retail copy of the mod".to_string(), b)
        }
    };
    let sb = sheet_build(&base_bytes, self_id, mods, &[], true)?;
    let bytes = sb.sheet.to_bytes_padded(current.len()).map_err(|e| format!("{e} (served placeholder {})", target.display()))?;
    let write = (bytes != current).then_some(bytes);
    Ok(BootMerge { base: base_src, target, report: sb.report, write, warnings: sb.warnings })
}

#[cfg(all(windows, target_arch = "x86_64"))]
mod rt;

#[cfg(test)]
mod tests {
    use super::*;

    const DUMP: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../../assets/v7.1.2/data/common/sound/sound_queue_sheet.cfg.bin");

    #[test]
    fn config_defaults() {
        let (c, e) = Cfg::from_text("");
        assert!(e.is_none() && c.armed_voice.enabled && c.queue_sheet.merge && c.build.enabled);
        let (c, _) = Cfg::from_text("[armed_voice]\nenabled = false\n[build]\nenabled = false\n");
        assert!(!c.armed_voice.enabled && c.queue_sheet.merge && !c.build.enabled);
        let (c, e) = Cfg::from_text("armed_voice = 3");
        assert!(e.is_some() && c.armed_voice.enabled);
    }

    fn write(p: &Path, b: &[u8]) {
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, b).unwrap();
    }

    #[test]
    fn sheet_build_takes_extra_banks() {
        let Ok(retail) = std::fs::read(DUMP) else { return };
        let extra = [sheet::BankReq { name: "evt_se_x".into(), group: sheet::Group::Global, voice: Some(false), source: "x".into() }];
        let sb = sheet_build(&retail, "audio_engine", &[], &extra, false).unwrap();
        assert_eq!(sb.report.added.len(), 1);
        assert_eq!(sb.sheet.rows()[12].0, "evt_se_x");
    }

    #[test]
    fn boot_merge_end_to_end() {
        let Ok(retail) = std::fs::read(DUMP) else { return };
        let root = std::env::temp_dir().join(format!("evt-audio-engine-boot-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let game = root.join("game");
        let me = root.join("mods").join("audio_engine");
        let slot = sheet::Sheet::parse(&retail).unwrap().to_bytes_padded(sheet::SLOT_SIZE).unwrap();
        write(&served_path(&me), &slot);
        write(&shipped_base(&me), &retail);
        let fwa = root.join("mods").join("fwa_sfx");
        write(&fwa.join(decl::FILE), b"[[bank]]\nname = \"evt_fwa_se\"\ncategory_template = \"sy0006\"\n");
        let old = root.join("mods").join("old_bgm");
        let (legacy, _) = sheet::merge(&retail, &[sheet::BankReq { name: "evt_bgm".into(), group: sheet::Group::Global, voice: None, source: "x".into() }]).unwrap();
        write(&old.join("files").join(sheet_rel()), &legacy.to_bytes().unwrap());
        let mods = vec![
            ModDir { id: "audio_engine".into(), dir: me.clone(), load_index: 0 },
            ModDir { id: "fwa_sfx".into(), dir: fwa.clone(), load_index: 1 },
            ModDir { id: "old_bgm".into(), dir: old.clone(), load_index: 2 },
        ];
        let r = boot_merge(&game, "audio_engine", &me, &mods).unwrap();
        assert_eq!(r.base, "retail copy of the mod");
        let names: Vec<&str> = r.report.added.iter().map(|a| a.1.as_str()).collect();
        assert_eq!(names, ["evt_fwa_se", "evt_bgm"]);
        assert_eq!(r.warnings.len(), 1, "{:?}", r.warnings);
        assert!(r.warnings[0].contains("old_bgm") && r.warnings[0].contains("ITS file is the one served"));
        let bytes = r.write.clone().unwrap();
        assert_eq!(bytes.len(), sheet::SLOT_SIZE);
        std::fs::write(&r.target, &bytes).unwrap();
        assert!(boot_merge(&game, "audio_engine", &me, &mods).unwrap().write.is_none());
        let (other, _) = sheet::merge(&retail, &[sheet::BankReq { name: "evt_fwa_se".into(), group: sheet::Group::Global, voice: None, source: "x".into() }]).unwrap();
        write(&game.join(sheet_rel()), &other.to_bytes().unwrap());
        let r = boot_merge(&game, "audio_engine", &me, &mods).unwrap();
        assert!(r.base.starts_with("game data"));
        assert_eq!(r.report.present, vec![("fwa_sfx".to_string(), "evt_fwa_se".to_string())]);
        assert!(r.write.is_none(), "same rows in the same order: same bytes");
        std::fs::remove_file(served_path(&me)).unwrap();
        assert!(boot_merge(&game, "audio_engine", &me, &mods).is_err());
        let _ = std::fs::remove_dir_all(&root);
    }
}
