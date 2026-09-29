//! Voice packs of module `mods` (docs/game/media/voice-packs.md): the «Pack de voces» setting and the bank redirect.
//!
//! A voice pack is a mod whose `mod.toml` declares `voice_language = { code, name }` and ships banks
//! `files/data/common/sound_asset/<code>/<bank>.acb/.awb` (built by `research/scripts/voice_pack_build.py` from its
//! `voice/<code>/<bank>/<suffix>.<ext>` sources, XOR-encrypted like every loose CRI file). While the player's voice
//! language is `<code>`, the `fs.ResolveOverlayPath` detour ([`super::hooks`]) answers the engine's opens of
//! `data/common/sound_asset/ja|en/<bank>.acb|awb` with the pack's `<code>/<bank>.*` when the pack ships BOTH files of
//! that bank; every other bank keeps the retail voice (per bank fallback, nothing to configure).
//!
//! The same `<code>/<bank>` convention also covers **root banks**: the language-independent banks at the top of
//! `sound_asset/` (`bgm`, `bgm_chronicle`, `bgm_title`, `anime_stream`…, no `ja|en` folder). [`root_redirect_key`] /
//! [`resolve_root`] answer `data/common/sound_asset/<bank>.acb|awb` the same all-or-nothing way; [`resolve_any`] is
//! the one lookup [`super::hooks`] needs for a `sound_asset` path (docs/game/media/voice-packs.md §10).
//!
//! Why this hook point (nie.exe v7.1.2, static; call chain in docs/game/media/voice-packs.md §7): the voice banks are
//! opened by `lives::CCriSoundController` (vtable slot 1 = `0x4E2BD0`: `<path>.acb` / `.awb` -> `fs.MakeFullPath` ->
//! `CCriFileOperate::Open` (vtable `+0x28` = `0x4E70C0`) -> `ResolveOverlayPath 0x4E8730`), the same file device as
//! every other asset; the `<VLG>` token of `common/sound_asset/<VLG>/%s` is expanded by `0x4C0A80` from the token
//! table of `0xEC83E0` with `ja` / `en` only (index = system save byte `+0x7F`). Redirecting whole files keeps the
//! retail `ja` / `en` switch meaningful for the banks a pack does not cover, and [`redirect_keys`] makes it pick the
//! language of the cues a pack does not dub inside the banks it does cover (the pack's `<code>/en/` copies).
//!
//! State: `evt_loader\voice.json` `{ "voice_lang": "es" }` ("" = Ninguno, no pack), read in DllMain (the boot-time voice sheets
//! of `sound_queue_sheet` load before the menus) and rewritten by `CMND_EVT_VOICE_SET`. A change applies to the banks
//! opened afterwards; the voice sheets resident since boot keep the old language until the next start.
//!
//! | Command | Args | Returns |
//! |---|---|---|
//! | `CMND_EVT_VOICE_GET` | – | `index` (0 = Ninguno), `count` (1 + installed voice languages) |
//! | `CMND_EVT_VOICE_NAME` | `i` | `name` («Ninguno» for 0, else the pack's `voice_language.name`), `code` |
//! | `CMND_EVT_VOICE_SET` | `i` | `ok`: saved to voice.json and active for the next bank loads |

use super::{Overlay, Redirect};
use evt_modfmt::{normalize_key, voice_bank_of, voice_key, LoadPlan, VoiceLang, RETAIL_VOICE_LANGS};
use serde::{Deserialize, Serialize};
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::OnceLock;

pub const STATE_FILE: &str = "voice.json";
/// Label of value 0 of the «Pack de voces» row (no pack: the retail voices of the «Idioma de voz» row).
pub const ORIGINAL: &str = "Ninguno";

/// `evt_loader\voice.json`. Written atomically (tmp + rename).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct State {
    /// Voice language code of an installed voice pack; "" = Ninguno (retail ja / en voices).
    pub voice_lang: String,
    /// Informational: when it was last changed.
    pub changed: String,
}

impl State {
    pub fn load(path: &Path) -> State {
        std::fs::read_to_string(path).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default()
    }

    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, serde_json::to_string_pretty(self).unwrap())?;
        std::fs::rename(&tmp, path)
    }
}

