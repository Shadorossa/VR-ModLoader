//! Mods layout (pack format 2): install the ModLoader + 3 mods (one depends on another) into a FAKE game folder
//! (temp dir, as `roundtrip.rs`), uninstall one mod and check exactly its files and data went away, then uninstall
//! everything and check the game is byte for byte what it was. Also: dependencies refused, update of one mod, a
//! wrong retail file, a ModLoader already new enough, an interrupted install. Never touches the real game.
//! Run: cargo test -p evt-installer --release --test mods_roundtrip

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use evt_installer::modops::{self, ModsOptions};
use evt_installer::modpack::ModPack;
use evt_installer::pack::{IndexFile, PackFile, Patch, PatchesFile};
use evt_installer::{delta, file_crc};
use vr_gamefiles::cpk::{read_list, write_list};
use vr_gamefiles::cpk_list::CpkItem;

fn retail_list_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../assets/v7.1.2/data/cpk_list.cfg.bin")
}

fn retail(n: usize) -> Vec<u8> {
    let mut v = Vec::new();
    for k in 0..4000 {
        v.extend_from_slice(format!("retail #{n} line {k} ").as_bytes());
    }
    v
}

fn modded(n: usize) -> Vec<u8> {
    let mut v = retail(n);
    v[100..110].copy_from_slice(b"EVT EDIT! ");
    v.splice(5000..5000, b"inserted by the mod".iter().copied());
    v
}

const XOR_KEY: u32 = 0x1234_5678;

fn xored(mut b: Vec<u8>) -> Vec<u8> {
    l5_cpk::xor::crypt_in_place(XOR_KEY, 0, &mut b);
    b
}

type Tree = BTreeMap<String, Vec<u8>>;

fn tree(root: &Path) -> Tree {
    let mut out = BTreeMap::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(d) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&d) else { continue };
        for e in rd {
            let p = e.unwrap().path();
            if p.is_dir() {
                stack.push(p);
            } else {
                out.insert(p.strip_prefix(root).unwrap().to_string_lossy().replace('\\', "/"), std::fs::read(&p).unwrap());
            }
        }
    }
    out
}

fn dirs(root: &Path) -> Vec<String> {
    let mut out = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(d) = stack.pop() {
        for e in std::fs::read_dir(&d).unwrap() {
            let p = e.unwrap().path();
            if p.is_dir() {
                out.push(p.strip_prefix(root).unwrap().to_string_lossy().replace('\\', "/"));
                stack.push(p);
            }
        }
    }
    out.sort();
    out
}

fn write(p: &Path, b: &[u8]) {
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(p, b).unwrap();
}

fn quiet(_: &str) {}

struct Env {
    root: PathBuf,
    game: PathBuf,
    pack: PathBuf,
    items: Vec<CpkItem>,
}

