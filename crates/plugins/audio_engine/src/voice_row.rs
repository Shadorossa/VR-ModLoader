//! The «Pack de voces» / "Voice pack" row of Opciones > «Ajustes del juego» (docs/game/media/voice-packs.md §12,
//! docs/game/media/audio-engine.md §4): the DATA side, built at the early phase from the player's own files and served
//! with the ModLoader's `file_serve`. The Lua side is `mods\audio_engine\lua\setting_menu\110_voice_pack.lua`; the
//! values come from the ModLoader's `mods` module (`CMND_EVT_VOICE_GET / _NAME / _SET`, they stay in the core: the pack
//! is chosen at run time and served by the core's path redirect, which a plugin cannot do after the early phase).
//!
//! * `setting_list_config_7.00.17.cfg.bin`: one `SETTING_INFO` row in tab 0 after its last row (setting type 40 = the
//!   Lua-driven «FPS máximos» kind, its empty `SETTING_OBJ_INFO`), with its `EXPLANATION_TEXT_LIST_*` entries.
//!   **One row only**: a row with the same id ([`ROW_KEY`] = `crc32("evt_set_voice_lang")`) is never added twice; an
//!   existing one (left by an earlier install) is *adopted*: label / help pointed at this mod's texts.
//! * texts: `text.toml` of the mod (text_engine format, 9 languages, English fallback), ids
//!   `crc32("audio_engine.<name>")`. With the text_engine plugin active it merges them (it serves `menu_text`, a second
//!   server of the same file would be refused); without it, [`patch_menu_text`] adds them to each language's
//!   `menu_text` and the plugin serves those.

use l5_core::hash::crc32_str;
use l5_core::t2b::{Entry, T2b, Value};
use std::collections::BTreeMap;

/// Game path of the settings list.
pub const SETTINGS: &str = "data/common/gamedata/setting_menu/setting_list_config_7.00.17.cfg.bin";
/// The 9 text languages (folders of `data/common/text/`).
pub const LANGS: [&str; 9] = ["ja", "en", "es", "fr", "de", "it", "pt", "zh_hans", "zh_hant"];
/// Mod id used in the text keys (fixed: the Lua has the ids).
pub const TEXT_MOD: &str = "audio_engine";
/// Text names (`text.toml` `[<lang>.new]`) the row and its Lua use.
pub const NAMES: [&str; 4] = ["voice_pack", "voice_pack_help", "voice_pack_none", "voice_pack_off"];
/// `SETTING_INFO` id of the row (an older row with this id is recognised and adopted).
pub const ROW_KEY: &str = "evt_set_voice_lang";
/// `EXPLANATION_TEXT_LIST_INFO` id of the row's help.
pub const HELP_LIST_KEY: &str = "audio_engine.voice_pack_help_list";
/// Row values: tab 0, `SETTING_OBJ_INFO` of «FPS máximos» (no value texts), type 40, kind 1, every platform.
const TAB: i32 = 0;
const OBJ_INFO_EMPTY: u32 = 0x0431_461D;
const SETTING_TYPE_DYNAMIC_TEXT: i32 = 40;
const KIND: i32 = 1;
const PLATFORM_ALL: u32 = 0xBC7F_972E;

pub fn row_id() -> u32 {
    crc32_str(ROW_KEY)
}

/// Text id of `name` (= text_engine's id of the new text `audio_engine.<name>`).
pub fn text_id(name: &str) -> u32 {
    crc32_str(&format!("{TEXT_MOD}.{name}"))
}

pub fn menu_text_path(lang: &str) -> String {
    format!("data/common/text/{lang}/menu_text.cfg.bin")
}

// ---------------------------------------------------------------- texts (text.toml)

/// `lang -> name -> text` for the 9 languages, English (else the first language written) where one is missing.
pub type Texts = BTreeMap<String, BTreeMap<String, String>>;