/// Voice languages of the boot plan that have at least one complete bank (the «Pack de voces» values 1..).
pub fn languages(plan: &LoadPlan) -> Vec<VoiceLang> {
    plan.voice_languages().into_iter().filter(|l| l.banks > 0).collect()
}

/// Index (0 = Ninguno) of the saved code among `langs`; None when the code is set but no longer installed.
pub fn index_of(langs: &[VoiceLang], code: &str) -> Option<usize> {
    if code.is_empty() {
        return Some(0);
    }
    langs.iter().position(|l| l.code == code).map(|i| i + 1)
}

/// Key of the voice-pack copy of a retail voice bank file opened by the engine (`.../sound_asset/ja|en/<bank>.acb|awb`
/// in any path form `normalize_key` accepts), for language `code`. None for any other file.
pub fn redirect_key(engine_path: &str, code: &str) -> Option<String> {
    let key = normalize_key(engine_path)?;
    let (lang, stem, ext) = voice_bank_of(&key)?;
    if !RETAIL_VOICE_LANGS.contains(&lang) || lang == code {
        return None;
    }
    Some(voice_key(code, &format!("{stem}.{ext}")))
}

/// Folder of the English-based copies of a pack's banks: `sound_asset/<code>/en/<bank>.*` (built with
/// `voice_pack_build.py --english-fallback` from the retail `en/<bank>`: what the pack does not dub stays ENGLISH).
/// The plain `sound_asset/<code>/<bank>.*` is built from `ja/<bank>` (undubbed cues Japanese).
pub const EN_BASE_DIR: &str = "en";

/// Keys to try, in order, for a retail voice bank the engine opens while pack `code` is active. The engine asks
/// `ja/<bank>` or `en/<bank>` according to the retail «Idioma de voz» row (system byte `+0x7F`, docs voice-packs.md
/// §7.1), so that row picks the language of what the pack does not dub: an `en/` path first tries the pack's
/// English-based copy `<code>/en/<bank>`, then its Japanese-based `<code>/<bank>` (the dub wins over the language of
/// the undubbed cues); a `ja/` path only the Japanese-based one. Empty for any other file.
pub fn redirect_keys(engine_path: &str, code: &str) -> Vec<String> {
    let Some(ja_based) = redirect_key(engine_path, code) else { return Vec::new() };
    let lang = normalize_key(engine_path).and_then(|k| voice_bank_of(&k).map(|(l, _, _)| l.to_string()));
    let mut v = Vec::with_capacity(2);
    if lang.as_deref() == Some("en") {
        if let Some((dir, file)) = ja_based.rsplit_once('/') {
            v.push(format!("{dir}/{EN_BASE_DIR}/{file}"));
        }
    }
    v.push(ja_based);
    v
}

/// The overlay entry of `k` when the overlay also holds the other file of that bank (`.acb` + `.awb`).
fn complete<'a>(overlay: &'a Overlay, k: &str) -> Option<(&'a str, &'a Redirect)> {
    let (stem_key, _) = k.rsplit_once('.')?;
    if !overlay.map.contains_key(&format!("{stem_key}.acb")) || !overlay.map.contains_key(&format!("{stem_key}.awb")) {
        return None;
    }
    overlay.map.get_key_value(k).map(|(k, v)| (k.as_str(), v))
}

/// The pack file that answers `engine_path` for language `code` ([`redirect_keys`] order): only a bank whose `.acb`
/// AND `.awb` are both in the overlay (the ACB's `StreamAwbHash` / header must match the AWB the engine streams).
pub fn resolve<'a>(overlay: &'a Overlay, engine_path: &str, code: &str) -> Option<(&'a str, &'a Redirect)> {
    redirect_keys(engine_path, code).iter().find_map(|k| complete(overlay, k))
}

/// Key of the voice-pack copy of a retail ROOT bank opened by the engine (`.../sound_asset/<bank>.acb|awb`, no
/// language folder: the shared banks like `bgm`, `bgm_chronicle`, `anime_stream`, `bevent_stream`…), for language
/// `code`. Reuses the voice-bank folder convention (`sound_asset/<code>/<bank>.*`, [`voice_key`]) so a pack can carry
/// both a dubbed character and a localized shared bank side by side. None for a voice bank (those go through
/// [`redirect_key`]) or any other file.
pub fn root_redirect_key(engine_path: &str, code: &str) -> Option<String> {
    let key = normalize_key(engine_path)?;
    let (stem, ext) = evt_modfmt::root_bank_of(&key)?;
    Some(voice_key(code, &format!("{stem}.{ext}")))
}