impl Drop for Env {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

/// Pack variants.
#[derive(Clone, Copy, Default)]
struct Opts {
    fantasy_version: &'static str,
    /// Leave `engine` out of the pack (its dependent then lacks it).
    no_engine: bool,
    /// Deltas made against a file that is not the game's retail one.
    wrong_retail: bool,
}

impl Env {
    fn new(name: &str) -> Env {
        let root = std::env::temp_dir().join(format!("evt_mods_{name}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let game = root.join("game");
        let pack = root.join("pack");
        let mut l = read_list(&retail_list_path()).unwrap();
        let mut items: Vec<CpkItem> = l
            .items
            .iter()
            .filter(|i| i.path().starts_with("data/") && !vr_gamefiles::walk::is_junk(&i.path()) && !i.is_loose())
            .take(40)
            .cloned()
            .collect();
        for (n, it) in items.iter_mut().take(3).enumerate() {
            it.cpk_dir = Some(String::new());
            it.cpk_name = Some(String::new());
            it.size = retail(n).len() as i32;
        }
        l.items = items.clone();
        l.sort();
        std::fs::create_dir_all(game.join("data")).unwrap();
        write_list(&game.join("data/cpk_list.cfg.bin"), &l).unwrap();
        for (n, it) in items.iter().take(3).enumerate() {
            write(&game.join(it.path()), &retail(n));
        }
        write(&game.join("nie.exe"), b"not the real exe");
        write(&game.join("winmm.dll"), b"someone else's proxy dll");
        write(&game.join("mods/other/mod.toml"), b"id = \"other\"\nname = \"Other\"\nversion = \"1.0.0\"\n");
        write(&game.join("mods/enabled.toml"), b"enabled = [\"other\"]\n");
        Env { root, game, pack, items }
    }

    fn item(&self, n: usize) -> String {
        self.items[n].path()
    }

    /// ModLoader 1.0.0 + mods `fantasy` (requires engine>=1.0; delta of retail #1 (XOR), whole retail #2, a new
    /// file, lua + files), `engine` (delta of retail #0, whole CPK file #5, a new file, a DLL) and `quit_fix`
    /// (DLL only). Pack order: fantasy, engine, quit_fix.
    fn make_pack(&self, o: Opts) {
        let p = &self.pack;
        let _ = std::fs::remove_dir_all(p);
        let fv = if o.fantasy_version.is_empty() { "0.2.0" } else { o.fantasy_version };
        let src = |n: usize| {
            let mut r = retail(n);
            if o.wrong_retail {
                r[7] ^= 0x55;
            }
            r
        };
        // ModLoader
        write(&p.join("cargador/winmm.dll"), b"OUR LOADER EVT_MODLOADER_VERSION=1.0.0\0");
        write(&p.join("cargador/steam_appid.txt"), b"2799860\r\n");
        write(&p.join("cargador/evt_loader/lua_patches/main_menu/a.lua"), b"-- global patch");
        write(&p.join("cargador/evt_loader/config.toml"), b"[loader]\r\nlog_level = \"info\"\r\n\r\n[modules]\r\nlua_bridge = true\r\nstats = false\r\n");
        write(&p.join("evt_loader_modules.toml"), b"lua_bridge = true\nstats = true\n");
        // fantasy
        let f = p.join("mods/fantasy");
        write(&f.join("mod/mod.toml"), format!("id = \"fantasy\"\nname = \"Fantasy\"\nversion = \"{fv}\"\nrequires = [\"engine>=1.0\"]\n").as_bytes());
        write(&f.join("mod/lua/title/x.lua"), format!("-- fantasy {fv}").as_bytes());
        write(&f.join("mod/files/data/common/y.bin"), b"served by the loader");
        let r1 = self.item(1);
        write(&f.join(format!("delta/{r1}.evtd")), &delta::diff(&src(1), &modded(1)));
        let fp = vec![Patch { ruta: r1.clone(), delta: format!("delta/{r1}.evtd"), tam: modded(1).len() as u64, xor: Some(XOR_KEY) }];
        std::fs::write(f.join("parches.json"), serde_json::to_string(&PatchesFile { parches: fp }).unwrap()).unwrap();
        write(&f.join(self.item(2)), b"fantasy whole replacement of retail loose #2");
        write(&f.join("data/common/evt_fantasy/new.bin"), format!("fantasy new {fv}").as_bytes());
        // engine
        if !o.no_engine {
            let e = p.join("mods/engine");
            write(&e.join("mod/mod.toml"), b"id = \"engine\"\nname = \"Engine\"\nversion = \"1.2.0\"\nplugin = \"engine.dll\"\nprovides = [\"rules_api=1\"]\n");
            write(&e.join("mod/engine.dll"), b"ENGINE PLUGIN");
            let r0 = self.item(0);
            write(&e.join(format!("delta/{r0}.evtd")), &delta::diff(&src(0), &modded(0)));
            let ep = vec![Patch { ruta: r0.clone(), delta: format!("delta/{r0}.evtd"), tam: modded(0).len() as u64, xor: None }];
            std::fs::write(e.join("parches.json"), serde_json::to_string(&PatchesFile { parches: ep }).unwrap()).unwrap();
            write(&e.join(self.item(5)), b"engine whole replacement of a CPK file");
            write(&e.join("data/common/evt_engine/deep/new.bin"), b"engine new");
        }
        // quit_fix
        write(&p.join("mods/quit_fix/mod/mod.toml"), b"id = \"quit_fix\"\nversion = \"1.0.0\"\nplugin = \"quit_fix.dll\"\nloader_min = \"1.0.0\"\n");
        write(&p.join("mods/quit_fix/mod/quit_fix.dll"), b"QUIT FIX PLUGIN");

        let mut meta = String::from("producto = \"Example Pack\"\nversion = \"0.2.0-test\"\nformato = 2\nloader_min = \"1.0.0\"\n\n[juego]\nbuildid = \"24370575\"\n\n[cargador]\nversion = \"1.0.0\"\n\n");
        meta += &format!("[[mod]]\nid = \"fantasy\"\nversion = \"{fv}\"\nrequires = [\"engine>=1.0\"]\n\n");
        if !o.no_engine {
            meta += "[[mod]]\nid = \"engine\"\nversion = \"1.2.0\"\nplugin = \"engine.dll\"\nprovides = [\"rules_api=1\"]\n\n";
        }
        meta += "[[mod]]\nid = \"quit_fix\"\nversion = \"1.0.0\"\nplugin = \"quit_fix.dll\"\nloader_min = \"1.0.0\"\n";
        std::fs::write(p.join("pack.toml"), meta).unwrap();
        let mut archivos = Vec::new();
        for (rel, _) in tree(p) {
            if rel.starts_with("cargador/") || (rel.starts_with("mods/") && !rel.ends_with("/parches.json")) {
                let (tam, crc) = file_crc(&p.join(&rel)).unwrap();
                archivos.push(PackFile { ruta: rel, tam, crc });
            }
        }
        std::fs::write(p.join("indice.json"), serde_json::to_string(&IndexFile { archivos }).unwrap()).unwrap();
    }

    fn open_pack(&self) -> ModPack {
        assert_eq!(evt_installer::modpack::pack_format(&self.pack).unwrap(), 2);
        let pack = ModPack::open(&self.pack).unwrap();
        assert!(pack.validate().is_empty(), "{:?}", pack.validate());
        assert!(pack.check_files().is_empty());
        pack
    }

    fn entry(&self, rel: &str) -> Option<CpkItem> {
        read_list(&self.game.join("data/cpk_list.cfg.bin")).unwrap().items.into_iter().find(|i| i.path() == rel)
    }

    fn enabled(&self) -> String {
        std::fs::read_to_string(self.game.join("mods/enabled.toml")).unwrap()
    }
}

fn same_as_before(before: &Tree, game: &Path) {
    same_trees(before, &tree(game));
}

fn same_trees(before: &Tree, after: &Tree) {
    assert_eq!(after.keys().collect::<Vec<_>>(), before.keys().collect::<Vec<_>>());
    for (k, v) in before {
        assert!(after[k] == *v, "{k} differs after uninstall");
    }
}

#[test]
fn install_all_uninstall_one_then_all() {
    if !std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../assets/v7.1.2/data/cpk_list.cfg.bin").is_file() {
        eprintln!("assets/v7.1.2/data/cpk_list.cfg.bin (your own retail cpk_list) not found: skipped");
        return;
    }
    let e = Env::new("all");
    e.make_pack(Opts::default());
    let pack = e.open_pack();
    let before = tree(&e.game);
    let before_dirs = dirs(&e.game);

    let prep = modops::prepare(&e.game, &pack, &ModsOptions::default()).unwrap();
    assert_eq!(prep.to_install, vec!["engine", "fantasy", "quit_fix"], "dependencies first");
    let o = modops::install(&e.game, &pack, &ModsOptions::default(), &mut quiet).unwrap();
    assert!(o.loader_installed, "{}", o.loader);
    assert_eq!(o.installed, vec!["engine 1.2.0", "fantasy 0.2.0", "quit_fix 1.0.0"]);
    assert_eq!(o.data_files, 6);
    assert!(!e.pack.join("_montaje").exists());
    // loader
    assert!(std::fs::read(e.game.join("winmm.dll")).unwrap().starts_with(b"OUR LOADER"));
    let cfg = std::fs::read_to_string(e.game.join("evt_loader/config.toml")).unwrap();
    assert!(cfg.contains("stats = true"), "{cfg}");
    assert_eq!(modops::loader_state(&e.game), evt_installer::modpack::LoaderState::Known { version: "1.0.0".into(), ours: true });
    // mods
    assert_eq!(std::fs::read(e.game.join("mods/engine/engine.dll")).unwrap(), b"ENGINE PLUGIN");
    assert!(e.game.join("mods/fantasy/lua/title/x.lua").is_file());
    assert!(e.game.join("mods/quit_fix/quit_fix.dll").is_file());
    assert_eq!(e.enabled(), "enabled = [\"other\", \"engine\", \"fantasy\", \"quit_fix\"]\n");
    // data of both mods
    assert_eq!(std::fs::read(e.game.join(e.item(0))).unwrap(), modded(0));
    assert_eq!(std::fs::read(e.game.join(e.item(1))).unwrap(), xored(modded(1)));
    assert!(e.entry(&e.item(5)).unwrap().is_loose());
    assert!(e.entry("data/common/evt_fantasy/new.bin").is_some() && e.entry("data/common/evt_engine/deep/new.bin").is_some());
    let comps = modops::find_components(&e.game);
    assert_eq!(comps.iter().map(|c| c.record.id.as_str()).collect::<Vec<_>>(), vec!["_modloader", "engine", "fantasy", "quit_fix"]);

    // same pack again: nothing to do
    let again = modops::prepare(&e.game, &pack, &ModsOptions::default()).unwrap();
    assert!(again.to_install.is_empty() && again.unchanged.len() == 3);
    assert!(matches!(again.loader, Some(evt_installer::modpack::LoaderAction::Keep(_))));

    // engine is needed by fantasy; the loader by every mod
    assert!(modops::uninstall_mods(&e.game, &["engine".into()], false, &mut quiet).unwrap_err().contains("fantasy"));
    assert!(modops::uninstall_loader(&e.game, &mut quiet).is_err());

    // play: the loader writes a log and a mod writes its own file
    write(&e.game.join("evt_loader/loader.log"), b"log");

    // uninstall fantasy alone: exactly its files and data go
    let with_all = tree(&e.game);
    let u = modops::uninstall_mods(&e.game, &["fantasy".into()], false, &mut quiet).unwrap();
    assert_eq!(u.len(), 1);
    let now = tree(&e.game);
    let gone: Vec<&String> = with_all.keys().filter(|k| !now.contains_key(*k) && !k.starts_with("evt_backup/")).collect();
    assert_eq!(gone, vec!["data/common/evt_fantasy/new.bin", "mods/fantasy/files/data/common/y.bin", "mods/fantasy/lua/title/x.lua", "mods/fantasy/mod.toml"]);
    assert_eq!(now[&e.item(1)], retail(1), "fantasy's patched file back to retail");
    assert_eq!(now[&e.item(2)], retail(2));
    assert_eq!(now[&e.item(0)], modded(0), "engine's data stays");
    assert!(e.entry("data/common/evt_fantasy/new.bin").is_none() && e.entry("data/common/evt_engine/deep/new.bin").is_some());
    assert!(!e.game.join("mods/fantasy").exists());
    assert_eq!(e.enabled(), "enabled = [\"other\", \"engine\", \"quit_fix\"]\n");

    // everything
    let u = modops::uninstall_all(&e.game, &mut quiet).unwrap();
    assert_eq!(u.iter().map(|(id, _)| id.as_str()).collect::<Vec<_>>(), vec!["engine", "quit_fix", "_modloader"]);
    assert!(u.iter().any(|(_, o)| o.list_exact), "cpk_list bytes put back");
    let mut after = tree(&e.game);
    assert_eq!(after.remove("evt_loader/loader.log").as_deref(), Some(&b"log"[..]), "player files kept");
    same_trees(&before, &after);
    let mut d = dirs(&e.game);
    d.retain(|x| x != "evt_loader");
    assert_eq!(d, before_dirs);
    assert!(!e.game.join("evt_backup").exists());
}

#[test]
fn missing_dependency_refused_before_any_change() {
    if !std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../assets/v7.1.2/data/cpk_list.cfg.bin").is_file() {
        eprintln!("assets/v7.1.2/data/cpk_list.cfg.bin (your own retail cpk_list) not found: skipped");
        return;
    }
    let e = Env::new("missing");
    e.make_pack(Opts { no_engine: true, ..Opts::default() });
    let pack = e.open_pack();
    let before = tree(&e.game);
    let err = modops::install(&e.game, &pack, &ModsOptions::default(), &mut quiet).unwrap_err();
    assert!(err.contains("engine"), "{err}");
    same_as_before(&before, &e.game);
    // partial: quit_fix goes in, fantasy is reported
    let o = modops::install(&e.game, &pack, &ModsOptions { partial: true, ..Default::default() }, &mut quiet).unwrap();
    assert_eq!(o.installed, vec!["quit_fix 1.0.0"]);
    assert_eq!(o.blocked.len(), 1);
    modops::uninstall_all(&e.game, &mut quiet).unwrap();
    same_as_before(&before, &e.game);
}

#[test]
fn installed_dependency_on_disk_is_enough() {
    if !std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../assets/v7.1.2/data/cpk_list.cfg.bin").is_file() {
        eprintln!("assets/v7.1.2/data/cpk_list.cfg.bin (your own retail cpk_list) not found: skipped");
        return;
    }
    let e = Env::new("ondisk");
    // the player already has engine 1.5 in mods\ (not from us)
    write(&e.game.join("mods/engine/mod.toml"), b"id = \"engine\"\nversion = \"1.5.0\"\n");
    e.make_pack(Opts { no_engine: true, ..Opts::default() });
    let pack = e.open_pack();
    let before = tree(&e.game);
    let o = modops::install(&e.game, &pack, &ModsOptions::default(), &mut quiet).unwrap();
    assert_eq!(o.installed, vec!["fantasy 0.2.0", "quit_fix 1.0.0"]);
    modops::uninstall_all(&e.game, &mut quiet).unwrap();
    same_as_before(&before, &e.game);
}

