//! The ModLoader itself (`winmm.dll` + `evt_loader\` next to `nie.exe`): status, install / update / remove through
//! `evt-installer`'s component install (`<game>\evt_backup\componentes\_modloader\`: what it replaced is backed up
//! and put back on removal, the player's `evt_loader\config.toml` is kept on update).
//!
//! The ModLoader files come from a **payload** (first found wins):
//! 1. built into the exe (`VRML_LOADER_PAYLOAD=<zip>` when building, see `build.rs`);
//! 2. `modloader\` or `modloader.zip` next to `VR-ModLoader.exe`;
//! 3. a zip / folder the player picks («Install ModLoader from file…»).
//!
//! A payload is either a release pack (`pack.toml` with `[cargador] version` + `cargador\**`) or a plain folder:
//! `winmm.dll`, optional `vr_loader.pdb`, `steam_appid.txt`, `evt_loader\**`, and `modloader.toml`
//! (`version = "1.0.0"`; without it the version marker inside the DLL is used).

use std::path::{Path, PathBuf};

use evt_installer::loadercfg;
use evt_installer::modops::{self, LOADER_ID};
use evt_installer::modpack::{self, LoaderAction, LoaderMeta, LoaderState, ModPack, ModPackMeta, FORMAT_MODS, LOADER_DIR};
use evt_installer::pack::LOADER_CONFIG;

/// Files of a plain payload (anything else in its root is ignored).
const PLAIN_FILES: &[&str] = &["winmm.dll", "vr_loader.pdb", "steam_appid.txt"];
const PLAIN_DIRS: &[&str] = &["evt_loader"];
pub const PAYLOAD_META: &str = "modloader.toml";

static EMBEDDED: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/loader_payload.zip"));

pub struct Payload {
    pub version: String,
    /// Where it came from (shown in the UI).
    pub source: String,
    pub pack: ModPack,
}

impl Payload {
    /// A copy for a worker thread (the files stay where they are).
    pub fn duplicate(&self) -> Payload {
        let p = &self.pack;
        let pack = ModPack {
            root: p.root.clone(),
            meta: p.meta.clone(),
            files: p.files.clone(),
            loader_files: p.loader_files.clone(),
            loader_bytes: p.loader_bytes,
            mods: p.mods.clone(),
            modules: p.modules.clone(),
        };
        Payload { version: self.version.clone(), source: self.source.clone(), pack }
    }
}

impl std::fmt::Debug for Payload {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Payload({} from {}, {} files)", self.version, self.source, self.pack.loader_files.len())
    }
}

fn walk(root: &Path) -> Vec<String> {
    let mut out = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(d) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&d) else { continue };
        for e in rd.flatten() {
            let p = e.path();
            if p.is_dir() {
                stack.push(p);
            } else if let Ok(rel) = p.strip_prefix(root) {
                out.push(rel.to_string_lossy().replace('\\', "/"));
            }
        }
    }
    out.sort();
    out
}

fn copy_tree(from: &Path, to: &Path) -> Result<(), String> {
    for rel in walk(from) {
        evt_installer::copy_file(&from.join(&rel), &to.join(&rel))?;
    }
    Ok(())
}

fn meta_version(dir: &Path) -> Option<String> {
    #[derive(serde::Deserialize)]
    struct M {
        version: String,
    }
    let t = std::fs::read_to_string(dir.join(PAYLOAD_META)).ok()?;
    toml::from_str::<M>(&t).ok().map(|m| m.version.trim().to_string()).filter(|v| !v.is_empty())
}

