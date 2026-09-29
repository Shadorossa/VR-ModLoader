//! Install into a FAKE game folder (temp dir: a small cpk_list cut from the retail v7.1.2 list, a few "retail" loose
//! files, a foreign winmm.dll and mod) from a fake pack (whole files, deltas, loader files, a mod), then uninstall and
//! check the game folder is byte for byte what it was. Also: a wrong retail file stops the install before anything
//! changes; reinstall over an install; version detection; merging a fake depot. Never touches the real game.
//! Run: cargo test -p evt-installer --release --test roundtrip

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use evt_installer::pack::{GameBuild, IndexFile, Pack, PackFile, Patch, PatchesFile};
use evt_installer::{delta, file_crc, ops, version};
use vr_gamefiles::cpk::{read_list, write_list};
use vr_gamefiles::cpk_list::CpkItem;

/// Retail list: your own v7.1.2 `cpk_list.cfg.bin` under assets/v7.1.2 (read only; the tests skip without it).
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

/// Our edit of a retail file: a few bytes changed, some inserted.
fn modded(n: usize) -> Vec<u8> {
    let mut v = retail(n);
    v[100..110].copy_from_slice(b"EVT EDIT! ");
    v.splice(5000..5000, b"inserted by the mod".iter().copied());
    v
}

type Tree = BTreeMap<String, Vec<u8>>;

const XOR_KEY: u32 = 0x1234_5678;

fn xored(mut b: Vec<u8>) -> Vec<u8> {
    l5_cpk::xor::crypt_in_place(XOR_KEY, 0, &mut b);
    b
}

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

impl Env {
    /// Fake game: 40 data entries of the retail list, the first 3 loose with "retail" files on disk; a foreign
    /// winmm.dll and a foreign mod enabled in mods\enabled.toml.
    fn new(name: &str) -> Env {
        let root = std::env::temp_dir().join(format!("evt_inst_{name}_{}", std::process::id()));
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

    /// Fake pack: 2 deltas (retail loose files 0, 1), 1 whole replaced file (loose retail 2), 1 whole replaced CPK
    /// file (item 5), 2 new files, loader files, a mod.
    fn make_pack(&self, retail_for_delta: &dyn Fn(usize) -> Vec<u8>) {
        let p = &self.pack;
        let _ = std::fs::remove_dir_all(p);
        let mut patches = Vec::new();
        for n in 0..2 {
            let rel = self.item(n);
            let d = delta::diff(&retail_for_delta(n), &modded(n));
            let dp = format!("delta/{rel}.evtd");
            write(&p.join(&dp), &d);
            // #1 is installed XOR-encrypted, like a loose awb (the delta is on the plaintext)
            patches.push(Patch { ruta: rel, delta: dp, tam: modded(n).len() as u64, xor: (n == 1).then_some(XOR_KEY) });
        }
        write(&p.join(self.item(2)), b"whole replacement of retail loose #2");
        write(&p.join(self.item(5)), b"whole replacement of a CPK file");
        write(&p.join("data/common/evt_test/new_a.bin"), b"new a");
        write(&p.join("data/common/evt_test/deep/new_b.bin"), b"new b");
        write(&p.join("juego/winmm.dll"), b"OUR LOADER");
        write(&p.join("juego/steam_appid.txt"), b"2799860\r\n");
        write(&p.join("juego/evt_loader/lua_patches/_fingerprints.json"), b"{}");
        write(&p.join("juego/evt_loader/lua_patches/main_menu/a.lua"), b"-- patch");
        write(&p.join("juego/evt_loader/cs_cameras/evtgx_x.json"), b"{\"cam\":1}");
        write(
            &p.join("juego/evt_loader/config.toml"),
            b"[loader]\r\nlog_level = \"info\"\r\n\r\n[modules]\r\nlua_bridge = true\r\nstats = false\r\n\r\n[stats]\r\nhero_mult = 1.0\r\n",
        );
        write(&p.join("juego/mods/testmod/mod.toml"), b"id = \"testmod\"\nname = \"Test\"\nversion = \"1.0.0\"\n");
        write(&p.join("juego/mods/testmod/files/data/common/x.bin"), b"mod file");
        write(&p.join("evt_loader_modules.toml"), b"lua_bridge = true\nstats = true  # per-player stats\nrogue = true\n");
        std::fs::write(p.join("parches.json"), serde_json::to_string(&PatchesFile { parches: patches }).unwrap()).unwrap();
        std::fs::write(
            p.join("pack.toml"),
            "producto = \"Example Pack\"\nversion = \"0.0.1-test\"\nformato = 1\nmods = [\"testmod\"]\n\n[juego]\nbuildid = \"24370575\"\ndepot = \"2799861\"\nmanifest = \"7633204652048533395\"\n",
        )
        .unwrap();
        let mut archivos = Vec::new();
        for (rel, _) in tree(p) {
            if ["data/", "delta/", "juego/"].iter().any(|t| rel.starts_with(t)) {
                let (tam, crc) = file_crc(&p.join(&rel)).unwrap();
                archivos.push(PackFile { ruta: rel, tam, crc });
            }
        }
        std::fs::write(p.join("indice.json"), serde_json::to_string(&IndexFile { archivos }).unwrap()).unwrap();
    }

    fn open_pack(&self) -> Pack {
        let pack = Pack::open(&self.pack).unwrap();
        assert!(pack.validate().is_empty(), "{:?}", pack.validate());
        assert!(pack.check_files().is_empty());
        pack
    }

    fn entry(&self, rel: &str) -> Option<CpkItem> {
        read_list(&self.game.join("data/cpk_list.cfg.bin")).unwrap().items.into_iter().find(|i| i.path() == rel)
    }
}

fn quiet(_: &str) {}

#[test]
fn install_then_uninstall_restores_byte_for_byte() {
    if !std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../assets/v7.1.2/data/cpk_list.cfg.bin").is_file() {
        eprintln!("assets/v7.1.2/data/cpk_list.cfg.bin (your own retail cpk_list) not found: skipped");
        return;
    }
    let e = Env::new("roundtrip");
    e.make_pack(&retail);
    let pack = e.open_pack();
    let before = tree(&e.game);
    let before_dirs = dirs(&e.game);

