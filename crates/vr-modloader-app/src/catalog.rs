//! Where to get a mod that another mod `requires` (libraries such as `vr_framework`, `clean_hud`): a small list
//! built into the exe (`catalog.toml`, filled in as the libraries are published) plus the player's own
//! `%APPDATA%\VR-ModLoader\catalog.toml` (same format; its entries win). Used by «Install missing…».
//!
//! ```toml
//! [[mod]]
//! id = "vr_framework"
//! name = "VR-Framework"
//! page = "https://gamebanana.com/mods/000000"          # opened in the browser
//! download = "https://gamebanana.com/dl/000000"        # optional: direct HTTPS link to the latest .zip
//! ```

use serde::Deserialize;
use std::path::Path;

const BUILTIN: &str = include_str!("../catalog.toml");

#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
pub struct Entry {
    pub id: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub page: String,
    #[serde(default)]
    pub download: String,
}

#[derive(Debug, Default, Deserialize)]
struct File {
    #[serde(default, rename = "mod")]
    mods: Vec<Entry>,
}

/// Entries of a catalog text (only `https://` links are kept).
pub fn parse(text: &str) -> Result<Vec<Entry>, String> {
    let f: File = toml::from_str(text).map_err(|e| e.message().to_string())?;
    Ok(f.mods
        .into_iter()
        .map(|mut e| {
            if !e.page.starts_with("https://") {
                e.page.clear();
            }
            if !e.download.starts_with("https://") {
                e.download.clear();
            }
            e
        })
        .collect())
}

/// The built-in list plus the player's file (`user`), the player's entries first.
pub fn load(user: &Path) -> Vec<Entry> {
    let mut out = std::fs::read_to_string(user).ok().and_then(|t| parse(&t).ok()).unwrap_or_default();
    for e in parse(BUILTIN).unwrap_or_default() {
        if !out.iter().any(|o| o.id == e.id) {
            out.push(e);
        }
    }
    out
}

pub fn find<'a>(cat: &'a [Entry], id: &str) -> Option<&'a Entry> {
    cat.iter().find(|e| e.id == id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtin_parses_and_user_wins() {
        parse(BUILTIN).unwrap();
        let d = crate::temp_dir("catalog").unwrap();
        let p = d.join("catalog.toml");
        std::fs::write(&p, "[[mod]]\nid = \"vr_framework\"\npage = \"https://example.org/fw\"\ndownload = \"http://insecure\"\n").unwrap();
        let c = load(&p);
        let fw = find(&c, "vr_framework").unwrap();
        assert_eq!(fw.page, "https://example.org/fw");
        assert_eq!(fw.download, "", "plain http is dropped");
        let _ = std::fs::remove_dir_all(&d);
    }
}