/// The pack file that answers a ROOT-bank open `engine_path` for language `code`: same all-or-nothing rule as
/// [`resolve`] (both `.acb` and `.awb` of that exact bank must be in the overlay). A pack overrides a root bank one
/// bank at a time — shipping `es/bgm.acb|.awb` only changes `bgm`; `bgm_chronicle` and every other shared bank keep
/// playing retail audio regardless of voice language.
pub fn resolve_root<'a>(overlay: &'a Overlay, engine_path: &str, code: &str) -> Option<(&'a str, &'a Redirect)> {
    complete(overlay, &root_redirect_key(engine_path, code)?)
}

/// [`resolve`] (voice banks) then [`resolve_root`] (shared banks): the one lookup [`super::hooks`] needs for a
/// `sound_asset` path while voice language `code` is active.
pub fn resolve_any<'a>(overlay: &'a Overlay, engine_path: &str, code: &str) -> Option<(&'a str, &'a Redirect)> {
    resolve(overlay, engine_path, code).or_else(|| resolve_root(overlay, engine_path, code))
}

// ---------------------------------------------------------------- run-time state (DllMain + Lua commands)

static LANGS: OnceLock<Vec<VoiceLang>> = OnceLock::new();
/// 0 = Ninguno, i = `LANGS[i - 1]`.
static ACTIVE: AtomicUsize = AtomicUsize::new(0);
static FILE: OnceLock<std::path::PathBuf> = OnceLock::new();

/// What DllMain set up, for the init thread's log.
#[derive(Debug, Clone, Default)]
pub struct Setup {
    pub langs: Vec<VoiceLang>,
    pub active: usize,
    /// voice.json names a code no installed voice pack provides (then: Ninguno).
    pub missing: Option<String>,
}

/// DllMain: the voice languages of the boot plan and the saved choice (`<data_dir>\voice.json`).
pub fn setup(plan: &LoadPlan, data_dir: &Path) -> Setup {
    let langs = languages(plan);
    let path = data_dir.join(STATE_FILE);
    let saved = State::load(&path);
    let (active, missing) = match index_of(&langs, &saved.voice_lang) {
        Some(i) => (i, None),
        None => (0, Some(saved.voice_lang.clone())),
    };
    let _ = FILE.set(path);
    let _ = LANGS.set(langs.clone());
    ACTIVE.store(active, Ordering::Release);
    Setup { langs, active, missing }
}

/// Init-thread log lines of [`setup`]: (is_warning, text). Nothing when no voice pack is installed.
pub fn report(s: &Setup) -> Vec<(bool, String)> {
    let mut out = Vec::new();
    if s.langs.is_empty() {
        if let Some(code) = &s.missing {
            out.push((true, format!("mods: voice.json asks for voice language `{code}` but no active voice pack provides it: Ninguno")));
        }
        return out;
    }
    let list: Vec<String> = s.langs.iter().map(|l| format!("{} «{}» ({} bank(s): {})", l.code, l.name, l.banks, l.mods.join(", "))).collect();
    out.push((false, format!("mods: voice packs: {}", list.join("; "))));
    if let Some(code) = &s.missing {
        out.push((true, format!("mods: voice.json asks for voice language `{code}`, which no active voice pack provides: Ninguno")));
    }
    let active = if s.active == 0 { ORIGINAL.to_string() } else { s.langs[s.active - 1].code.clone() };
    out.push((false, format!("mods: voice pack {active} (Opciones > Ajustes del juego > Pack de voces; the retail Idioma de voz row picks ja / en for what the pack does not dub)")));
    out
}

/// Code of the active voice language (None = Ninguno). Hot path: every file open while the overlay hook is on.
pub fn active_code() -> Option<&'static str> {
    let i = ACTIVE.load(Ordering::Acquire);
    if i == 0 {
        return None;
    }
    LANGS.get()?.get(i - 1).map(|l| l.code.as_str())
}

