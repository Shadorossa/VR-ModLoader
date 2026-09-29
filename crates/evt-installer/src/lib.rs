//! Install library of VR-ModLoader (used by `VR-ModLoader.exe`, crate `vr-modloader-app`) for a player's own copy of
//! Inazuma Eleven Victory Road v7.1.2.
//!
//! * [`modpack`] / [`modops`]: the mods layout: the ModLoader itself when missing or too old, then each mod into
//!   `<game>\mods\<id>\` with its own backup, so mods can be uninstalled one by one.
//! * [`ops`] / [`pack`] / [`delta`]: single-pack installs: files that replace a retail file ship as binary deltas
//!   applied to the player's own retail file; everything overwritten is backed up first into
//!   `<game>\evt_backup\<stamp>\` (`evt_install.json` = what was done), the data goes in through
//!   `vr_gamefiles::install`, and uninstall puts every file back and the original `cpk_list` bytes.
//! * [`game`] / [`version`]: game folder detection, v7.1.2 check, bringing the game to v7.1.2 with the Steam console.

pub mod delta;
pub mod game;
pub mod loadercfg;
pub mod modops;
pub mod modpack;
pub mod ops;
pub mod pack;
pub mod version;

use std::io::Read;
use std::path::Path;

/// Product name shown to the player.
pub const PRODUCT: &str = "VR-ModLoader pack";
/// Folder of the game install that holds our backups.
pub const BACKUP_DIR: &str = "evt_backup";
/// Record of one install inside its backup folder.
pub const RECORD_FILE: &str = "evt_install.json";

/// Size and CRC-32 of a file (streamed).
pub fn file_crc(p: &Path) -> std::io::Result<(u64, u32)> {
    let mut f = std::fs::File::open(p)?;
    let mut h = crc32fast::Hasher::new();
    let mut buf = vec![0u8; 1 << 20];
    let mut n = 0u64;
    loop {
        let r = f.read(&mut buf)?;
        if r == 0 {
            break;
        }
        h.update(&buf[..r]);
        n += r as u64;
    }
    Ok((n, h.finalize()))
}

/// Copy a file, creating the destination folder.
pub fn copy_file(from: &Path, to: &Path) -> Result<(), String> {
    if let Some(d) = to.parent() {
        std::fs::create_dir_all(d).map_err(|e| format!("{}: {e}", d.display()))?;
    }
    std::fs::copy(from, to).map(|_| ()).map_err(|e| format!("{} → {}: {e}", from.display(), to.display()))
}

/// `YYYYMMDD-HHMMSS` (UTC) of a Unix time.
pub fn stamp(secs: u64) -> String {
    let days = (secs / 86_400) as i64;
    let rem = secs % 86_400;
    // days → civil date (H. Hinnant's algorithm)
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + if m <= 2 { 1 } else { 0 };
    format!("{y:04}{m:02}{d:02}-{:02}{:02}{:02}", rem / 3600, rem % 3600 / 60, rem % 60)
}

/// A console tool started from a GUI (the VR-ModLoader manager) opens no console window (`CREATE_NO_WINDOW`);
/// output is captured through pipes anyway, so the console installer behaves the same.
pub fn no_window(cmd: &mut std::process::Command) -> &mut std::process::Command {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000);
    }
    cmd
}

pub fn now_secs() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

/// `1.234.567` → human size in MB / GB.
pub fn human_size(n: u64) -> String {
    if n >= 1 << 30 {
        format!("{:.2} GB", n as f64 / (1u64 << 30) as f64)
    } else {
        format!("{:.1} MB", n as f64 / (1u64 << 20) as f64)
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn stamp_is_utc_civil() {
        assert_eq!(super::stamp(0), "19700101-000000");
        // 2026-09-29 12:34:56 UTC
        assert_eq!(super::stamp(1_790_685_296), "20260929-123456");
    }
}
