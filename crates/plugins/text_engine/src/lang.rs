//! The 9 text languages of v7.1.2 (`data/common/text/<lang>/`) and the game's language codes.

/// Folder names under `data/common/text/`, in the canonical order of the engine (fallback "first language").
pub const LANGS: [&str; 9] = ["ja", "en", "es", "fr", "de", "it", "pt", "zh_hans", "zh_hant"];

/// Block name for "every language" in a mod's text files.
pub const ALL: &str = "all";

/// nie.exe v7.1.2 language table (`.rdata` RVA 0x1980450, 21 pointers): the index is the game's language code.
/// `CMND_GET_LANGUAGE_CODE` returns 0 for Japanese (title_main shows the CESA warning when it is 0) and the voice
/// language (system save `+0x7F`) indexes this same table (docs/game/engine/hook-map.md) **[inferred: the text
/// language code uses this table too]**.
pub const GAME_LANG_TABLE: [&str; 21] = [
    "ja", "en", "fr", "es", "pt", "nl", "de", "it", "da", "fi", "no", "sv", "ru", "ar", "pl", "tr", "zh_hant", "zh_hans", "ko", "us", "eu",
];

/// Canonical folder name of a language written by a modder (`es`, `ES`, `zh-Hans`, `zh_cn`, `jp`…).
pub fn norm(s: &str) -> Option<&'static str> {
    let l = s.trim().to_ascii_lowercase().replace('-', "_");
    let l = match l.as_str() {
        "jp" | "jpn" => "ja",
        "zh" | "zh_cn" | "zh_sg" | "chs" | "zh_hans_cn" => "zh_hans",
        "zh_tw" | "zh_hk" | "cht" | "zh_hant_tw" => "zh_hant",
        "pt_br" | "pt_pt" => "pt",
        "es_es" | "es_mx" | "es_419" => "es",
        "en_us" | "en_gb" => "en",
        x => x,
    };
    LANGS.iter().find(|x| **x == l).copied()
}

/// Folder of a game language code (None for codes without a text folder).
pub fn from_game_code(n: i64) -> Option<&'static str> {
    usize::try_from(n).ok().and_then(|i| GAME_LANG_TABLE.get(i)).and_then(|c| norm(c))
}

/// Root text tables present in every language (docs/game/data/text-localization.md §1); searched, in this order, for
/// a text id written without a table.
pub const ROOT_TABLES: [&str; 43] = [
    "menu_text",
    "system_text",
    "chara_text",
    "chara_description_text",
    "item_text",
    "skill_text",
    "team_text",
    "setting_text",
    "help_list_text",
    "map_text",
    "ai_text",
    "chara_add_info_text",
    "chara_text_roma",
    "chat_text",
    "craft_text",
    "data_file_text",
    "extend_story_text",
    "inacode_text",
    "map_text_roma",
    "medal_text",
    "mission_text",
    "music_name_text",
    "players_universe_text",
    "post_text",
    "quest_purpose_text",
    "quest_title_text",
    "rpg_battle_cmd_text",
    "rpg_battle_message_text",
    "rpg_battle_text",
    "scene_archive_text",
    "scout_phase_text",
    "search_word_text",
    "shop_text",
    "soccer_common_text",
    "soccer_game_title",
    "soccer_history_check_text",
    "soccer_quick_action_text",
    "soccer_suggest_text",
    "soccer_team_passive_text",
    "soccer_technic_text",
    "staffroll_text",
    "theater_text",
    "trophy_text",
];

/// Overlay key of a text table: `data/common/text/<lang>/<table>.cfg.bin` (`table` may hold sub folders:
/// `event/ev00_00010`, `map/w10_npc_text`).
pub fn key(lang: &str, table: &str) -> String {
    format!("data/common/text/{lang}/{table}.cfg.bin")
}

/// `(lang, table)` of an overlay key made by [`key`].
pub fn split_key(key: &str) -> Option<(&'static str, String)> {
    let rest = key.strip_prefix("data/common/text/")?;
    let (l, t) = rest.split_once('/')?;
    Some((norm(l)?, t.strip_suffix(".cfg.bin")?.to_string()))
}

/// A table name a modder may write: lower-case `[a-z0-9_]` segments separated by `/`.
pub fn valid_table(t: &str) -> bool {
    !t.is_empty()
        && t.len() <= 96
        && t.split('/').all(|s| !s.is_empty() && s.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_'))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_and_codes() {
        assert_eq!(norm("ES"), Some("es"));
        assert_eq!(norm("zh-Hans"), Some("zh_hans"));
        assert_eq!(norm("zh_TW"), Some("zh_hant"));
        assert_eq!(norm("jp"), Some("ja"));
        assert_eq!(norm("nl"), None);
        assert_eq!(norm("all"), None);
        assert_eq!(from_game_code(0), Some("ja"));
        assert_eq!(from_game_code(3), Some("es"));
        assert_eq!(from_game_code(17), Some("zh_hans"));
        assert_eq!(from_game_code(5), None);
        assert_eq!(from_game_code(-1), None);
        assert_eq!(key("en", "menu_text"), "data/common/text/en/menu_text.cfg.bin");
        assert_eq!(split_key("data/common/text/en/event/ev00_00010.cfg.bin"), Some(("en", "event/ev00_00010".to_string())));
        assert!(valid_table("event/ev00_00010") && valid_table("menu_text"));
        assert!(!valid_table("Menu") && !valid_table("a//b") && !valid_table("../x"));
        // every language has every root table: no duplicates in the list
        let mut t = ROOT_TABLES.to_vec();
        t.sort();
        t.dedup();
        assert_eq!(t.len(), ROOT_TABLES.len());
    }
}
