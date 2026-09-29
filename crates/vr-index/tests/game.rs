//! Integration tests against the real game (read-only).
//!
//! * `VR_GAME_DIR` (default `D:\SteamLibrary\steamapps\common\INAZUMA ELEVEN Victory Road`): the install whose CPKs
//!   are indexed. Tests are skipped (pass with a message) when it is missing.
//! * `VR_DUMP_DIR` (default the v7.1.2 `Extracted` dump): only used to check that what we read from the CPKs is
//!   byte-identical to the extracted retail files.
//!
//! The index is built once into `CARGO_TARGET_TMPDIR/vr-index-test` (thumbnails are reused between runs).

use std::path::PathBuf;
use std::sync::OnceLock;

use vr_index::{lang_index, BuildOptions, Category, Freshness, GameSource, Index, Query, Resolution, SourceOptions, Val};

fn game_dir() -> Option<PathBuf> {
    let p = std::env::var_os("VR_GAME_DIR").map(PathBuf::from).unwrap_or_else(|| PathBuf::from(r"D:\SteamLibrary\steamapps\common\INAZUMA ELEVEN Victory Road"));
    p.join("data").join("cpk_list.cfg.bin").is_file().then_some(p)
}

fn dump_dir() -> Option<PathBuf> {
    let p = std::env::var_os("VR_DUMP_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/../../assets/v7.1.2")));
    p.join("data").is_dir().then_some(p)
}

fn out_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("vr-index-test")
}

fn index() -> Option<&'static Index> {
    static IDX: OnceLock<Option<Index>> = OnceLock::new();
    IDX.get_or_init(|| {
        let game = game_dir()?;
        let t0 = std::time::Instant::now();
        let idx = Index::build(&game, out_dir(), &BuildOptions::default(), &|_, _| {}).expect("build");
        let size = std::fs::metadata(out_dir().join(vr_index::INDEX_FILE)).map(|m| m.len()).unwrap_or(0);
        eprintln!("index built in {:.1} s ({} ms in build_index), index.vri {:.1} MB, {} thumbnails, counts {:?}", t0.elapsed().as_secs_f64(), idx.meta().build_ms, size as f64 / 1e6, idx.meta().thumbs, idx.meta().counts);
        Some(idx)
    })
    .as_ref()
}

macro_rules! need_index {
    () => {
        match index() {
            Some(i) => i,
            None => {
                eprintln!("SKIP: no game install (set VR_GAME_DIR)");
                return;
            }
        }
    };
}

fn en() -> usize {
    lang_index("en").unwrap()
}
fn es() -> usize {
    lang_index("es").unwrap()
}

#[test]
fn cpk_reads_match_the_extracted_dump() {
    let (Some(game), Some(dump)) = (game_dir(), dump_dir()) else {
        eprintln!("SKIP: game install or dump missing");
        return;
    };
    let src = GameSource::open(&game, SourceOptions::default()).unwrap();
    let mut files = vec![
        src.latest("data/common/gamedata/character/", "chara_base").unwrap(),
        src.latest("data/common/gamedata/skill/", "skill_config").unwrap(),
        src.latest("data/common/gamedata/team/", "team_config").unwrap(),
        // listed as a loose (mod) file in a modded install: must come from its retail CPK
        src.latest("data/common/gamedata/soccer/", "soccer_game_config").unwrap(),
        "data/common/text/es/skill_text.cfg.bin".to_string(),
        "data/dx11/menu/200_icon/10_icon_chr/face/c01000010_l.g4tx".to_string(),
        "data/common/sound_asset/bgm_title.acb".to_string(),
    ];
    files.dedup();
    let got = src.read_many(&files);
    for f in &files {
        let ours = got[f].as_ref().unwrap_or_else(|e| panic!("{f}: {e}"));
        let theirs = std::fs::read(dump.join(f)).unwrap_or_else(|e| panic!("dump {f}: {e}"));
        assert_eq!(ours.len(), theirs.len(), "{f}: size");
        assert!(ours == &theirs, "{f}: bytes differ from the dump");
    }
    let st = src.stats.lock().unwrap().clone();
    assert_eq!(st.from_loose, 0, "everything must come from CPKs: {:?}", st.loose_files);
}

