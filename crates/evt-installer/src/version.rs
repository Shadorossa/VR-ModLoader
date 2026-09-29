//! Game version check and the semi-automatic way back to v7.1.2 (Steam console `download_depot`).
//!
//! Our code never handles Steam credentials and ships no download tool: the player pastes the command into their
//! own Steam client's console (we open it with `steam://open/console` and put the command on the clipboard), Steam
//! downloads the v7.1.2 depot into `<Steam>\steamapps\content\app_<appid>\depot_<depot>\`, and then
//! [`merge_depot`] puts those files over the game folder (moved when on the same drive, otherwise only the files that
//! differ are copied). Guide: docs/app/instalador-release.md §4.

use std::io::Read;
use std::path::{Path, PathBuf};

use crate::game;
use crate::pack::GameBuild;

/// What the game folder says about its version.
#[derive(Debug, Clone, Default)]
pub struct VersionState {
    pub nie_sha1: String,
    /// `nie.exe` is the v7.1.2 one (the decisive check: the loader refuses any other exe).
    pub nie_ok: bool,
    /// `buildid` of `appmanifest_<appid>.acf` (None = no appmanifest found next to the game).
    pub buildid: Option<String>,
    pub auto_update: Option<String>,
    pub acf: Option<PathBuf>,
}

impl VersionState {
    /// Steam's build id says v7.1.2 (or there is no appmanifest to ask).
    pub fn buildid_ok(&self, want: &GameBuild) -> bool {
        want.buildid.is_empty() || self.buildid.as_deref().map_or(true, |b| b == want.buildid)
    }
}

/// `<library>\steamapps\appmanifest_<appid>.acf` of a game in `<library>\steamapps\common\<folder>`.
pub fn acf_path(game_dir: &Path) -> Option<PathBuf> {
    let steamapps = game_dir.parent()?.parent()?;
    let p = steamapps.join(format!("appmanifest_{}.acf", game::APPID));
    p.is_file().then_some(p)
}

pub fn state(game_dir: &Path, want: &GameBuild) -> VersionState {
    let nie_sha1 = game::nie_sha1(game_dir).unwrap_or_default();
    let want_sha = if want.nie_sha1.is_empty() { game::V712_SHA1 } else { want.nie_sha1.as_str() };
    let acf = acf_path(game_dir);
    let text = acf.as_ref().and_then(|p| std::fs::read_to_string(p).ok()).unwrap_or_default();
    VersionState {
        nie_ok: nie_sha1 == want_sha,
        nie_sha1,
        buildid: game::vdf_value(&text, "buildid"),
        auto_update: game::vdf_value(&text, "AutoUpdateBehavior"),
        acf,
    }
}

/// The command the player pastes into the Steam console.
pub fn console_command(b: &GameBuild) -> String {
    format!("download_depot {} {} {}", game::APPID, b.depot, b.manifest)
}

/// Where `download_depot` leaves the files (in the Steam install folder, not in the game's library).
pub fn depot_dir(steam: &Path, b: &GameBuild) -> PathBuf {
    steam.join("steamapps").join("content").join(format!("app_{}", game::APPID)).join(format!("depot_{}", b.depot))
}

/// Steam install folder (registry `SteamPath`, else the default one).
pub fn steam_dir() -> Option<PathBuf> {
    game::steam_roots().into_iter().find(|p| p.join("steam.exe").is_file())
}

/// Open the Steam console (`steam://open/console`).
pub fn open_console() -> Result<(), String> {
    std::process::Command::new("cmd").args(["/C", "start", "", "steam://open/console"]).status().map(|_| ()).map_err(|e| e.to_string())
}

/// Put `text` on the Windows clipboard (`clip.exe`).
pub fn set_clipboard(text: &str) -> Result<(), String> {
    use std::io::Write;
    use std::process::{Command, Stdio};
    let mut c = Command::new("clip").stdin(Stdio::piped()).spawn().map_err(|e| e.to_string())?;
    c.stdin.take().ok_or("clip sin stdin")?.write_all(text.as_bytes()).map_err(|e| e.to_string())?;
    c.wait().map_err(|e| e.to_string())?;
    Ok(())
}