/// A payload from a folder. `work` = an empty scratch folder (a plain layout is copied into `work\cargador\`).
pub fn from_dir(dir: &Path, work: &Path, source: &str) -> Result<Payload, String> {
    let pack_version = std::fs::read_to_string(dir.join(evt_installer::pack::META_FILE))
        .ok()
        .and_then(|t| modpack::parse_meta(&t).ok())
        .map(|m| m.cargador.version)
        .filter(|v| !v.is_empty());
    let (root, dll) = if dir.join(LOADER_DIR).join("winmm.dll").is_file() {
        (dir.to_path_buf(), dir.join(LOADER_DIR).join("winmm.dll"))
    } else if dir.join("winmm.dll").is_file() {
        let car = work.join(LOADER_DIR);
        for f in PLAIN_FILES {
            if dir.join(f).is_file() {
                evt_installer::copy_file(&dir.join(f), &car.join(f))?;
            }
        }
        for d in PLAIN_DIRS {
            if dir.join(d).is_dir() {
                copy_tree(&dir.join(d), &car.join(d))?;
            }
        }
        (work.to_path_buf(), car.join("winmm.dll"))
    } else {
        return Err(format!("{}: no winmm.dll (not a ModLoader package)", dir.display()));
    };
    let version = pack_version
        .or_else(|| meta_version(dir))
        .or_else(|| std::fs::read(&dll).ok().and_then(|b| modpack::scan_version_marker(&b)))
        .ok_or_else(|| format!("{}: unknown ModLoader version (add {PAYLOAD_META} with version = \"x.y.z\")", dir.display()))?;
    let car = root.join(LOADER_DIR);
    let loader_files: Vec<String> = walk(&car);
    let loader_bytes = loader_files.iter().filter_map(|f| std::fs::metadata(car.join(f)).ok()).map(|m| m.len()).sum();
    let meta = ModPackMeta {
        producto: crate::APP_NAME.into(),
        version: version.clone(),
        formato: FORMAT_MODS,
        cargador: LoaderMeta { version: version.clone() },
        ..ModPackMeta::default()
    };
    let pack = ModPack { root, meta, files: Vec::new(), loader_files, loader_bytes, mods: Vec::new(), modules: Vec::new() };
    Ok(Payload { version, source: source.to_string(), pack })
}

/// A payload from a zip (extracted into `work`).
pub fn from_zip(zip: &Path, work: &Path, source: &str) -> Result<Payload, String> {
    let x = work.join("x");
    crate::archive::extract_zip(zip, &x)?;
    // a zip holding one top folder: use it
    let mut dir = x.clone();
    if !dir.join("winmm.dll").is_file() && !dir.join(LOADER_DIR).is_dir() {
        let subs: Vec<PathBuf> = std::fs::read_dir(&x).map(|rd| rd.flatten().map(|e| e.path()).filter(|p| p.is_dir()).collect()).unwrap_or_default();
        if subs.len() == 1 {
            dir = subs[0].clone();
        }
    }
    from_dir(&dir, &work.join("p"), source)
}

/// A payload from a zip or a folder the player picked.
pub fn from_path(p: &Path) -> Result<Payload, String> {
    let work = crate::temp_dir("loader")?;
    let src = p.display().to_string();
    if p.is_dir() {
        from_dir(p, &work, &src)
    } else {
        from_zip(p, &work, &src)
    }
}

/// The payload built into the exe, then the one next to it. None = none found.
pub fn find_payload(exe_dir: &Path) -> Option<Result<Payload, String>> {
    if !EMBEDDED.is_empty() {
        let r = crate::temp_dir("loader").and_then(|work| {
            let z = work.join("embedded.zip");
            std::fs::write(&z, EMBEDDED).map_err(|e| e.to_string())?;
            from_zip(&z, &work, "built-in")
        });
        return Some(r);
    }
    let dir = exe_dir.join("modloader");
    if dir.is_dir() {
        return Some(crate::temp_dir("loader").and_then(|w| from_dir(&dir, &w, &dir.display().to_string())));
    }
    let zip = exe_dir.join("modloader.zip");
    if zip.is_file() {
        return Some(from_path(&zip));
    }
    None
}

// ---------------------------------------------------------------- status

/// The game folder when `VR-ModLoader.exe` sits in it (next to `nie.exe`): the release layout, that folder IS the
/// game. None = the exe is elsewhere (the manager falls back to the saved folder / Steam and warns).
pub fn exe_game_dir(exe: &Path) -> Option<PathBuf> {
    exe.parent().filter(|d| evt_installer::game::is_game_dir(d)).map(Path::to_path_buf)
}

#[derive(Debug, Clone, PartialEq)]
pub struct Status {
    pub state: LoaderState,
    /// This manager can remove it: installed by evt-installer / this manager (removed with its backup), or a
    /// `winmm.dll` with our version marker unzipped from the release (see `unpacked`).
    pub ours: bool,
    /// Our `winmm.dll` (version marker) with no install record: the release zip unpacked into the game folder.
    /// Repair / update write the payload in place; removal deletes the ModLoader files.
    pub unpacked: bool,
    /// `[modules] mods` of `evt_loader\config.toml` (None = no config yet / no such line).
    pub mods_module: Option<bool>,
}

impl Status {
    pub fn installed(&self) -> bool {
        self.state != LoaderState::Missing
    }
    pub fn version(&self) -> Option<&str> {
        match &self.state {
            LoaderState::Known { version, .. } => Some(version),
            _ => None,
        }
    }
    /// A newer payload is available.
    pub fn update_available(&self, payload: &str) -> bool {
        match &self.state {
            LoaderState::Missing => false,
            LoaderState::Known { version, .. } => evt_modfmt::compare_versions(version, payload).is_lt(),
            LoaderState::Unknown { .. } => true,
        }
    }
}

