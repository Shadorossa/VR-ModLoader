//! Integration test against the v7.1.2 dump (read-only; skipped when absent): the real `chara_base`, `chara_text`,
//! `menu_text`, `system_text` of the 9 languages, read through the dump's `data\cpk_list.cfg.bin` like in the game.
//! Path: env `EVT_DUMP` (the `Extracted` folder), else the default location.

use std::path::{Path, PathBuf};
use std::time::Instant;
use text_engine::boot::{self, BootIn};
use text_engine::fw::game::{self, GameSource};
use text_engine::fw::slots::{SlotPolicy, SlotState};
use text_engine::fw::{Lvl, ModDir};
use text_engine::lang::{self, LANGS};
use text_engine::table::{Kind, TextTable};

const DEFAULT_DUMP: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../../assets/v7.1.2");
const MARK: u32 = -447611118i32 as u32; // 0xE551FF12, chara_text id of Mark Evans' full name (CHARA_BASE_INFO[3] of c01000010)

fn dump() -> Option<PathBuf> {
    let p = PathBuf::from(std::env::var("EVT_DUMP").unwrap_or_else(|_| DEFAULT_DUMP.to_string()));
    p.join("data").join("cpk_list.cfg.bin").is_file().then_some(p)
}

fn write(p: &Path, b: &[u8]) {
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(p, b).unwrap();
}

fn table(p: &Path) -> TextTable {
    TextTable::parse(&std::fs::read(p).unwrap()).unwrap()
}

fn text(t: &TextTable, k: Option<Kind>, id: u32, v: i32) -> Option<String> {
    t.find(k, id, v).and_then(|(_, i)| t.text(i).map(str::to_string))
}

#[test]
fn retail_tables_rename_add_and_serve() {
    let Some(game) = dump() else {
        eprintln!("dump not found: skipped");
        return;
    };
    let root = std::env::temp_dir().join(format!("evt-te-dump-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let te = root.join("mods").join("text_engine");
    std::fs::create_dir_all(&te).unwrap();
    let m = root.join("mods").join("test_text_mod");
    write(
        &m.join("text").join("en.toml"),
        b"[new]\ngreeting = \"Hello from test_text_mod!\"\n[replace]\n\"chara.c01000010.name\" = \"Mark Evans MOD\"\n",
    );
    write(&m.join("text").join("es.toml"), "[new]\ngreeting = \"¡Hola desde test_text_mod!\"\n[replace]\n\"chara.c01000010.name\" = \"Mark Evans MOD (es)\"\n".as_bytes());
    // a second mod: renames Mark's given name in French only, and a bare label of system_text in English only
    let m2 = root.join("mods").join("second");
    write(
        &m2.join("text.toml"),
        b"default_lang = \"none\"\n[fr.replace]\n\"chara.c01000010.given\" = \"Marc\"\n[en.replace]\n\"sysmes_notification_log_get_item\" = \"Got [CG]<ITEM_NAME>[C]!\"\n",
    );
    let mods = vec![
        ModDir { id: "text_engine".into(), dir: te.clone(), load_index: 0 },
        ModDir { id: "test_text_mod".into(), dir: m.clone(), load_index: 1 },
        ModDir { id: "second".into(), dir: m2.clone(), load_index: 2 },
    ];
    let loader = root.join("evt_loader");
    let inp = BootIn { self_id: "text_engine", self_dir: &te, loader_dir: &loader, mods: &mods, inactive: &[], policy: SlotPolicy::default() };
    let t0 = Instant::now();
    let out = boot::run(&inp, &mut GameSource::new(&game));
    eprintln!("first build: {} ms", t0.elapsed().as_millis());
    for (l, s) in &out.notes.0 {
        eprintln!("{l:?} {s}");
    }
    assert!(out.notes.at(Lvl::Error).next().is_none(), "{:?}", out.notes);
    assert!(!out.from_cache);
    let gid = out.index.keys["test_text_mod.greeting"].id;
    assert_eq!(gid, l5_core::hash::crc32_str("test_text_mod.greeting"));
    // slots: chara_text + menu_text of 9 languages, system_text of en
    let keys: Vec<&str> = out.slots.iter().map(|s| s.key.as_str()).collect();
    assert_eq!(out.slots.len(), 19, "{keys:?}");
    assert!(out.slots.iter().all(|s| s.state == SlotState::Pending && s.size % 4096 == 0));
    for l in LANGS {
        let base = table(&game.join(lang::key(l, "chara_text")));
        let slot = table(&game::mod_file(&te, &lang::key(l, "chara_text")));
        let want = match l {
            "es" => "Mark Evans MOD (es)",
            _ => "Mark Evans MOD",
        };
        assert_eq!(text(&slot, Some(Kind::Noun), MARK, 0).as_deref(), Some(want), "{l}");
        let given = if l == "fr" { Some("Marc".to_string()) } else { text(&base, Some(Kind::Noun), MARK, 12) };
        assert_eq!(text(&slot, Some(Kind::Noun), MARK, 12), given, "{l}");
        assert_eq!(text(&slot, Some(Kind::Noun), MARK, 11), text(&base, Some(Kind::Noun), MARK, 11), "{l}");
        // every other row is untouched
        assert_eq!(slot.doc.entries.len(), base.doc.entries.len());
        let diff = slot.doc.entries.iter().zip(&base.doc.entries).filter(|(a, b)| a != b).count();
        assert_eq!(diff, if l == "fr" { 2 } else { 1 }, "{l}");
        // menu_text: one new row, the rest identical
        let mb = table(&game.join(lang::key(l, "menu_text")));
        let ms = table(&game::mod_file(&te, &lang::key(l, "menu_text")));
        assert_eq!(ms.rows(), mb.rows() + 1);
        let want = if l == "es" { "¡Hola desde test_text_mod!" } else { "Hello from test_text_mod!" };
        assert_eq!(text(&ms, Some(Kind::Text), gid, 0).as_deref(), Some(want), "{l}");
        assert!(mb.find(None, gid, 0).is_none());
    }
    let sys = table(&game::mod_file(&te, &lang::key("en", "system_text")));
    assert_eq!(text(&sys, None, 1389146809, 0).as_deref(), Some("Got [CG]<ITEM_NAME>[C]!"));
    assert!(!game::mod_file(&te, &lang::key("de", "system_text")).exists());
    // run-time lookups
    assert_eq!(out.index.text("de", None, gid, 0), Some("Hello from test_text_mod!"));
    // second start: from the cache, pending → served, fast
    let t0 = Instant::now();
    let out2 = boot::run(&inp, &mut GameSource::new(&game));
    eprintln!("cache hit: {} ms", t0.elapsed().as_millis());
    assert!(out2.from_cache);
    assert!(out2.slots.iter().all(|s| s.state == SlotState::Served));
    assert_eq!(out2.index, out.index);
    let _ = std::fs::remove_dir_all(&root);
}