#[test]
fn update_one_mod_then_uninstall() {
    if !std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../assets/v7.1.2/data/cpk_list.cfg.bin").is_file() {
        eprintln!("assets/v7.1.2/data/cpk_list.cfg.bin (your own retail cpk_list) not found: skipped");
        return;
    }
    let e = Env::new("update");
    e.make_pack(Opts::default());
    let before = tree(&e.game);
    modops::install(&e.game, &e.open_pack(), &ModsOptions::default(), &mut quiet).unwrap();
    e.make_pack(Opts { fantasy_version: "0.3.0", ..Opts::default() });
    let pack = e.open_pack();
    let prep = modops::prepare(&e.game, &pack, &ModsOptions::default()).unwrap();
    assert_eq!(prep.to_install, vec!["fantasy"]);
    assert_eq!(prep.unchanged, vec!["engine", "quit_fix"]);
    let o = modops::install(&e.game, &pack, &ModsOptions::default(), &mut quiet).unwrap();
    assert!(!o.loader_installed);
    assert_eq!(o.installed, vec!["fantasy 0.3.0"]);
    assert_eq!(std::fs::read(e.game.join("mods/fantasy/lua/title/x.lua")).unwrap(), b"-- fantasy 0.3.0");
    assert_eq!(std::fs::read(e.game.join("data/common/evt_fantasy/new.bin")).unwrap(), b"fantasy new 0.3.0");
    assert_eq!(std::fs::read(e.game.join(e.item(1))).unwrap(), xored(modded(1)));
    modops::uninstall_all(&e.game, &mut quiet).unwrap();
    same_as_before(&before, &e.game);
}