    let o = ops::install(&e.game, &pack, &mut quiet).unwrap();
    assert_eq!(o.data_files, 6, "2 patched + 2 whole replaced + 2 new");
    assert!(!e.pack.join("_montaje").exists(), "staging removed");
    // data
    assert_eq!(std::fs::read(e.game.join(e.item(0))).unwrap(), modded(0));
    assert_eq!(std::fs::read(e.game.join(e.item(1))).unwrap(), xored(modded(1)));
    assert_eq!(std::fs::read(e.game.join("data/common/evt_test/deep/new_b.bin")).unwrap(), b"new b");
    let it = e.entry(&e.item(5)).unwrap();
    assert!(it.is_loose());
    assert_eq!(it.size as usize, b"whole replacement of a CPK file".len());
    assert_eq!(e.entry(&e.item(0)).unwrap().size as usize, modded(0).len());
    assert!(e.entry("data/common/evt_test/new_a.bin").is_some());
    // loader files, config, mods
    assert_eq!(std::fs::read(e.game.join("winmm.dll")).unwrap(), b"OUR LOADER");
    assert!(e.game.join("evt_loader/lua_patches/main_menu/a.lua").is_file());
    let cfg = std::fs::read_to_string(e.game.join("evt_loader/config.toml")).unwrap();
    assert!(cfg.contains("stats = true") && cfg.contains("rogue = true") && cfg.contains("hero_mult = 1.0"), "{cfg}");
    assert_eq!(std::fs::read_to_string(e.game.join("mods/enabled.toml")).unwrap(), "enabled = [\"other\", \"testmod\"]\n");
    assert_eq!(ops::find_installs(&e.game).len(), 1);
    assert_eq!(ops::find_installs(&e.game)[0].record.estado, "instalado");

    // The player plays: the loader writes its log and a save file of the mod.
    write(&e.game.join("evt_loader/loader.log"), b"log");
    write(&e.game.join("evt_loader/career_slot2.json"), b"{}");

    let u = ops::uninstall_all(&e.game, &mut quiet).unwrap();
    assert_eq!(u.len(), 1);
    assert!(u[0].list_exact, "cpk_list bytes put back");
    let mut after = tree(&e.game);
    assert_eq!(after.remove("evt_loader/loader.log").as_deref(), Some(&b"log"[..]), "player files kept");
    assert!(after.remove("evt_loader/career_slot2.json").is_some());
    assert_eq!(after.keys().collect::<Vec<_>>(), before.keys().collect::<Vec<_>>());
    for (k, v) in &before {
        assert!(after[k] == *v, "{k} differs after uninstall");
    }
    let mut d = dirs(&e.game);
    d.retain(|x| x != "evt_loader");
    assert_eq!(d, before_dirs, "created folders removed (evt_loader kept: it has player files)");
    assert!(!e.game.join("evt_backup").exists());
}

#[test]
fn wrong_retail_file_stops_before_any_change() {
    if !std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../assets/v7.1.2/data/cpk_list.cfg.bin").is_file() {
        eprintln!("assets/v7.1.2/data/cpk_list.cfg.bin (your own retail cpk_list) not found: skipped");
        return;
    }
    let e = Env::new("wrongsrc");
    // deltas made against another "retail" file than the one in the game
    e.make_pack(&|n| {
        let mut r = retail(n);
        r[7] ^= 0x55;
        r
    });
    let pack = e.open_pack();
    let before = tree(&e.game);
    let err = ops::install(&e.game, &pack, &mut quiet).unwrap_err();
    assert!(err.contains("no son los de la v7.1.2"), "{err}");
    assert_eq!(tree(&e.game), before, "game untouched");
    assert!(!e.game.join("evt_backup").exists());
    assert!(!e.pack.join("_montaje").exists());
}