#[test]
fn mark_evans() {
    let idx = need_index!();
    let m = idx.get_in(Category::Character, "c01000010").expect("c01000010");
    assert_eq!(m.name(en()), Some("Mark Evans"));
    assert_eq!(m.name(es()), Some("Mark Evans"));
    assert_eq!(m.names.len(), 9);
    assert!(m.names[0].contains('円'), "ja name: {}", m.names[0]);
    assert_eq!(m.field("position"), Some(&Val::from("GK")));
    assert_eq!(m.field("element"), Some(&Val::from("mountain")));
    assert_eq!(m.field("voice_bank"), Some(&Val::from("c01000010")));
    assert_eq!(m.hash, crc32fast::hash(b"c01000010"));
    assert!(m.links("variant").any(|l| l.id == "pc_para_c01000010"));
    // face thumbnail
    let t = idx.thumb_path(m).expect("thumb");
    let png = std::fs::read(&t).unwrap();
    assert_eq!(&png[1..4], b"PNG");
    // base variant
    let v = idx.get_in(Category::Variant, "pc_para_c01000010").expect("variant");
    assert_eq!(idx.display_name(v, en()), "Mark Evans (GK · mountain)");
    assert!(v.field("skills").and_then(Val::as_list).is_some_and(|s| !s.is_empty()));
    // by key number too
    assert_eq!(idx.get_in(Category::Character, &format!("0x{:08X}", m.hash)).map(|e| e.id.as_str()), Some("c01000010"));
    // readable name -> id: several characters are called "Mark Evans" -> ambiguous, id -> unique
    match idx.resolve("Mark Evans", &[Category::Character], Some(en())) {
        Resolution::Ambiguous { candidates } => assert!(candidates.iter().any(|c| c.id == "c01000010")),
        r => panic!("expected ambiguous, got {r:?}"),
    }
    assert_eq!(idx.resolve("c01000010", &[Category::Character], None).unique().map(|c| c.id.as_str()), Some("c01000010"));
    // without a category the id is also a voice bank -> ambiguous
    assert!(matches!(idx.resolve("c01000010", &[], None), Resolution::Ambiguous { .. }));
    // search in Japanese reading and with a typo
    assert!(idx.search(&Query::new("mrak evnas").category(Category::Character)).iter().any(|h| h.id == "c01000010"));
}

#[test]
fn technique_whs01980() {
    let idx = need_index!();
    let t = idx.get_in(Category::Technique, "whs01980").expect("whs01980");
    assert_eq!(t.name(es()), Some("Disparo doble"));
    assert_eq!(t.name(en()), Some("Fission and Fusion"));
    assert_eq!(t.field("type"), Some(&Val::from("shoot")));
    assert_eq!(t.field("tp").and_then(Val::as_i64), Some(60));
    assert_eq!(t.field("se_cue"), Some(&Val::from("ev60_01980_me")));
    let d = t.field("desc_text").and_then(Val::as_i64).unwrap() as u32;
    assert!(idx.text("skill_text", d, es()).is_some_and(|s| !s.is_empty()));
    assert_eq!(idx.resolve("Disparo doble", &[Category::Technique], None).unique().map(|c| c.id.as_str()), Some("whs01980"));
    let hits = idx.search(&Query::new("tornado fuego").category(Category::Technique).lang(es()));
    assert_eq!(hits.first().map(|h| h.id.as_str()), Some("whs00030"), "{hits:?}");
}