#[test]
fn wrong_retail_file_stops_before_any_change() {
    if !std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../assets/v7.1.2/data/cpk_list.cfg.bin").is_file() {
        eprintln!("assets/v7.1.2/data/cpk_list.cfg.bin (your own retail cpk_list) not found: skipped");
        return;
    }
    let e = Env::new("wrong");
    e.make_pack(Opts { wrong_retail: true, ..Opts::default() });
    let pack = e.open_pack();
    let before = tree(&e.game);
    let err = modops::install(&e.game, &pack, &ModsOptions::default(), &mut quiet).unwrap_err();
    assert!(err.contains("no son los de la v7.1.2"), "{err}");
    same_as_before(&before, &e.game);
    assert!(!e.game.join("evt_backup").exists());
    assert!(!e.pack.join("_montaje").exists());
}

#[test]
fn loader_new_enough_is_kept() {
    if !std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../assets/v7.1.2/data/cpk_list.cfg.bin").is_file() {
        eprintln!("assets/v7.1.2/data/cpk_list.cfg.bin (your own retail cpk_list) not found: skipped");
        return;
    }
    let e = Env::new("keeploader");
    write(&e.game.join("winmm.dll"), b"a ModLoader built elsewhere EVT_MODLOADER_VERSION=1.1.0\0");
    e.make_pack(Opts::default());
    let pack = e.open_pack();
    let before = tree(&e.game);
    let o = modops::install(&e.game, &pack, &ModsOptions::default(), &mut quiet).unwrap();
    assert!(!o.loader_installed, "{}", o.loader);
    assert_eq!(std::fs::read(e.game.join("winmm.dll")).unwrap(), before["winmm.dll"]);
    assert!(o.warnings.iter().any(|w| w.contains("evt_loader_modules.toml")), "{:?}", o.warnings);
    modops::uninstall_all(&e.game, &mut quiet).unwrap();
    same_as_before(&before, &e.game);
}