#[test]
fn reinstall_over_install_then_uninstall() {
    if !std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../assets/v7.1.2/data/cpk_list.cfg.bin").is_file() {
        eprintln!("assets/v7.1.2/data/cpk_list.cfg.bin (your own retail cpk_list) not found: skipped");
        return;
    }
    let e = Env::new("reinstall");
    e.make_pack(&retail);
    let pack = e.open_pack();
    let before = tree(&e.game);
    ops::install(&e.game, &pack, &mut quiet).unwrap();
    // update = uninstall the previous one, install again (what the UI does)
    ops::uninstall_all(&e.game, &mut quiet).unwrap();
    ops::install(&e.game, &pack, &mut quiet).unwrap();
    assert_eq!(std::fs::read(e.game.join(e.item(1))).unwrap(), xored(modded(1)));
    ops::uninstall_all(&e.game, &mut quiet).unwrap();
    assert_eq!(tree(&e.game), before);
}

#[test]
fn interrupted_install_is_undone() {
    if !std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../assets/v7.1.2/data/cpk_list.cfg.bin").is_file() {
        eprintln!("assets/v7.1.2/data/cpk_list.cfg.bin (your own retail cpk_list) not found: skipped");
        return;
    }
    let e = Env::new("interrupted");
    e.make_pack(&retail);
    let pack = e.open_pack();
    let before = tree(&e.game);
    // a loader file missing from the pack folder: the install fails after the data went in
    std::fs::remove_file(e.pack.join("juego/evt_loader/cs_cameras/evtgx_x.json")).unwrap();
    assert!(ops::install(&e.game, &pack, &mut quiet).is_err());
    let i = ops::find_installs(&e.game);
    assert_eq!(i.len(), 1);
    assert_eq!(i[0].record.estado, "instalando");
    ops::uninstall_all(&e.game, &mut quiet).unwrap();
    assert_eq!(tree(&e.game), before);
}

#[test]
fn version_detection() {
    let root = std::env::temp_dir().join(format!("evt_ver_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let game = root.join("steamapps/common/INAZUMA ELEVEN Victory Road");
    write(&game.join("nie.exe"), b"a newer exe");
    write(&game.join("data/cpk_list.cfg.bin"), b"x");
    write(
        &root.join("steamapps/appmanifest_2799860.acf"),
        b"\"AppState\"\n{\n\t\"appid\"\t\t\"2799860\"\n\t\"buildid\"\t\t\"99999999\"\n\t\"AutoUpdateBehavior\"\t\t\"0\"\n}\n",
    );
    let want = GameBuild { buildid: "24370575".into(), depot: "2799861".into(), manifest: "7633204652048533395".into(), ..GameBuild::default() };
    let st = version::state(&game, &want);
    assert!(!st.nie_ok);
    assert_eq!(st.buildid.as_deref(), Some("99999999"));
    assert!(!st.buildid_ok(&want));
    assert_eq!(st.auto_update.as_deref(), Some("0"));
    assert_eq!(version::console_command(&want), "download_depot 2799860 2799861 7633204652048533395");
    // the exe the pack was built on counts as v7.1.2
    let want2 = GameBuild { nie_sha1: delta::hex(&delta::sha1(b"a newer exe")), ..want.clone() };
    assert!(version::state(&game, &want2).nie_ok);
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn depot_merge_copies_only_what_differs() {
    for allow_move in [false, true] {
        let root = std::env::temp_dir().join(format!("evt_depot_{allow_move}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let depot = root.join("depot");
        let game = root.join("game");
        write(&depot.join("nie.exe"), b"exe 7.1.2");
        write(&depot.join("data/packs/a.cpk"), b"same pack");
        write(&depot.join("data/packs/b.cpk"), b"old pack B"); // same size as the new one, other content
        write(&depot.join("data/packs/c.cpk"), b"only in 7.1.2");
        write(&depot.join("data/cpk_list.cfg.bin"), b"list 7.1.2");
        write(&game.join("nie.exe"), b"exe 7.2.0 (newer)");
        write(&game.join("data/packs/a.cpk"), b"same pack");
        write(&game.join("data/packs/b.cpk"), b"new pack B");
        write(&game.join("data/packs/z.cpk"), b"only in the newer build");
        write(&game.join("data/cpk_list.cfg.bin"), b"list 7.2.0");
        let mut last = (0, 0);
        let r = version::merge_depot(&depot, &game, allow_move, &mut |d, t| last = (d, t)).unwrap();
        assert_eq!(r.files, 5);
        assert_eq!((r.replaced, r.same), (4, 1));
        assert_eq!(r.moved, allow_move);
        assert_eq!(last.0, last.1);
        assert_eq!(std::fs::read(game.join("nie.exe")).unwrap(), b"exe 7.1.2");
        assert_eq!(std::fs::read(game.join("data/packs/b.cpk")).unwrap(), b"old pack B");
        assert_eq!(std::fs::read(game.join("data/packs/c.cpk")).unwrap(), b"only in 7.1.2");
        assert_eq!(std::fs::read(game.join("data/cpk_list.cfg.bin")).unwrap(), b"list 7.1.2");
        assert!(game.join("data/packs/z.cpk").is_file(), "extra files of the newer build left alone");
        assert!(!tree(&game).keys().any(|k| k.ends_with(".evt_tmp")));
        let _ = std::fs::remove_dir_all(&root);
    }
}