/// (index, count) of the «Pack de voces» row.
pub fn get() -> (usize, usize) {
    (ACTIVE.load(Ordering::Acquire), 1 + LANGS.get().map_or(0, Vec::len))
}

/// (name, code) of value `i` (None when out of range).
pub fn name(i: usize) -> Option<(String, String)> {
    if i == 0 {
        return Some((ORIGINAL.to_string(), String::new()));
    }
    LANGS.get()?.get(i - 1).map(|l| (l.name.clone(), l.code.clone()))
}

/// Store value `i` in voice.json and make it active for the next bank loads.
pub fn set(i: usize) -> Result<String, String> {
    let code = match i {
        0 => String::new(),
        _ => LANGS.get().and_then(|v| v.get(i - 1)).map(|l| l.code.clone()).ok_or_else(|| format!("value {i} out of range (count {})", get().1))?,
    };
    let path = FILE.get().ok_or("voice packs not set up")?;
    let st = State { voice_lang: code.clone(), changed: format!("{} (Opciones)", crate::platform::timestamp()) };
    st.save(path).map_err(|e| format!("cannot write {}: {e}", path.display()))?;
    ACTIVE.store(i, Ordering::Release);
    Ok(if code.is_empty() { ORIGINAL.to_string() } else { code })
}

#[cfg(all(windows, target_arch = "x86_64"))]
pub use rt::init;

#[cfg(all(windows, target_arch = "x86_64"))]
mod rt {
    use crate::lua::{self, Call};
    use crate::{info, warn};

    fn cmd_get(c: &mut Call) {
        let (i, n) = super::get();
        c.push_int(i as i64);
        c.push_int(n as i64);
    }

    fn cmd_name(c: &mut Call) {
        match c.int(0).filter(|i| *i >= 0).and_then(|i| super::name(i as usize)) {
            Some((name, code)) => {
                c.push_str(&name);
                c.push_str(&code);
            }
            None => {
                c.push_str("");
                c.push_str("");
            }
        }
    }

    fn cmd_set(c: &mut Call) {
        let Some(i) = c.int(0).filter(|i| *i >= 0) else {
            warn!("mods: CMND_EVT_VOICE_SET({}) refused", c.describe(0));
            c.push_bool(false);
            return;
        };
        match super::set(i as usize) {
            Ok(v) => {
                info!("mods: voice language set to {v} (banks loaded from now on; a restart applies it everywhere)");
                c.push_bool(true);
            }
            Err(e) => {
                warn!("mods: CMND_EVT_VOICE_SET({i}): {e}");
                c.push_bool(false);
            }
        }
    }