#[test]
fn team_and_links() {
    let idx = need_index!();
    let t = idx.get_in(Category::Team, "tm_st_game_0101a").expect("team");
    assert_eq!(t.name(en()), Some("Northbright"));
    let members = t.field("members").and_then(Val::as_list).unwrap();
    assert_eq!(members.len(), 18);
    assert!(idx.related(t, "formation").len() == 1 && idx.related(t, "emblem").len() == 1);
    assert!(t.thumb.is_some(), "team thumbnail = its emblem");
    assert!(idx.related(t, "member").iter().all(|v| v.category == Category::Variant));
    // Raimon: many teams share the name
    assert!(matches!(idx.resolve("Raimon", &[Category::Team], Some(en())), Resolution::Ambiguous { .. }));
    // backlinks: the teams where Mark Evans's base variant plays
    assert!(!idx.backlinks(Category::Variant, "pc_para_c01000010").is_empty());
}

#[test]
fn other_categories() {
    let idx = need_index!();
    for (c, min) in [
        (Category::Character, 7000),
        (Category::Variant, 6000),
        (Category::Technique, 1000),
        (Category::Keshin, 40),
        (Category::Armour, 150),
        (Category::Miximax, 50),
        (Category::Passive, 1500),
        (Category::Team, 700),
        (Category::Formation, 100),
        (Category::Item, 1000),
        (Category::Emblem, 300),
        (Category::Kit, 600),
        (Category::Map, 150),
        (Category::Match, 400),
        (Category::Bgm, 100),
        (Category::Se, 3000),
        (Category::VoiceBank, 4000),
        (Category::Menu, 375),
    ] {
        let n = idx.list(c).count();
        assert!(n >= min, "{c}: {n} < {min}");
    }
    // a match with its stadium and opponent
    let m = idx.get_in(Category::Match, "fbtl_st_0101").expect("match");
    assert_eq!(m.field("game_type"), Some(&Val::from("full")));
    assert_eq!(idx.related(m, "stadium").len(), 1);
    assert!(!idx.related(m, "own_team").is_empty());
    // most matches name their rival team (a few rivals are not SOCCER_TEAM_INFO rows)
    let all = idx.list(Category::Match).count();
    let with = idx.list(Category::Match).filter(|m| m.links("opponent").next().is_some()).count();
    eprintln!("matches with a resolved opponent: {with}/{all}");
    assert!(with * 10 >= all * 8, "{with}/{all}");
    // menus by name hash
    assert!(idx.get_in(Category::Menu, "title_menu").is_some_and(|e| e.hash == crc32fast::hash(b"title_menu")));
    // keshin with its icon, armour linked to its keshin body
    assert!(idx.list(Category::Keshin).any(|k| k.thumb.is_some()));
    assert!(idx.list(Category::Armour).all(|a| a.links("aura_chara").next().is_some()));
    // text tables in 9 languages
    let st = idx.text_table("skill_text").expect("skill_text");
    assert!(st.len() > 2000);
    assert!(idx.text_files().count() >= 40);
}

#[test]
fn saved_index_round_trip_and_freshness() {
    let idx = need_index!();
    let game = game_dir().unwrap();
    assert_eq!(Index::freshness(&game, out_dir(), &SourceOptions::default()), Freshness::Fresh);
    assert_eq!(Index::freshness(&game, out_dir(), &SourceOptions { include_mods: true, ..Default::default() }), Freshness::OtherSource);
    let t0 = std::time::Instant::now();
    let loaded = Index::load(out_dir()).unwrap();
    eprintln!("load: {:?}", t0.elapsed());
    assert_eq!(loaded.entities().len(), idx.entities().len());
    assert_eq!(loaded.meta().fingerprint, idx.meta().fingerprint);
    assert_eq!(loaded.meta().game_version.as_deref(), Some("7.1.2"));
    let a = loaded.get_in(Category::Technique, "whs01980").unwrap();
    assert_eq!(a.name(es()), Some("Disparo doble"));
    assert_eq!(loaded.text("skill_text", a.field("desc_text").and_then(Val::as_i64).unwrap() as u32, es()), idx.text("skill_text", a.field("desc_text").and_then(Val::as_i64).unwrap() as u32, es()));
    let t = std::time::Instant::now();
    for _ in 0..5 {
        loaded.search(&Query::new("fuego").lang(es()));
    }
    eprintln!("5 searches: {:?}", t.elapsed());
}