/// Parse the mod's `text.toml` (`[<lang>.new] name = "..."`, text_engine format) into the texts of [`NAMES`] per
/// language. Err when a name has no text in any language.
pub fn texts_from_toml(src: &str) -> Result<Texts, String> {
    let t: toml::Table = src.parse().map_err(|e: toml::de::Error| format!("text.toml: {}", e.message()))?;
    let get = |lang: &str, name: &str| -> Option<String> {
        t.get(lang)?.get("new")?.get(name)?.as_str().map(|s| s.replace("\r\n", "\\n").replace('\n', "\\n"))
    };
    let mut out = Texts::new();
    for lang in LANGS {
        let mut m = BTreeMap::new();
        for name in NAMES {
            let s = get(lang, name).or_else(|| get("en", name)).or_else(|| LANGS.iter().find_map(|l| get(l, name)));
            m.insert(name.to_string(), s.ok_or_else(|| format!("text.toml: `{name}` has no text in any language"))?);
        }
        out.insert(lang.to_string(), m);
    }
    Ok(out)
}

// ---------------------------------------------------------------- setting_list_config

fn pos(e: &[Entry], name: &str) -> Option<usize> {
    let h = crc32_str(name);
    e.iter().position(|x| x.hash == h)
}

fn int(e: &Entry, i: usize) -> Option<u32> {
    e.values.get(i).and_then(Value::as_int).map(|v| v as u32)
}

fn bump(e: &mut [Entry], name: &str, by: i32) -> Result<(), String> {
    let i = pos(e, name).ok_or_else(|| format!("no {name}"))?;
    match e[i].values.first_mut() {
        Some(Value::Int(n)) => {
            *n += by;
            Ok(())
        }
        _ => Err(format!("{name}: no count")),
    }
}

fn like(proto: &Entry, ints: &[u32]) -> Entry {
    let mut e = proto.clone();
    e.values = ints.iter().map(|&v| Value::Int(v as i32)).collect();
    e
}

/// What [`patch_settings`] did.
#[derive(Debug, Clone, PartialEq)]
pub enum RowChange {
    /// The row was added (tab 0, after its last row).
    Added,
    /// A row with the same id existed (e.g. from an earlier install): label / help now point at this mod's texts.
    Adopted,
    /// The row is already exactly this one: nothing to serve.
    Present,
}

