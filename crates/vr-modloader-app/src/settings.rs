//! Manager settings: `%APPDATA%\VR-ModLoader\settings.toml` (game folder, language). A `--game-dir` given on the
//! command line is never saved (safe manual tests on a copy of the game).

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Settings {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub game_dir: Option<PathBuf>,
    /// `en` / `es` (empty = from the Windows UI language).
    #[serde(default)]
    pub language: String,
}

/// `%APPDATA%\VR-ModLoader` (the folder of this program's own files).
pub fn app_data_dir() -> PathBuf {
    std::env::var_os("APPDATA").map(PathBuf::from).unwrap_or_else(std::env::temp_dir).join(crate::APP_NAME)
}

pub fn settings_path() -> PathBuf {
    app_data_dir().join("settings.toml")
}

impl Settings {
    pub fn load_from(p: &Path) -> Settings {
        std::fs::read_to_string(p).ok().and_then(|t| toml::from_str(&t).ok()).unwrap_or_default()
    }
    pub fn save_to(&self, p: &Path) -> Result<(), String> {
        let text = toml::to_string(self).map_err(|e| e.to_string())?;
        evt_modfmt::write_atomic(p, &text)
    }
}

/// `es` when the user's Windows locale (`HKCU\Control Panel\International` `LocaleName`) is Spanish, else `en`.
pub fn system_language() -> String {
    let mut c = std::process::Command::new("reg");
    c.args(["query", r"HKCU\Control Panel\International", "/v", "LocaleName"]);
    let out = evt_installer::no_window(&mut c).output().ok();
    let text = out.map(|o| String::from_utf8_lossy(&o.stdout).to_lowercase()).unwrap_or_default();
    if text.lines().any(|l| l.contains("reg_sz") && l.trim_end().rsplit(' ').next().is_some_and(|v| v.starts_with("es"))) {
        "es".into()
    } else {
        "en".into()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip() {
        let d = crate::temp_dir("settings_test").unwrap();
        let p = d.join("s.toml");
        assert_eq!(Settings::load_from(&p), Settings::default());
        let s = Settings { game_dir: Some(PathBuf::from(r"D:\Games\VR")), language: "es".into() };
        s.save_to(&p).unwrap();
        assert_eq!(Settings::load_from(&p), s);
        let _ = std::fs::remove_dir_all(&d);
    }
}
