//! **VR-ModLoader.exe** — the public mod manager of the VR-ModLoader (Inazuma Eleven Victory Road PC v7.1.2).
//! GPL-3.0. Guide (Spanish): `docs/app/modloader-manager.md`; players' summary: `sdk/README.md`.
//!
//! Everything that is not drawing lives in this library so it can be unit tested:
//! * [`model`]: the mod list of `<game>\mods` (enabled / load order in memory, «Save» writes `enabled.toml` +
//!   `load_order.toml`, the same files the ModLoader and its in-game Mods menu read), per-mod status and conflicts
//!   from `evt_modfmt::build_plan_full`; profiles through `evt_modfmt`.
//! * [`archive`] + [`install`]: a mod `.zip` → validated (the `evt-mod check` rules) → `mods\<id>\`; uninstall moves
//!   the folder to `mods\_trash\` (skipped by the loader), nothing is hard-deleted without confirmation.
//! * [`modloader`]: detect / install / update / remove the ModLoader (`winmm.dll` + `evt_loader\`) through
//!   `evt-installer`'s component install (backup of what it replaces).
//! * [`urlscheme`]: `vrmodloader:` 1-click links (GameBanana style), HKCU registration, HTTPS download.
//! * [`audio`]: cue conflicts between the `audio.toml` declarations of the active mods.
//! * [`i18n`]: English UI with a Spanish table.

pub mod archive;
pub mod audio;
pub mod catalog;
pub mod i18n;
pub mod install;
pub mod model;
pub mod modloader;
pub mod settings;
pub mod urlscheme;

use std::path::{Path, PathBuf};

/// Product name shown in the window title and the registry.
pub const APP_NAME: &str = "VR-ModLoader";
/// Version of this manager (not of the ModLoader DLL).
pub const APP_VERSION: &str = env!("CARGO_PKG_VERSION");
/// Steam app id of Inazuma Eleven Victory Road.
pub const STEAM_APPID: &str = evt_installer::game::APPID;

/// `<game>\mods`.
pub fn mods_root(game: &Path) -> PathBuf {
    game.join(evt_modfmt::MODS_DIR)
}

/// Unique scratch folder under `%TEMP%\VR-ModLoader\<what>-<pid>-<n>` (created).
pub fn temp_dir(what: &str) -> Result<PathBuf, String> {
    use std::sync::atomic::{AtomicU32, Ordering};
    static N: AtomicU32 = AtomicU32::new(0);
    let d = std::env::temp_dir()
        .join(APP_NAME)
        .join(format!("{what}-{}-{}-{}", std::process::id(), evt_installer::now_secs(), N.fetch_add(1, Ordering::Relaxed)));
    std::fs::create_dir_all(&d).map_err(|e| format!("{}: {e}", d.display()))?;
    Ok(d)
}

/// Remove what earlier runs left in `%TEMP%\VR-ModLoader` (unpacked ModLoader payloads, downloads) when older
/// than a day (a second window opened by a 1-click link may still be using the recent ones).
pub fn cleanup_temp() {
    let root = std::env::temp_dir().join(APP_NAME);
    let old = |p: &Path| {
        std::fs::metadata(p).and_then(|m| m.modified()).ok().and_then(|t| t.elapsed().ok()).is_some_and(|e| e.as_secs() > 86_400)
    };
    let Ok(rd) = std::fs::read_dir(&root) else { return };
    for e in rd.flatten() {
        let p = e.path();
        if p.file_name().is_some_and(|n| n == "downloads") {
            for f in std::fs::read_dir(&p).into_iter().flatten().flatten() {
                if old(&f.path()) {
                    let _ = std::fs::remove_file(f.path());
                }
            }
        } else if p.is_dir() && old(&p) {
            let _ = std::fs::remove_dir_all(&p);
        }
    }
}

/// Open a folder or URL with the shell (Explorer / the default browser), without a console window.
pub fn shell_open(target: &str) -> Result<(), String> {
    let mut c = std::process::Command::new("explorer.exe");
    c.arg(target);
    evt_installer::no_window(&mut c).spawn().map(|_| ()).map_err(|e| e.to_string())
}

/// Start the game through Steam (`steam://rungameid/<appid>`).
pub fn launch_game() -> Result<(), String> {
    shell_open(&format!("steam://rungameid/{STEAM_APPID}"))
}
