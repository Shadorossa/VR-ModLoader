//! Menu names for the console: `crc32(name)` -> name. The loader ships no list of the game's menus: put one name
//! per line in `evt_loader\menu_names.txt` ('#' = comment; e.g. the menus your mod opens, or a list you made from
//! your own copy of the game) and the console shows those names. Unknown hashes are shown in hex.

use std::collections::HashMap;
use std::sync::OnceLock;

static MAP: OnceLock<HashMap<u32, String>> = OnceLock::new();

/// `crc32(name)` -> name of every non-comment line of `text`.
pub fn parse_list(text: &str) -> HashMap<u32, String> {
    text.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .map(|n| (crc32fast::hash(n.as_bytes()), n.to_string()))
        .collect()
}

#[cfg(all(windows, target_arch = "x86_64"))]
fn list_text() -> String {
    crate::runtime::ctx().and_then(|c| std::fs::read_to_string(c.data_dir.join("menu_names.txt")).ok()).unwrap_or_default()
}
#[cfg(not(all(windows, target_arch = "x86_64")))]
fn list_text() -> String {
    String::new()
}

/// Name of the menu whose hash is `h`.
pub fn menu_name(h: u32) -> Option<&'static str> {
    MAP.get_or_init(|| parse_list(&list_text())).get(&h).map(String::as_str)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn list_parsing() {
        let m = parse_list("# comment\ntitle_menu\n\n  my_mod_menu  \n");
        assert_eq!(m.len(), 2);
        assert_eq!(m.get(&crc32fast::hash(b"my_mod_menu")).map(String::as_str), Some("my_mod_menu"));
        assert_eq!(menu_name(0), None);
    }
}