pub fn status(game: &Path) -> Status {
    let state = modops::loader_state(game);
    let component = modops::find_components(game).iter().any(|c| c.is_loader());
    let unpacked = !component && matches!(state, LoaderState::Known { .. });
    let mods_module = std::fs::read_to_string(game.join(LOADER_CONFIG))
        .ok()
        .and_then(|t| loadercfg::modules_of(&t).into_iter().find(|(n, _)| n == "mods").map(|(_, v)| v));
    Status { state, ours: component || unpacked, unpacked, mods_module }
}

/// Install, update or repair the ModLoader from `payload`, then make sure `[modules] mods = true`. Ok(true) = files
/// written, Ok(false) = nothing to do (every file already matches the payload).
/// * nothing / someone else's `winmm.dll`: installed as a component (what it replaces is backed up);
/// * ours, older: updated (a component in place with its backup; an unpacked release file by file);
/// * ours, same version: repaired (files that differ from the payload are written again).
///
/// `evt_loader\config.toml` of the player is never overwritten. A newer installed ModLoader is refused.
pub fn install(game: &Path, payload: &Payload) -> Result<bool, String> {
    let st = status(game);
    let installed_newer = st.version().is_some_and(|v| evt_modfmt::compare_versions(v, &payload.version).is_gt());
    if installed_newer {
        return Err(format!("the installed ModLoader ({}) is newer than this package ({})", st.version().unwrap_or_default(), payload.version));
    }
    let in_place = st.unpacked || (st.ours && !st.update_available(&payload.version));
    let wrote = if in_place {
        write_in_place(game, payload)?
    } else {
        let (action, _) = modops::install_loader_only(game, &payload.pack, true, &mut |_| {})?;
        matches!(action, LoaderAction::Install(_))
    };
    if status(game).mods_module == Some(false) {
        enable_mods_module(game)?;
    }
    Ok(wrote)
}

/// The payload's files over the game's where they differ (not an existing `evt_loader\config.toml`).
fn write_in_place(game: &Path, payload: &Payload) -> Result<bool, String> {
    let src = payload.pack.root.join(LOADER_DIR);
    let mut wrote = false;
    for rel in &payload.pack.loader_files {
        let dst = game.join(rel);
        if rel.eq_ignore_ascii_case(LOADER_CONFIG) && dst.is_file() {
            continue;
        }
        let new = std::fs::read(src.join(rel)).map_err(|e| format!("{}: {e}", src.join(rel).display()))?;
        if std::fs::read(&dst).ok().as_deref() != Some(new.as_slice()) {
            evt_installer::copy_file(&src.join(rel), &dst)?;
            wrote = true;
        }
    }
    Ok(wrote)
}

/// `[modules] mods = true` in `evt_loader\config.toml` (tracked by our ModLoader component when there is one).
pub fn enable_mods_module(game: &Path) -> Result<(), String> {
    let switch = [("mods".to_string(), true)];
    if let Some(c) = modops::find_components(game).into_iter().find(|c| c.is_loader()) {
        modops::apply_modules(game, &c, &switch)?;
        return Ok(());
    }
    let p = game.join(LOADER_CONFIG);
    let text = std::fs::read_to_string(&p).map_err(|e| format!("{}: {e}", p.display()))?;
    let (new, _) = loadercfg::apply_modules(&text, &switch)?;
    if new != text {
        evt_modfmt::write_atomic(&p, &new)?;
    }
    Ok(())
}

/// Files the ModLoader puts in the game folder (an unpacked release: removed by [`remove`]).
const LOADER_FILES: &[&str] = &["winmm.dll", "vr_loader.pdb"];

/// Remove the ModLoader: one we installed goes with its backup (what it replaced is put back); an unpacked release
/// (our `winmm.dll`, no record) is deleted: `winmm.dll`, `vr_loader.pdb`, `steam_appid.txt` when it holds the game's
/// id and `evt_loader\` (settings, logs, caches). Refused when mods installed by the release installer (components)
/// are left; plain `mods\<id>` folders stay and are simply not loaded.
pub fn remove(game: &Path) -> Result<(), String> {
    if modops::uninstall_loader(game, &mut |_| {})?.is_some() {
        return Ok(());
    }
    if !status(game).unpacked {
        return Err(format!("{LOADER_ID}: not installed by this program"));
    }
    for f in LOADER_FILES {
        let p = game.join(f);
        if p.is_file() {
            std::fs::remove_file(&p).map_err(|e| format!("{}: {e}", p.display()))?;
        }
    }
    let appid = game.join("steam_appid.txt");
    if std::fs::read_to_string(&appid).is_ok_and(|t| t.trim() == crate::STEAM_APPID) {
        let _ = std::fs::remove_file(&appid);
    }
    let dir = game.join("evt_loader");
    if dir.is_dir() {
        std::fs::remove_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    }
    Ok(())
}