    /// Init thread: the «Pack de voces» commands (state set up in DllMain by [`super::setup`]).
    pub fn init() {
        lua::register("CMND_EVT_VOICE_GET", cmd_get);
        lua::register("CMND_EVT_VOICE_NAME", cmd_name);
        lua::register("CMND_EVT_VOICE_SET", cmd_set);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn red(p: &str) -> Redirect {
        let mut v = p.as_bytes().to_vec();
        v.push(0);
        Redirect { module: "va".into(), cpath: v, size: 0 }
    }

    #[test]
    fn redirect_key_of_retail_paths() {
        assert_eq!(redirect_key("data/common/sound_asset/ja/c01000010.acb", "es").as_deref(), Some("data/common/sound_asset/es/c01000010.acb"));
        assert_eq!(redirect_key(r"common\sound_asset\en\C01000010.AWB", "es").as_deref(), Some("data/common/sound_asset/es/c01000010.awb"));
        assert_eq!(redirect_key("D:/game/data/common/sound_asset/ja/ev01_00300.awb", "fr").as_deref(), Some("data/common/sound_asset/fr/ev01_00300.awb"));
        assert_eq!(redirect_key("data/common/sound_asset/bgm.acb", "es"), None);
        assert_eq!(redirect_key("data/common/sound_asset/es/c01000010.acb", "es"), None);
        assert_eq!(redirect_key("data/common/sound_asset/fr/c01000010.acb", "es"), None);
        assert_eq!(redirect_key("data/common/sound/ja/ev01_00300_010_010.p3lip", "es"), None);
        assert_eq!(redirect_key("data/common/sound_asset/ja/c01000010.acf", "es"), None);
    }

    #[test]
    fn resolve_needs_both_files() {
        let mut map = HashMap::new();
        map.insert("data/common/sound_asset/es/c01000010.acb".to_string(), red("D:/m/va/files/data/common/sound_asset/es/c01000010.acb"));
        map.insert("data/common/sound_asset/es/c01000010.awb".to_string(), red("D:/m/va/files/data/common/sound_asset/es/c01000010.awb"));
        map.insert("data/common/sound_asset/es/c01000020.acb".to_string(), red("D:/m/va/x.acb"));
        let o = Overlay { map };
        let (k, r) = resolve(&o, "data/common/sound_asset/ja/c01000010.acb", "es").unwrap();
        assert_eq!(k, "data/common/sound_asset/es/c01000010.acb");
        assert!(r.cpath.ends_with(b"es/c01000010.acb\0"));
        assert!(resolve(&o, "data/common/sound_asset/en/c01000010.awb", "es").is_some());
        assert!(resolve(&o, "data/common/sound_asset/ja/c01000020.acb", "es").is_none(), "no .awb: retail voice");
        assert!(resolve(&o, "data/common/sound_asset/ja/c01000030.acb", "es").is_none());
        assert!(resolve(&o, "data/common/sound_asset/ja/c01000010.acb", "fr").is_none());
    }

    #[test]
    fn english_row_picks_the_english_based_copy() {
        assert_eq!(
            redirect_keys("data/common/sound_asset/en/c01000010.acb", "es"),
            ["data/common/sound_asset/es/en/c01000010.acb", "data/common/sound_asset/es/c01000010.acb"]
        );
        assert_eq!(redirect_keys("data/common/sound_asset/ja/c01000010.awb", "es"), ["data/common/sound_asset/es/c01000010.awb"]);
        assert!(redirect_keys("data/common/sound_asset/bgm.acb", "es").is_empty());
        let mut map = HashMap::new();
        for ext in ["acb", "awb"] {
            // c01000010: Japanese-based and English-based copies; c01000020: Japanese-based only
            map.insert(format!("data/common/sound_asset/es/c01000010.{ext}"), red(&format!("D:/m/va/es/c01000010.{ext}")));
            map.insert(format!("data/common/sound_asset/es/en/c01000010.{ext}"), red(&format!("D:/m/va/es/en/c01000010.{ext}")));
            map.insert(format!("data/common/sound_asset/es/c01000020.{ext}"), red(&format!("D:/m/va/es/c01000020.{ext}")));
        }
        // an English-based copy with only its .acb does not count
        map.insert("data/common/sound_asset/es/en/c01000020.acb".into(), red("D:/m/va/es/en/c01000020.acb"));
        let o = Overlay { map };
        // retail row English: the English-based copy (undubbed cues stay English)
        assert_eq!(resolve(&o, "data/common/sound_asset/en/c01000010.acb", "es").unwrap().0, "data/common/sound_asset/es/en/c01000010.acb");
        // retail row Japanese: the Japanese-based copy
        assert_eq!(resolve(&o, "data/common/sound_asset/ja/c01000010.awb", "es").unwrap().0, "data/common/sound_asset/es/c01000010.awb");
        // no complete English-based copy: the Japanese-based one (the dub wins)
        assert_eq!(resolve(&o, "data/common/sound_asset/en/c01000020.acb", "es").unwrap().0, "data/common/sound_asset/es/c01000020.acb");
        assert_eq!(resolve_any(&o, "data/common/sound_asset/en/c01000020.awb", "es").unwrap().0, "data/common/sound_asset/es/c01000020.awb");
        assert!(resolve(&o, "data/common/sound_asset/en/c01000030.acb", "es").is_none(), "not in the pack: retail English");
        assert_eq!(name(0).unwrap().0, "Ninguno");
    }

    #[test]
    fn root_redirect_keys() {
        assert_eq!(root_redirect_key("data/common/sound_asset/bgm.acb", "es").as_deref(), Some("data/common/sound_asset/es/bgm.acb"));
        assert_eq!(root_redirect_key(r"common\sound_asset\BGM_CHRONICLE.AWB", "es").as_deref(), Some("data/common/sound_asset/es/bgm_chronicle.awb"));
        assert_eq!(root_redirect_key("data/common/sound_asset/ja/c01000010.acb", "es"), None, "voice bank, not a root bank");
        assert_eq!(root_redirect_key("data/common/sound_asset/es/bgm.acb", "es"), None, "already the pack's own copy");
        assert_eq!(root_redirect_key("data/common/sound/bgm.acb", "es"), None, "wrong folder");
    }

    #[test]
    fn resolve_root_needs_both_files_and_is_per_bank() {
        let mut map = HashMap::new();
        map.insert("data/common/sound_asset/es/bgm.acb".to_string(), red("D:/m/va/files/data/common/sound_asset/es/bgm.acb"));
        map.insert("data/common/sound_asset/es/bgm.awb".to_string(), red("D:/m/va/files/data/common/sound_asset/es/bgm.awb"));
        map.insert("data/common/sound_asset/es/bgm_chronicle.acb".to_string(), red("D:/m/va/x.acb"));
        let o = Overlay { map };
        let (k, r) = resolve_root(&o, "data/common/sound_asset/bgm.acb", "es").unwrap();
        assert_eq!(k, "data/common/sound_asset/es/bgm.acb");
        assert!(r.cpath.ends_with(b"es/bgm.acb\0"));
        assert!(resolve_root(&o, "data/common/sound_asset/bgm.awb", "es").is_some());
        assert!(resolve_root(&o, "data/common/sound_asset/bgm_chronicle.acb", "es").is_none(), "no .awb: retail bank stays");
        assert!(resolve_root(&o, "data/common/sound_asset/anime_stream.acb", "es").is_none(), "not overridden at all: retail bank stays");
        assert!(resolve_root(&o, "data/common/sound_asset/bgm.acb", "fr").is_none());
        // resolve_any: voice banks and root banks share one lookup, each keyed off its own path shape
        assert!(resolve_any(&o, "data/common/sound_asset/bgm.acb", "es").is_some());
        assert!(resolve_any(&o, "data/common/sound_asset/ja/c01000010.acb", "es").is_none(), "not shipped by this pack");
    }

    #[test]
    fn state_and_index() {
        let langs = vec![
            VoiceLang { code: "es".into(), name: "Español".into(), mods: vec!["va".into()], banks: 2 },
            VoiceLang { code: "fr".into(), name: "Français".into(), mods: vec!["vf".into()], banks: 1 },
        ];
        assert_eq!(index_of(&langs, ""), Some(0));
        assert_eq!(index_of(&langs, "fr"), Some(2));
        assert_eq!(index_of(&langs, "it"), None);
        let dir = std::env::temp_dir().join(format!("vr-loader-voice-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join(STATE_FILE);
        assert_eq!(State::load(&p), State::default());
        std::fs::write(&p, "{ nope").unwrap();
        assert_eq!(State::load(&p).voice_lang, "");
        State { voice_lang: "es".into(), changed: "t".into() }.save(&p).unwrap();
        assert_eq!(State::load(&p).voice_lang, "es");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn languages_skip_packs_without_banks() {
        let root = std::env::temp_dir().join(format!("vr-loader-voice-plan-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        for (id, code, bank) in [("va", "es", true), ("vf", "fr", false)] {
            let d = root.join(id);
            std::fs::create_dir_all(d.join("files/data/common/sound_asset").join(code)).unwrap();
            std::fs::write(
                d.join("mod.toml"),
                format!("id=\"{id}\"\nname=\"{id}\"\nversion=\"1\"\nvoice_language = {{ code = \"{code}\", name = \"N{code}\" }}\n"),
            )
            .unwrap();
            if bank {
                for ext in ["acb", "awb"] {
                    std::fs::write(d.join(format!("files/data/common/sound_asset/{code}/c01000010.{ext}")), "x").unwrap();
                }
            }
        }
        let plan = crate::mods::plan_root(&root);
        let l = languages(&plan);
        assert_eq!(l.len(), 1);
        assert_eq!((l[0].code.as_str(), l[0].name.as_str(), l[0].banks), ("es", "Nes", 1));
        let (o, bad) = Overlay::from_plan(&plan);
        assert!(bad.is_empty());
        assert!(resolve(&o, "data/common/sound_asset/ja/c01000010.awb", "es").is_some());
        let _ = std::fs::remove_dir_all(&root);
    }
}