/// Add (or adopt) the row in the settings list `base`. Returns the new file (None = unchanged) and what happened.
pub fn patch_settings(base: &[u8]) -> Result<(Option<Vec<u8>>, RowChange), String> {
    let mut doc = T2b::parse(base).map_err(|e| format!("not a T2B settings list: {e}"))?;
    let (rid, label, list, help) = (row_id(), text_id("voice_pack"), crc32_str(HELP_LIST_KEY), text_id("voice_pack_help"));
    let h_info = crc32_str("SETTING_INFO");
    let h_list = crc32_str("EXPLANATION_TEXT_LIST_INFO");
    let e = &mut doc.entries;
    // 1. the help list (TEXT_DATA + INFO / REF pair), unless a list with our id exists
    if !e.iter().any(|x| x.hash == h_list && int(x, 0) == Some(list)) {
        let td_h = crc32_str("EXPLANATION_TEXT_LIST_TEXT_DATA");
        let n_td = e.iter().filter(|x| x.hash == td_h).count() as u32;
        let proto_td = e.iter().find(|x| x.hash == td_h).cloned().ok_or("no EXPLANATION_TEXT_LIST_TEXT_DATA")?;
        let proto_li = e.iter().find(|x| x.hash == h_list).cloned().ok_or("no EXPLANATION_TEXT_LIST_INFO")?;
        let proto_ref = e.iter().find(|x| x.hash == crc32_str("EXPLANATION_TEXT_LIST_INFO_REF_TEXT_DATA")).cloned().ok_or("no EXPLANATION_TEXT_LIST_INFO_REF_TEXT_DATA")?;
        let td_end = pos(e, "EXPLANATION_TEXT_LIST_TEXT_DATA_LIST_END").ok_or("no EXPLANATION_TEXT_LIST_TEXT_DATA_LIST_END")?;
        e.insert(td_end, like(&proto_td, &[help]));
        bump(e, "EXPLANATION_TEXT_LIST_TEXT_DATA_LIST_BEG", 1)?;
        let li_end = pos(e, "EXPLANATION_TEXT_LIST_INFO_LIST_END").ok_or("no EXPLANATION_TEXT_LIST_INFO_LIST_END")?;
        e.splice(li_end..li_end, [like(&proto_li, &[list]), like(&proto_ref, &[n_td, 1])]);
        bump(e, "EXPLANATION_TEXT_LIST_INFO_LIST_BEG", 1)?;
    }
    // 2. the row
    let change = match e.iter().position(|x| x.hash == h_info && int(x, 1) == Some(rid)) {
        Some(i) => {
            if int(&e[i], 2) == Some(label) && int(&e[i], 3) == Some(list) {
                RowChange::Present
            } else {
                if e[i].values.len() < 4 {
                    return Err("the existing row has fewer than 4 values".into());
                }
                e[i].values[2] = Value::Int(label as i32);
                e[i].values[3] = Value::Int(list as i32);
                RowChange::Adopted
            }
        }
        None => {
            if !e.iter().any(|x| x.hash == crc32_str("SETTING_OBJ_INFO") && int(x, 0) == Some(OBJ_INFO_EMPTY)) {
                return Err(format!("no SETTING_OBJ_INFO {OBJ_INFO_EMPTY:#010X} (the empty one of «FPS máximos»)"));
            }
            let last = e.iter().rposition(|x| x.hash == h_info && x.values.first().and_then(Value::as_int) == Some(TAB)).ok_or("no SETTING_INFO in tab 0")?;
            let row = like(&e[last], &[TAB as u32, rid, label, list, OBJ_INFO_EMPTY, SETTING_TYPE_DYNAMIC_TEXT as u32, KIND as u32, PLATFORM_ALL]);
            e.insert(last + 1, row);
            bump(e, "SETTING_INFO_LIST_BEG", 1)?;
            RowChange::Added
        }
    };
    let out = doc.to_bytes().map_err(|e| format!("settings list: {e}"))?;
    Ok(((out != base).then_some(out), change))
}

// ---------------------------------------------------------------- menu_text

/// Add / update the text rows `(id, text)` of a `menu_text` table (`TEXT_INFO(id, 0, text)`, cloned from its first
/// row, before `TEXT_INFO_END`, `TEXT_INFO_BEGIN` count raised). None = the table already has exactly these texts.
pub fn patch_menu_text(base: &[u8], rows: &[(u32, String)]) -> Result<Option<Vec<u8>>, String> {
    let mut doc = T2b::parse(base).map_err(|e| format!("not a T2B text table: {e}"))?;
    let h_row = crc32_str("TEXT_INFO");
    let proto = doc.entries.iter().find(|x| x.hash == h_row).cloned().ok_or("no TEXT_INFO row")?;
    let mut changed = false;
    for (id, text) in rows {
        let at = doc.entries.iter().position(|x| x.hash == h_row && int(x, 0) == Some(*id) && x.values.get(1).and_then(Value::as_int) == Some(0));
        match at {
            Some(i) => {
                let v = &mut doc.entries[i].values;
                if v.len() < 3 {
                    v.resize(3, Value::String(None));
                }
                if v[2].as_str() != Some(text.as_str()) {
                    v[2] = Value::String(Some(text.clone()));
                    changed = true;
                }
            }
            None => {
                let mut e = proto.clone();
                for (i, v) in e.values.iter_mut().enumerate() {
                    *v = match (i, &*v) {
                        (0, _) => Value::Int(*id as i32),
                        (1, _) => Value::Int(0),
                        (2, _) => Value::String(Some(text.clone())),
                        (_, Value::String(_)) => Value::String(None),
                        (_, Value::Int(_)) => Value::Int(0),
                        (_, Value::Float(_)) => Value::Float(0.0),
                    };
                }
                let end = pos(&doc.entries, "TEXT_INFO_END").ok_or("no TEXT_INFO_END")?;
                doc.entries.insert(end, e);
                bump(&mut doc.entries, "TEXT_INFO_BEGIN", 1)?;
                changed = true;
            }
        }
    }
    if !changed {
        return Ok(None);
    }
    doc.to_bytes().map(Some).map_err(|e| format!("menu_text: {e}"))
}

