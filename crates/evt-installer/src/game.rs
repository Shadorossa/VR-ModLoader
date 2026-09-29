//! Finding the game, checking its version, and the checks before touching it.

use std::path::{Path, PathBuf};

pub const APPID: &str = "2799860";
pub const GAME_FOLDER: &str = "INAZUMA ELEVEN Victory Road";
/// `nie.exe` of PC v7.1.2 (same value as `vr-loader`'s `gate::V712_SHA1`).
pub const V712_SHA1: &str = "d27e76217730783fec8df4a3b0541cb15fe8f100";
pub const DEFAULT_GAME: &str = r"D:\SteamLibrary\steamapps\common\INAZUMA ELEVEN Victory Road";

/// A game install: `nie.exe` and `data\cpk_list.cfg.bin`.
pub fn is_game_dir(p: &Path) -> bool {
    p.join("nie.exe").is_file() && p.join("data").join("cpk_list.cfg.bin").is_file()
}

/// Candidate game folders, best first (Steam libraries from `libraryfolders.vdf`, then usual places).
pub fn find_games() -> Vec<PathBuf> {
    let mut libs: Vec<PathBuf> = Vec::new();
    for root in steam_roots() {
        let vdf = root.join("steamapps").join("libraryfolders.vdf");
        if let Ok(t) = std::fs::read_to_string(&vdf) {
            libs.extend(vdf_paths(&t));
        }
        libs.push(root);
    }
    for drive in 'C'..='Z' {
        for l in ["SteamLibrary", r"Steam", r"Program Files (x86)\Steam", r"Program Files\Steam", r"Juegos\SteamLibrary", r"Games\SteamLibrary"] {
            libs.push(PathBuf::from(format!(r"{drive}:\{l}")));
        }
    }
    let mut out: Vec<PathBuf> = Vec::new();
    for lib in libs {
        let steamapps = lib.join("steamapps");
        let mut dirs = Vec::new();
        if let Ok(acf) = std::fs::read_to_string(steamapps.join(format!("appmanifest_{APPID}.acf"))) {
            if let Some(d) = vdf_value(&acf, "installdir") {
                dirs.push(steamapps.join("common").join(d));
            }
        }
        dirs.push(steamapps.join("common").join(GAME_FOLDER));
        for d in dirs {
            if is_game_dir(&d) && !out.iter().any(|o| same_path(o, &d)) {
                out.push(d);
            }
        }
    }
    let def = PathBuf::from(DEFAULT_GAME);
    if is_game_dir(&def) && !out.iter().any(|o| same_path(o, &def)) {
        out.push(def);
    }
    out
}

pub fn same_path(a: &Path, b: &Path) -> bool {
    let n = |p: &Path| p.to_string_lossy().to_lowercase().replace('/', "\\").trim_end_matches('\\').to_string();
    n(a) == n(b)
}

/// Steam install folders from the registry (`reg query`, no extra dependency) and the default one.
pub fn steam_roots() -> Vec<PathBuf> {
    let mut out = Vec::new();
    for (key, val) in [(r"HKCU\Software\Valve\Steam", "SteamPath"), (r"HKLM\SOFTWARE\WOW6432Node\Valve\Steam", "InstallPath"), (r"HKLM\SOFTWARE\Valve\Steam", "InstallPath")] {
        if let Ok(o) = crate::no_window(std::process::Command::new("reg").args(["query", key, "/v", val])).output() {
            let text = String::from_utf8_lossy(&o.stdout);
            for line in text.lines() {
                if let Some(i) = line.find("REG_SZ") {
                    let v = line[i + 6..].trim();
                    if !v.is_empty() {
                        out.push(PathBuf::from(v.replace('/', "\\")));
                    }
                }
            }
        }
    }
    out.push(PathBuf::from(r"C:\Program Files (x86)\Steam"));
    out
}

/// Every `"path"` value of `libraryfolders.vdf`.
pub fn vdf_paths(text: &str) -> Vec<PathBuf> {
    text.lines().filter_map(|l| vdf_pair(l).filter(|(k, _)| k.eq_ignore_ascii_case("path")).map(|(_, v)| PathBuf::from(v))).collect()
}