pub fn steam_running() -> bool {
    let Ok(o) = crate::no_window(std::process::Command::new("tasklist").args(["/FI", "IMAGENAME eq steam.exe", "/NH", "/FO", "CSV"])).output() else {
        return false;
    };
    String::from_utf8_lossy(&o.stdout).to_lowercase().contains("\"steam.exe\"")
}

/// Free bytes of the drive holding `dir` (PowerShell `Get-PSDrive`; None when unknown).
pub fn free_space(dir: &Path) -> Option<u64> {
    let s = dir.to_string_lossy();
    let letter = s.chars().next().filter(|c| c.is_ascii_alphabetic() && s.chars().nth(1) == Some(':'))?;
    let o = std::process::Command::new("powershell")
        .args(["-NoProfile", "-Command", &format!("(Get-PSDrive -Name {letter}).Free")])
        .output()
        .ok()?;
    String::from_utf8_lossy(&o.stdout).trim().parse().ok()
}

/// Files and bytes under `dir`.
pub fn folder_size(dir: &Path) -> (usize, u64) {
    let (mut n, mut b) = (0, 0);
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&d) else { continue };
        for e in rd.flatten() {
            match e.file_type() {
                Ok(t) if t.is_dir() => stack.push(e.path()),
                Ok(t) if t.is_file() => {
                    n += 1;
                    b += e.metadata().map(|m| m.len()).unwrap_or(0);
                }
                _ => {}
            }
        }
    }
    (n, b)
}

/// The downloaded depot looks complete: its `nie.exe` is the v7.1.2 one and it has the depot's bytes.
pub fn depot_ready(depot: &Path, want: &GameBuild) -> bool {
    let want_sha = if want.nie_sha1.is_empty() { game::V712_SHA1 } else { want.nie_sha1.as_str() };
    if game::nie_sha1(depot).ok().as_deref() != Some(want_sha) {
        return false;
    }
    want.depot_bytes == 0 || folder_size(depot).1 >= want.depot_bytes
}

#[derive(Debug, Default)]
pub struct MergeReport {
    pub files: usize,
    /// Files put into the game (missing or different there).
    pub replaced: usize,
    pub replaced_bytes: u64,
    /// Identical files left as they were.
    pub same: usize,
    /// Moved instead of copied (depot and game on the same drive).
    pub moved: bool,
}

fn same_drive(a: &Path, b: &Path) -> bool {
    let root = |p: &Path| p.components().next().map(|c| c.as_os_str().to_string_lossy().to_lowercase());
    root(a).is_some() && root(a) == root(b)
}

fn same_content(a: &Path, b: &Path) -> std::io::Result<bool> {
    let (mut fa, mut fb) = (std::fs::File::open(a)?, std::fs::File::open(b)?);
    let (mut ba, mut bb) = (vec![0u8; 4 << 20], vec![0u8; 4 << 20]);
    loop {
        let na = read_full(&mut fa, &mut ba)?;
        let nb = read_full(&mut fb, &mut bb)?;
        if na != nb || ba[..na] != bb[..nb] {
            return Ok(false);
        }
        if na == 0 {
            return Ok(true);
        }
    }
}

fn read_full(f: &mut std::fs::File, buf: &mut [u8]) -> std::io::Result<usize> {
    let mut n = 0;
    while n < buf.len() {
        let r = f.read(&mut buf[n..])?;
        if r == 0 {
            break;
        }
        n += r;
    }
    Ok(n)
}

