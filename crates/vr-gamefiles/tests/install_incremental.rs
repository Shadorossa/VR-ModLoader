//! Incremental install vs the full path, on temp dirs only: a fake game (a small `cpk_list` cut from the repo fixture
//! plus a few "retail" loose files) and a fake project. Never touches the real game.
//! Run: cargo test -p vr-gamefiles --release --test install_incremental
//! Timing on a synthetic ~3,000-file project: add `-- --ignored --nocapture`.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use vr_gamefiles::cpk::{read_list, write_list};
use vr_gamefiles::cpk_list::CpkItem;
use vr_gamefiles::install::{backup_dir_for, install_dirs, restore, InstallOptions, InstallReport};

/// The tests cut their fake game's `cpk_list` from your own retail one at `<repo>/assets/v7.1.2/data/`; without it
/// they are skipped.
fn have_fixture() -> bool {
    let ok = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../assets/v7.1.2/data/cpk_list.cfg.bin").is_file();
    if !ok {
        eprintln!("assets/v7.1.2/data/cpk_list.cfg.bin (your own retail cpk_list) not found: skipped");
    }
    ok
}

/// Small list (60 `data/` entries of the real one; the first 4 made loose with retail files on disk), encrypted.
fn small_list() -> &'static (Vec<u8>, Vec<CpkItem>) {
    static L: OnceLock<(Vec<u8>, Vec<CpkItem>)> = OnceLock::new();
    L.get_or_init(|| {
        let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../assets/v7.1.2/data/cpk_list.cfg.bin");
        let mut l = read_list(&fixture).unwrap();
        let mut items: Vec<CpkItem> = l
            .items
            .iter()
            .filter(|i| i.path().to_lowercase().starts_with("data/") && !vr_gamefiles::walk::is_junk(&i.path()) && !i.is_loose())
            .take(60)
            .cloned()
            .collect();
        for (n, it) in items.iter_mut().take(4).enumerate() {
            it.cpk_dir = Some(String::new());
            it.cpk_name = Some(String::new());
            it.size = retail(n).len() as i32;
        }
        l.items = items.clone();
        l.sort();
        let tmp = std::env::temp_dir().join(format!("evt_incr_list_{}.bin", std::process::id()));
        write_list(&tmp, &l).unwrap();
        let bytes = std::fs::read(&tmp).unwrap();
        let _ = std::fs::remove_file(&tmp);
        (bytes, items)
    })
}

fn retail(n: usize) -> Vec<u8> {
    format!("retail original #{n} ").repeat(n + 3).into_bytes()
}

/// Path of the n-th list item (0..4 are retail loose files, 4.. live in a CPK).
fn item(n: usize) -> String {
    small_list().1[n].path()
}

struct Env {
    root: PathBuf,
    game: PathBuf,
    project: PathBuf,
    backups: PathBuf,
}