/// The rows of one language: `(text id, text)` of every name.
pub fn rows_for(texts: &Texts, lang: &str) -> Vec<(u32, String)> {
    texts.get(lang).map(|m| m.iter().map(|(n, s)| (text_id(n), s.clone())).collect()).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn repo() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("..").join("..").join("..")
    }

    /// The mod's source folder (`mods/audio_engine`).
    fn mod_src(rel: &str) -> PathBuf {
        repo().join("mods/audio_engine").join(rel)
    }

    fn retail(rel: &str) -> Option<Vec<u8>> {
        let p = repo().join("assets").join("v7.1.2").join(rel.replace('/', std::path::MAIN_SEPARATOR_STR));
        std::fs::read(p).ok()
    }

    fn infos(b: &[u8]) -> Vec<Vec<i32>> {
        let h = crc32_str("SETTING_INFO");
        T2b::parse(b).unwrap().entries.iter().filter(|x| x.hash == h).map(|x| x.values.iter().filter_map(Value::as_int).collect()).collect()
    }

    fn count(b: &[u8], name: &str) -> (usize, i32) {
        let d = T2b::parse(b).unwrap();
        let h = crc32_str(name);
        let n = d.entries.iter().filter(|x| x.hash == h).count();
        let beg = d.entries.iter().find(|x| x.hash == crc32_str(&format!("{name}_LIST_BEG"))).and_then(|x| x.values[0].as_int()).unwrap_or(-1);
        (n, beg)
    }

    #[test]
    fn texts_of_the_mod() {
        let src = std::fs::read_to_string(mod_src("text.toml")).unwrap();
        let t = texts_from_toml(&src).unwrap();
        assert_eq!(t.len(), 9);
        assert_eq!(t["es"]["voice_pack"], "Pack de voces");
        assert_eq!(t["en"]["voice_pack_none"], "None");
        assert!(t.values().all(|m| m.len() == NAMES.len() && m.values().all(|s| !s.is_empty())));
        // a language without texts falls back to English
        let t2 = texts_from_toml("[en.new]\nvoice_pack='V'\nvoice_pack_help='H'\nvoice_pack_none='N'\nvoice_pack_off='O'\n[es.new]\nvoice_pack='P'\n").unwrap();
        assert_eq!((t2["es"]["voice_pack"].as_str(), t2["es"]["voice_pack_none"].as_str(), t2["ja"]["voice_pack"].as_str()), ("P", "N", "V"));
        assert!(texts_from_toml("[en.new]\nvoice_pack='V'\n").is_err());
        // the ids the Lua has hard-coded
        assert_eq!((text_id("voice_pack"), text_id("voice_pack_none"), text_id("voice_pack_off")), (249340083, 581565135, 2052939705));
        let lua = std::fs::read_to_string(mod_src("lua/setting_menu/110_voice_pack.lua")).unwrap();
        for n in ["voice_pack", "voice_pack_none", "voice_pack_off"] {
            assert!(lua.contains(&format!("{} --[[CMND_TEXT_GET]]", 4074371074u32)) && lua.contains(&text_id(n).to_string()), "{n}");
        }
    }

    #[test]
    fn row_added_once_on_retail() {
        let Some(base) = retail("data/common/gamedata/setting_menu/setting_list_config_7.00.17.cfg.bin") else { return };
        let (out, ch) = patch_settings(&base).unwrap();
        assert_eq!(ch, RowChange::Added);
        let out = out.unwrap();
        let (old, new) = (infos(&base), infos(&out));
        assert_eq!(new.len(), old.len() + 1);
        let at = new.iter().position(|v| v[1] as u32 == row_id()).unwrap();
        assert_eq!(new[at - 1][1] as u32, 0x728E_4B23, "right after the last tab-0 row (retail «Quitar aparición de Héroes ya obtenidos»)");
        assert_ne!(new[at + 1][0], 0, "the last row of tab 0");
        assert_eq!(new[at][..6], [0, row_id() as i32, text_id("voice_pack") as i32, crc32_str(HELP_LIST_KEY) as i32, OBJ_INFO_EMPTY as i32, 40]);
        let mut rest = new.clone();
        rest.remove(at);
        assert_eq!(rest, old, "retail rows untouched");
        assert_eq!(count(&out, "SETTING_INFO"), (79, 79));
        assert_eq!(count(&out, "EXPLANATION_TEXT_LIST_TEXT_DATA"), (97, 97));
        assert_eq!(count(&out, "EXPLANATION_TEXT_LIST_INFO"), (96, 96));
        // idempotent: the served file is recognised
        assert_eq!(patch_settings(&out).unwrap(), (None, RowChange::Present));
    }

    #[test]
    fn an_older_row_with_the_same_id_is_adopted() {
        let Some(base) = retail("data/common/gamedata/setting_menu/setting_list_config_7.00.17.cfg.bin") else { return };
        // an older row: same row id, other label / help list ids
        let (out, _) = patch_settings(&base).unwrap();
        let mut d = T2b::parse(&out.unwrap()).unwrap();
        let h = crc32_str("SETTING_INFO");
        let i = d.entries.iter().position(|x| x.hash == h && int(x, 1) == Some(row_id())).unwrap();
        d.entries[i].values[2] = Value::Int(crc32_str("evt_set_voice_lang_btn") as i32);
        d.entries[i].values[3] = Value::Int(crc32_str("evt_set_voice_lang_expl_list") as i32);
        let legacy = d.to_bytes().unwrap();
        let (out2, ch) = patch_settings(&legacy).unwrap();
        assert_eq!(ch, RowChange::Adopted);
        let out2 = out2.unwrap();
        let rows: Vec<_> = infos(&out2).into_iter().filter(|v| v[1] as u32 == row_id()).collect();
        assert_eq!(rows.len(), 1, "never a second row");
        assert_eq!(rows[0][2] as u32, text_id("voice_pack"));
        assert_eq!(count(&out2, "EXPLANATION_TEXT_LIST_INFO"), (96, 96), "our help list is not added twice");
    }

    #[test]
    fn menu_text_rows_in_every_language() {
        let src = std::fs::read_to_string(mod_src("text.toml")).unwrap();
        let t = texts_from_toml(&src).unwrap();
        for lang in LANGS {
            let Some(base) = retail(&menu_text_path(lang)) else { return };
            // our ids are not game text ids (text_engine would move them to `#1`)
            let d = T2b::parse(&base).unwrap();
            for n in NAMES {
                assert!(!d.entries.iter().any(|x| int(x, 0) == Some(text_id(n))), "{lang}: {n} collides with a game text");
            }
            let rows = rows_for(&t, lang);
            let out = patch_menu_text(&base, &rows).unwrap().unwrap();
            let d2 = T2b::parse(&out).unwrap();
            for (id, s) in &rows {
                let r = d2.entries.iter().find(|x| x.hash == crc32_str("TEXT_INFO") && int(x, 0) == Some(*id)).unwrap();
                assert_eq!(r.values[2].as_str(), Some(s.as_str()), "{lang}");
            }
            let n = d2.entries.iter().filter(|x| x.hash == crc32_str("TEXT_INFO")).count() as i32;
            assert_eq!(d2.entries.iter().find(|x| x.hash == crc32_str("TEXT_INFO_BEGIN")).unwrap().values[0].as_int(), Some(n));
            assert_eq!(patch_menu_text(&out, &rows).unwrap(), None, "{lang}: idempotent");
        }
    }
}