pub fn vdf_value(text: &str, key: &str) -> Option<String> {
    text.lines().filter_map(vdf_pair).find(|(k, _)| k.eq_ignore_ascii_case(key)).map(|(_, v)| v)
}

/// `"key"  "value"` (VDF escapes `\\` and `\"`).
fn vdf_pair(line: &str) -> Option<(String, String)> {
    let mut parts = Vec::new();
    let mut cur = String::new();
    let mut inq = false;
    let mut chars = line.trim().chars();
    while let Some(c) = chars.next() {
        match (inq, c) {
            (false, '"') => inq = true,
            (true, '"') => {
                parts.push(std::mem::take(&mut cur));
                inq = false;
            }
            (true, '\\') => {
                if let Some(n) = chars.next() {
                    cur.push(n);
                }
            }
            (true, c) => cur.push(c),
            _ => {}
        }
    }
    (parts.len() == 2).then(|| (parts[0].clone(), parts[1].clone()))
}

/// SHA-1 of `nie.exe` (hex, lower case).
pub fn nie_sha1(game: &Path) -> Result<String, String> {
    use sha1::{Digest, Sha1};
    use std::io::Read;
    let p = game.join("nie.exe");
    let mut f = std::fs::File::open(&p).map_err(|e| format!("{}: {e}", p.display()))?;
    let mut h = Sha1::new();
    let mut buf = vec![0u8; 1 << 20];
    loop {
        let n = f.read(&mut buf).map_err(|e| e.to_string())?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
    }
    Ok(h.finalize().iter().map(|b| format!("{b:02x}")).collect())
}

/// The game is v7.1.2 (`Ok(false)` = another version).
pub fn version_ok(game: &Path) -> Result<bool, String> {
    Ok(nie_sha1(game)? == V712_SHA1)
}

/// `nie.exe` is running (`tasklist`).
pub fn game_running() -> bool {
    let Ok(o) = crate::no_window(std::process::Command::new("tasklist").args(["/FI", "IMAGENAME eq nie.exe", "/NH", "/FO", "CSV"])).output() else {
        return false;
    };
    String::from_utf8_lossy(&o.stdout).to_lowercase().contains("\"nie.exe\"")
}

/// UVR's `D3DCOMPILER_47.dll` proxy in the game root (93,941 B; the retail game has no such file there).
pub fn uvr_proxy(game: &Path) -> bool {
    std::fs::metadata(game.join("D3DCOMPILER_47.dll")).is_ok_and(|m| m.len() < 1 << 20)
}

/// Backup folder of an install made by the EVT editor app on this game (`%APPDATA%\com.expandedvictory.tool`).
pub fn app_install(game: &Path) -> Option<PathBuf> {
    let appdata = std::env::var_os("APPDATA")?;
    let root = PathBuf::from(appdata).join("com.expandedvictory.tool").join("backups");
    let dir = vr_gamefiles::install::backup_dir_for(&root, game);
    dir.join("manifest.json").is_file().then_some(dir)
}

#[cfg(test)]
mod tests {
    #[test]
    fn vdf() {
        let t = "\"libraryfolders\"\n{\n\t\"0\"\n\t{\n\t\t\"path\"\t\t\"C:\\\\Program Files (x86)\\\\Steam\"\n\t}\n\t\"1\"\n\t{\n\t\t\"path\"\t\t\"D:\\\\SteamLibrary\"\n\t\t\"apps\"\n\t}\n}";
        let p = super::vdf_paths(t);
        assert_eq!(p, vec![std::path::PathBuf::from(r"C:\Program Files (x86)\Steam"), std::path::PathBuf::from(r"D:\SteamLibrary")]);
        assert_eq!(super::vdf_value("\t\"installdir\"\t\t\"INAZUMA ELEVEN Victory Road\"", "installdir").as_deref(), Some("INAZUMA ELEVEN Victory Road"));
    }
}