impl Env {
    fn new(name: &str) -> Self {
        let root = std::env::temp_dir().join(format!("evt_incr_{name}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let e = Env { game: root.join("game"), project: root.join("project"), backups: root.join("backups"), root };
        std::fs::create_dir_all(e.game.join("data")).unwrap();
        std::fs::create_dir_all(e.project.join("data")).unwrap();
        std::fs::write(e.game.join("data/cpk_list.cfg.bin"), &small_list().0).unwrap();
        for n in 0..4 {
            write(&e.game.join(item(n)), &retail(n));
        }
        e
    }

    fn put(&self, rel: &str, bytes: &[u8]) {
        write(&self.project.join(rel), bytes);
    }

    fn drop_file(&self, rel: &str) {
        std::fs::remove_file(self.project.join(rel)).unwrap();
    }

    fn install(&self, full: bool) -> InstallReport {
        install_dirs(&self.game, &self.project, &self.backups, InstallOptions { full }).unwrap()
    }

    fn backup(&self) -> PathBuf {
        backup_dir_for(&self.backups, &self.game)
    }

    fn restore(&self) {
        restore(&self.game, &self.backup()).unwrap().unwrap();
    }

    fn list(&self) -> Vec<u8> {
        std::fs::read(self.game.join("data/cpk_list.cfg.bin")).unwrap()
    }

    fn entry(&self, rel: &str) -> Option<CpkItem> {
        read_list(&self.game.join("data/cpk_list.cfg.bin")).unwrap().items.into_iter().find(|i| i.path() == rel)
    }

    /// Manifest records (no timestamps / absolute folders).
    fn records(&self) -> serde_json::Value {
        let m: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(self.backup().join("manifest.json")).unwrap()).unwrap();
        m["files"].clone()
    }

    /// Everything that matters of an install: game tree (incl. cpk_list), backup copies, manifest records.
    fn state(&self) -> (Tree, Tree, serde_json::Value) {
        (tree(&self.game), tree(&self.backup().join("files")), self.records())
    }
}

impl Drop for Env {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn write(p: &Path, bytes: &[u8]) {
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(p, bytes).unwrap();
}

/// Relative path → bytes of every file under `root`.
type Tree = BTreeMap<String, Vec<u8>>;
type Step = Box<dyn Fn(&Env)>;

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

/// Project: 2 retail loose files replaced, 2 CPK files made loose, 2 brand-new files.
fn base_project(e: &Env) {
    e.put(&item(0), b"mod 0");
    e.put(&item(1), b"mod 1 longer");
    e.put(&item(5), b"mod 5");
    e.put(&item(6), b"mod 6 ......");
    e.put("data/common/evt_test/new_a.bin", b"new a");
    e.put("data/common/evt_test/new_b.bin", b"new b");
}

#[test]
fn noop_install_copies_nothing() {
    if !have_fixture() {
        return;
    }
    let e = Env::new("noop");
    base_project(&e);
    let first = e.install(false);
    assert!(first.full, "no manifest yet: full install");
    assert_eq!((first.written, first.files, first.new_entries), (6, 6, 2));
    let (list, state) = (e.list(), e.state());

    let again = e.install(false);
    assert!(!again.full, "{}", again.full_reason);
    assert_eq!((again.written, again.skipped, again.restored, again.backed_up), (0, 6, 0, 0));
    assert_eq!((again.new_entries, again.modified_entries), (0, 0));
    assert_eq!(e.list(), list, "same cpk_list");
    assert_eq!(e.state(), state, "same game, backup and manifest");

    // `--full` still reinstalls everything, with the same result.
    let full = e.install(true);
    assert!(full.full);
    assert_eq!((full.written, full.restored), (6, 6));
    assert_eq!(e.list(), list);
    assert_eq!(e.state(), state);

    // An older manifest version falls back to the full path.
    let p = e.backup().join("manifest.json");
    let m = std::fs::read_to_string(&p).unwrap().replace("\"version\": 3", "\"version\": 2");
    std::fs::write(&p, m).unwrap();
    let old = e.install(false);
    assert!(old.full && old.full_reason.contains("v2"), "{}", old.full_reason);
    assert_eq!(e.state(), state);
}

#[test]
fn one_changed_file_is_one_copy_and_backup_stays_retail() {
    if !have_fixture() {
        return;
    }
    let e = Env::new("changed");
    base_project(&e);
    e.install(false);
    e.put(&item(0), b"mod 0, second version (other size)");
    let r = e.install(false);
    assert!(!r.full);
    assert_eq!((r.written, r.skipped, r.backed_up, r.modified_entries), (1, 5, 0, 1));
    assert_eq!(std::fs::read(e.game.join(item(0))).unwrap(), b"mod 0, second version (other size)");
    assert_eq!(std::fs::read(e.backup().join("files").join(item(0))).unwrap(), retail(0), "backup is still the retail original");
    assert_eq!(e.entry(&item(0)).unwrap().size, 34, "cpk_list size follows the file");
    // Same size, other content: still one copy, list untouched.
    let list = e.list();
    e.put(&item(0), b"MOD 0, SECOND VERSION (OTHER SIZE)");
    let r = e.install(false);
    assert_eq!((r.written, r.modified_entries), (1, 0));
    assert_eq!(e.list(), list);
    e.restore();
    assert_eq!(std::fs::read(e.game.join(item(0))).unwrap(), retail(0));
    assert_eq!(e.list(), small_list().0, "restore gives back the original list byte for byte");
}

#[test]
fn new_and_removed_files() {
    if !have_fixture() {
        return;
    }
    let e = Env::new("newgone");
    base_project(&e);
    e.install(false);
    e.put("data/common/evt_test/new_c.bin", b"new c");
    e.put(&item(2), b"mod 2 over a retail file");
    e.drop_file(&item(1)); // retail file: goes back
    e.drop_file(&item(5)); // CPK file: loose copy deleted, entry back to the CPK
    e.drop_file("data/common/evt_test/new_a.bin"); // new file: deleted, entry removed
    let r = e.install(false);
    assert!(!r.full);
    assert_eq!((r.written, r.skipped, r.restored, r.backed_up), (2, 3, 3, 1));
    assert_eq!((r.new_entries, r.modified_entries), (1, 3), "new_c added; item 2 resized; items 1 and 5 reverted");
    assert_eq!(std::fs::read(e.game.join(item(1))).unwrap(), retail(1));
    assert!(!e.game.join(item(5)).exists());
    assert!(!e.game.join("data/common/evt_test/new_a.bin").exists());
    assert_eq!(e.entry(&item(5)), small_list().1.iter().find(|i| i.path() == item(5)).cloned());
    assert!(e.entry("data/common/evt_test/new_a.bin").is_none());
    assert!(e.entry("data/common/evt_test/new_c.bin").unwrap().is_loose());
    assert!(!e.backup().join("files").join(item(1)).exists(), "backup of a file no longer installed is dropped");
    assert_eq!(std::fs::read(e.backup().join("files").join(item(2))).unwrap(), retail(2));
    e.restore();
    assert_eq!(e.list(), small_list().0);
    for n in 0..4 {
        assert_eq!(std::fs::read(e.game.join(item(n))).unwrap(), retail(n));
    }
}

/// Another program rewrites one of our installed files (still in the project) and one we are removing: the
/// incremental install decides exactly like the full path and warns.
#[test]
fn file_changed_by_another_program() {
    if !have_fixture() {
        return;
    }
    let (inc, full) = (Env::new("foreign_inc"), Env::new("foreign_full"));
    for (e, f) in [(&inc, false), (&full, true)] {
        base_project(e);
        e.install(f);
        write(&e.game.join(item(0)), b"another tool's version");
        write(&e.game.join(item(6)), b"another tool, 6");
        e.drop_file(&item(6));
    }
    let r = inc.install(false);
    full.install(true);
    assert!(!r.full);
    assert!(r.warnings.iter().any(|w| w.starts_with(&item(0)) && w.contains("otro programa")), "{:?}", r.warnings);
    assert!(r.warnings.iter().any(|w| w.starts_with(&item(6)) && w.contains("otro programa")), "{:?}", r.warnings);
    assert_eq!(r.written, 1, "only the foreign-modified project file is copied again");
    assert_eq!(std::fs::read(inc.game.join(item(0))).unwrap(), b"mod 0");
    assert_eq!(std::fs::read(inc.game.join(item(6))).unwrap(), b"another tool, 6", "removed but foreign: left alone");
    assert_eq!(inc.state(), full.state());
    assert_eq!(inc.list(), full.list());
    inc.restore();
    full.restore();
    assert_eq!(tree(&inc.game), tree(&full.game));
}

/// «Restaurar el juego» after a series of incremental installs == after the full path (today's behaviour), and
/// both == the untouched game when nobody else changed anything.
#[test]
fn restore_after_incremental_equals_restore_after_full() {
    if !have_fixture() {
        return;
    }
    let pristine = Env::new("pristine");
    let (inc, full) = (Env::new("eq_inc"), Env::new("eq_full"));
    let steps: Vec<Step> = vec![
        Box::new(base_project),
        Box::new(|e: &Env| e.put(&item(0), b"mod 0 v2 with another size")),
        Box::new(|e: &Env| {
            e.put("data/common/evt_test/new_c.bin", b"c");
            e.put(&item(3), b"mod 3");
            e.drop_file(&item(1));
            e.drop_file("data/common/evt_test/new_a.bin");
        }),
        Box::new(|e: &Env| {
            e.put(&item(1), b"mod 1 is back");
            e.put("data/common/evt_test/new_a.bin", b"a again");
            e.drop_file(&item(5));
        }),
        Box::new(|_: &Env| {}),
    ];
    for (i, step) in steps.iter().enumerate() {
        for (e, f) in [(&inc, false), (&full, true)] {
            step(e);
            let r = e.install(f);
            assert_eq!(r.full, f || i == 0, "step {i}: {}", r.full_reason);
        }
        assert_eq!(inc.state(), full.state());
        assert_eq!(inc.list(), full.list());
    }
    inc.restore();
    full.restore();
    assert_eq!(tree(&inc.game), tree(&full.game));
    assert_eq!(tree(&inc.game), tree(&pristine.game), "back to the untouched game, cpk_list included");

    // With foreign changes in between (a file of ours and an unrelated list entry), still identical to the full path.
    let (inc, full) = (Env::new("eqf_inc"), Env::new("eqf_full"));
    for (i, step) in steps.iter().enumerate() {
        for (e, f) in [(&inc, false), (&full, true)] {
            step(e);
            e.install(f);
            if i == 1 {
                write(&e.game.join(item(5)), b"foreign 5");
                let p = e.game.join("data/cpk_list.cfg.bin");
                let mut l = read_list(&p).unwrap();
                let it = l.items.iter_mut().find(|i| i.path() == item(20)).unwrap();
                it.cpk_dir = Some(String::new());
                it.cpk_name = Some(String::new());
                write_list(&p, &l).unwrap();
            }
        }
        assert_eq!(inc.state(), full.state(), "step {i}");
        assert_eq!(inc.list(), full.list(), "step {i}");
    }
    inc.restore();
    full.restore();
    assert_eq!(tree(&inc.game), tree(&full.game));
}

/// Timing of a no-op and a 1-file install on ~3,000 synthetic files (~150 MB) with the real-size `cpk_list` (the repo
/// fixture, 13 MB), full vs incremental. `vr_gamefiles::timing` echoes the phases.
#[test]
#[ignore]
fn timing_3000_files() {
    if !have_fixture() {
        return;
    }
    let e = Env::new("timing");
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../assets/v7.1.2/data/cpk_list.cfg.bin");
    std::fs::copy(fixture, e.game.join("data/cpk_list.cfg.bin")).unwrap();
    let old = std::time::SystemTime::now() - std::time::Duration::from_secs(3600);
    for n in 0..3000 {
        let rel = format!("data/common/evt_bench/d{:02}/f{n:04}.bin", n % 40);
        let size = 8_000 + (n * 7919) % 90_000;
        let bytes: Vec<u8> = (0..size).map(|i| (i ^ n) as u8).collect();
        e.put(&rel, &bytes);
        // An old mtime, like a real project (files written in the last 2 s are never cached).
        std::fs::File::options().write(true).open(e.project.join(&rel)).unwrap().set_modified(old).unwrap();
    }
    let show = |what: &str, r: &InstallReport| {
        println!(
            "{what:<28} {:>6} ms  full={} copied={} skipped={} restored={} backed_up={}",
            r.elapsed_ms, r.full, r.written, r.skipped, r.restored, r.backed_up
        )
    };
    vr_gamefiles::timing::enable(true);
    show("first install (full)", &e.install(false));
    show("no-op, --full (old path)", &e.install(true));
    show("no-op, incremental", &e.install(false));
    show("no-op, incremental (again)", &e.install(false));
    let one = e.project.join("data/common/evt_bench/d07/f0007.bin");
    std::fs::write(&one, b"changed").unwrap();
    std::fs::File::options().write(true).open(&one).unwrap().set_modified(old).unwrap();
    show("1 file changed, incremental", &e.install(false));
}