/// Delete `exe` a few seconds after this process exits (a hidden `cmd` waits, then deletes it): «Remove ModLoader»
/// on request also removes the manager from the game folder. The caller closes the window right after.
pub fn delete_after_exit(exe: &Path) -> Result<(), String> {
    let mut c = std::process::Command::new("cmd.exe");
    #[cfg(windows)]
    {
        // raw: cmd.exe does not understand the backslash escaping of Command::arg
        use std::os::windows::process::CommandExt;
        c.raw_arg(self_delete_command(exe));
    }
    evt_installer::no_window(&mut c).spawn().map(|_| ()).map_err(|e| e.to_string())
}

fn self_delete_command(exe: &Path) -> String {
    format!("/C ping -n 4 127.0.0.1 >nul & del /f /q \"{}\"", exe.display())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fake_game(d: &Path) -> PathBuf {
        let g = d.join("game");
        std::fs::create_dir_all(g.join("data")).unwrap();
        std::fs::write(g.join("nie.exe"), "not the real exe").unwrap();
        std::fs::write(g.join("data").join("cpk_list.cfg.bin"), "list").unwrap();
        g
    }

    fn plain_payload(d: &Path, version: &str, dll: &str) -> PathBuf {
        let p = d.join(format!("payload_{version}"));
        std::fs::create_dir_all(p.join("evt_loader")).unwrap();
        std::fs::write(p.join("winmm.dll"), dll).unwrap();
        std::fs::write(p.join("steam_appid.txt"), "2799860\n").unwrap();
        std::fs::write(p.join("evt_loader").join("config.toml"), "[loader]\nlog_level = \"info\"\n\n[modules]\nmods = false\n").unwrap();
        std::fs::write(p.join(PAYLOAD_META), format!("version = \"{version}\"\n")).unwrap();
        std::fs::write(p.join("README.txt"), "ignored").unwrap();
        p
    }

    #[test]
    fn install_update_remove_round_trip() {
        let d = crate::temp_dir("loader_rt").unwrap();
        let game = fake_game(&d);
        // someone else's winmm.dll was there: backed up and put back
        std::fs::write(game.join("winmm.dll"), "other proxy").unwrap();
        assert_eq!(status(&game).state, LoaderState::Unknown { ours: false });
        let p1 = from_path(&plain_payload(&d, "1.0.0", "dll v1")).unwrap();
        assert_eq!(p1.version, "1.0.0");
        assert_eq!(p1.pack.loader_files, vec!["evt_loader/config.toml", "steam_appid.txt", "winmm.dll"]);
        assert!(install(&game, &p1).unwrap());
        let st = status(&game);
        assert_eq!(st.version(), Some("1.0.0"));
        assert!(st.ours);
        assert_eq!(st.mods_module, Some(true), "the mods module is switched on");
        assert_eq!(std::fs::read_to_string(game.join("winmm.dll")).unwrap(), "dll v1");
        // same version again: nothing to do
        assert!(!install(&game, &p1).unwrap());
        // the player changes a setting; the update keeps config.toml
        let cfg = game.join(LOADER_CONFIG);
        let mine = std::fs::read_to_string(&cfg).unwrap().replace("info", "debug");
        std::fs::write(&cfg, &mine).unwrap();
        let p2 = from_path(&plain_payload(&d, "1.1.0", "dll v2")).unwrap();
        assert!(st.update_available(&p2.version));
        assert!(install(&game, &p2).unwrap());
        assert_eq!(status(&game).version(), Some("1.1.0"));
        assert_eq!(std::fs::read_to_string(game.join("winmm.dll")).unwrap(), "dll v2");
        assert_eq!(std::fs::read_to_string(&cfg).unwrap(), mine);
        // remove: the other proxy is back, evt_loader and the backup folder are gone
        remove(&game).unwrap();
        assert_eq!(std::fs::read_to_string(game.join("winmm.dll")).unwrap(), "other proxy");
        assert!(!game.join("evt_loader").exists());
        assert!(!game.join("evt_backup").exists());
        assert!(remove(&game).is_err());
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn unpacked_release_repair_update_remove() {
        let d = crate::temp_dir("loader_unpacked").unwrap();
        let game = fake_game(&d);
        // the release zip unpacked into the game folder: our winmm.dll, no install record
        let dll1 = "MZ....EVT_MODLOADER_VERSION=1.0.0\0....";
        std::fs::write(game.join("winmm.dll"), dll1).unwrap();
        std::fs::create_dir_all(game.join("mods").join("some_mod")).unwrap();
        let st = status(&game);
        assert_eq!(st.version(), Some("1.0.0"), "the version comes from the marker in the DLL");
        assert!(st.unpacked && st.ours, "removable by the manager");
        let p1 = from_path(&plain_payload(&d, "1.0.0", dll1)).unwrap();
        assert!(!st.update_available(&p1.version), "same version: up to date");
        // repair: the missing files are written, winmm.dll already matches
        assert!(install(&game, &p1).unwrap());
        assert_eq!(std::fs::read_to_string(game.join("steam_appid.txt")).unwrap(), "2799860\n");
        assert!(!game.join("evt_backup").exists(), "an unpacked release is repaired in place, no component");
        assert!(!install(&game, &p1).unwrap(), "nothing left to repair");
        // a damaged DLL (same marker, other bytes) is written again; the player's config.toml stays
        std::fs::write(game.join("winmm.dll"), "MZ..damaged..EVT_MODLOADER_VERSION=1.0.0\0").unwrap();
        let cfg = game.join(LOADER_CONFIG);
        std::fs::write(&cfg, "[modules]\nmods = true\nconsole = true\n").unwrap();
        assert!(install(&game, &p1).unwrap());
        assert_eq!(std::fs::read_to_string(game.join("winmm.dll")).unwrap(), dll1);
        assert!(std::fs::read_to_string(&cfg).unwrap().contains("console = true"));
        // update in place
        let dll2 = "MZ....EVT_MODLOADER_VERSION=1.1.0\0....";
        let p2 = from_path(&plain_payload(&d, "1.1.0", dll2)).unwrap();
        assert!(status(&game).update_available(&p2.version));
        assert!(install(&game, &p2).unwrap());
        assert_eq!(status(&game).version(), Some("1.1.0"));
        // an older package never downgrades
        assert!(install(&game, &p1).is_err());
        // remove: the ModLoader files go, the mods folder stays
        std::fs::write(game.join("vr_loader.pdb"), "pdb").unwrap();
        remove(&game).unwrap();
        for f in ["winmm.dll", "vr_loader.pdb", "steam_appid.txt", "evt_loader"] {
            assert!(!game.join(f).exists(), "{f} removed");
        }
        assert!(game.join("mods").join("some_mod").is_dir());
        assert_eq!(status(&game).state, LoaderState::Missing);
        assert!(remove(&game).is_err());
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn someone_elses_dll_is_not_removed_by_file() {
        let d = crate::temp_dir("loader_foreign").unwrap();
        let game = fake_game(&d);
        std::fs::write(game.join("winmm.dll"), "another proxy, no marker").unwrap();
        let st = status(&game);
        assert!(!st.unpacked && !st.ours);
        assert!(remove(&game).is_err());
        assert!(game.join("winmm.dll").is_file());
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn exe_in_the_game_folder_is_the_game() {
        let d = crate::temp_dir("loader_exe").unwrap();
        let game = fake_game(&d);
        assert_eq!(exe_game_dir(&game.join("VR-ModLoader.exe")), Some(game.clone()));
        assert_eq!(exe_game_dir(&d.join("VR-ModLoader.exe")), None);
        let cmd = self_delete_command(&game.join("VR-ModLoader.exe"));
        assert!(cmd.starts_with("/C ") && cmd.ends_with(&format!("del /f /q \"{}\"", game.join("VR-ModLoader.exe").display())), "{cmd}");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn payload_from_zip_and_marker() {
        let d = crate::temp_dir("loader_zip").unwrap();
        let z = d.join("ml.zip");
        crate::archive::tests::make_zip(
            &z,
            &[("VR-ModLoader 1.2.0/winmm.dll", "....EVT_MODLOADER_VERSION=1.2.0\0...."), ("VR-ModLoader 1.2.0/evt_loader/lua_patches/x.lua", "--")],
            true,
        );
        let p = from_path(&z).unwrap();
        assert_eq!(p.version, "1.2.0");
        assert_eq!(p.pack.loader_files, vec!["evt_loader/lua_patches/x.lua", "winmm.dll"]);
        let bad = d.join("bad.zip");
        crate::archive::tests::make_zip(&bad, &[("readme.txt", "x")], false);
        assert!(from_path(&bad).is_err());
        let _ = std::fs::remove_dir_all(&d);
    }
}