#[test]
fn interrupted_install_is_undone() {
    if !std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../assets/v7.1.2/data/cpk_list.cfg.bin").is_file() {
        eprintln!("assets/v7.1.2/data/cpk_list.cfg.bin (your own retail cpk_list) not found: skipped");
        return;
    }
    let e = Env::new("interrupted");
    e.make_pack(Opts::default());
    let pack = e.open_pack();
    let before = tree(&e.game);
    // a file of the last mod missing from the pack folder: the install fails after the loader and two mods went in
    std::fs::remove_file(e.pack.join("mods/quit_fix/mod/quit_fix.dll")).unwrap();
    assert!(modops::install(&e.game, &pack, &ModsOptions::default(), &mut quiet).is_err());
    let c = modops::find_components(&e.game);
    assert_eq!(c.last().unwrap().record.estado, "instalando");
    modops::uninstall_all(&e.game, &mut quiet).unwrap();
    same_as_before(&before, &e.game);
}

#[test]
fn no_mods_folder_before_is_removed_by_the_last_one_out() {
    if !std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../assets/v7.1.2/data/cpk_list.cfg.bin").is_file() {
        eprintln!("assets/v7.1.2/data/cpk_list.cfg.bin (your own retail cpk_list) not found: skipped");
        return;
    }
    let e = Env::new("nomods");
    std::fs::remove_dir_all(e.game.join("mods")).unwrap();
    e.make_pack(Opts::default());
    let pack = e.open_pack();
    let before = tree(&e.game);
    let before_dirs = dirs(&e.game);
    modops::install(&e.game, &pack, &ModsOptions::default(), &mut quiet).unwrap();
    // no enabled.toml before: the list starts with the pack's mods
    assert_eq!(e.enabled(), "enabled = [\"engine\", \"fantasy\", \"quit_fix\"]\n");
    // the first mod installed (engine, creator of mods\) goes out first: mods\ is handed over, no warning
    let mut warnings = Vec::new();
    for id in ["fantasy", "engine", "quit_fix"] {
        for (_, o) in modops::uninstall_mods(&e.game, &[id.to_string()], false, &mut quiet).unwrap() {
            warnings.extend(o.warnings);
        }
    }
    assert!(warnings.is_empty(), "{warnings:?}");
    assert!(!e.game.join("mods").exists(), "mods/ and enabled.toml gone with the last mod");
    modops::uninstall_all(&e.game, &mut quiet).unwrap();
    same_as_before(&before, &e.game);
    assert_eq!(dirs(&e.game), before_dirs);
}