/// Put the downloaded depot over the game folder. Same drive: every file is moved (instant, the depot folder is
/// emptied). Other drive: a file is copied only when the game's copy is missing, of another size or of other
/// content. Files the game has and the depot lacks are left alone (a newer build's extra files are unused by the
/// v7.1.2 `cpk_list`). `allow_move = false` forces the compare-and-copy path. `progress(done, total)` per file.
pub fn merge_depot(depot: &Path, game_dir: &Path, allow_move: bool, progress: &mut dyn FnMut(u64, u64)) -> Result<MergeReport, String> {
    let mut files: Vec<(PathBuf, u64)> = Vec::new();
    let mut stack = vec![depot.to_path_buf()];
    while let Some(d) = stack.pop() {
        for e in std::fs::read_dir(&d).map_err(|e| format!("{}: {e}", d.display()))?.flatten() {
            let t = e.file_type().map_err(|e| e.to_string())?;
            if t.is_dir() {
                stack.push(e.path());
            } else if t.is_file() {
                files.push((e.path(), e.metadata().map(|m| m.len()).unwrap_or(0)));
            }
        }
    }
    files.sort();
    let total: u64 = files.iter().map(|f| f.1).sum();
    let moved = allow_move && same_drive(depot, game_dir);
    let mut r = MergeReport { files: files.len(), moved, ..MergeReport::default() };
    let mut done = 0u64;
    for (src, size) in files {
        let rel = src.strip_prefix(depot).unwrap();
        let dst = game_dir.join(rel);
        if let Some(d) = dst.parent() {
            std::fs::create_dir_all(d).map_err(|e| format!("{}: {e}", d.display()))?;
        }
        let differs = match std::fs::metadata(&dst) {
            Ok(m) if m.len() == size => !same_content(&src, &dst).map_err(|e| format!("{}: {e}", dst.display()))?,
            _ => true,
        };
        if differs || moved {
            if moved && std::fs::rename(&src, &dst).is_ok() {
                // done
            } else {
                let tmp = dst.with_extension("evt_tmp");
                std::fs::copy(&src, &tmp).map_err(|e| format!("{} → {}: {e}", src.display(), tmp.display()))?;
                std::fs::rename(&tmp, &dst).map_err(|e| format!("{}: {e}", dst.display()))?;
            }
        }
        if differs {
            r.replaced += 1;
            r.replaced_bytes += size;
        } else {
            r.same += 1;
        }
        done += size;
        progress(done, total);
    }
    Ok(r)
}

/// Set `AutoUpdateBehavior` to 1 («only update this game when I launch it»). Only while Steam is closed: Steam
/// rewrites the appmanifest while it runs. Returns whether the file changed.
pub fn set_update_on_launch(acf: &Path) -> Result<bool, String> {
    let text = std::fs::read_to_string(acf).map_err(|e| format!("{}: {e}", acf.display()))?;
    let mut changed = false;
    let mut out = String::with_capacity(text.len());
    for line in text.split_inclusive('\n') {
        if line.trim_start().starts_with("\"AutoUpdateBehavior\"") && !line.contains("\"1\"") {
            let indent: String = line.chars().take_while(|c| c.is_whitespace()).collect();
            let eol = if line.ends_with("\r\n") { "\r\n" } else if line.ends_with('\n') { "\n" } else { "" };
            out.push_str(&format!("{indent}\"AutoUpdateBehavior\"\t\t\"1\"{eol}"));
            changed = true;
        } else {
            out.push_str(line);
        }
    }
    if changed {
        std::fs::write(acf, out).map_err(|e| format!("{}: {e}", acf.display()))?;
    }
    Ok(changed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn update_on_launch() {
        let dir = std::env::temp_dir().join(format!("evt_acf_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("a.acf");
        std::fs::write(&p, "\"AppState\"\r\n{\r\n\t\"buildid\"\t\t\"1\"\r\n\t\"AutoUpdateBehavior\"\t\t\"0\"\r\n}\r\n").unwrap();
        assert!(set_update_on_launch(&p).unwrap());
        assert_eq!(std::fs::read_to_string(&p).unwrap(), "\"AppState\"\r\n{\r\n\t\"buildid\"\t\t\"1\"\r\n\t\"AutoUpdateBehavior\"\t\t\"1\"\r\n}\r\n");
        assert!(!set_update_on_launch(&p).unwrap());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
