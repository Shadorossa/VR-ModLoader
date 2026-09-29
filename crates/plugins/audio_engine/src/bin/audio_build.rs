//! `audio_build` — the pack-time FAST PATH of the audio engine (docs/game/media/audio-engine.md §0): the same build the
//! plugin does at boot, for one mod, written into the mod's own `files\` (XOR-encrypted, like any loose CRI file). A mod
//! that ships its built banks is not rebuilt at boot (its banks are then the base the other mods' cues go on).
//!
//! ```text
//! cargo run --release -p evt-plugin-audio-engine --bin audio_build -- --mod "<game>\mods\mi_mod"
//!     [--retail <data folder of the v7.1.2 dump>] [--index <audio_index.json>] [--force]
//! ```
//! Retail banks come from `--retail` (default: `%EVT_DUMP%\data`), the name index from `--index` (default:
//! `<mods>\audio_engine\index\audio_index.json`). Stamps (input hashes) go to
//! `<mod>\audio\.build\`; unchanged banks are skipped. Exit code 1 when an entry or a bank failed.

use audio_engine::build::{self, Env};
use audio_engine::framework::{Cache, DumpFiles, ModDir};
use audio_engine::index::Index;
use std::path::PathBuf;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let get = |k: &str| args.iter().position(|a| a == k).and_then(|i| args.get(i + 1)).cloned();
    let Some(md) = get("--mod").map(PathBuf::from) else {
        eprintln!("usage: audio_build --mod <mod folder> [--retail <dump data>] [--index <audio_index.json>] [--force]");
        std::process::exit(2);
    };
    // retail banks: --retail, else %EVT_DUMP%\data (your own extracted v7.1.2)
    let Some(retail) = get("--retail").map(PathBuf::from).or_else(|| std::env::var_os("EVT_DUMP").map(|d| PathBuf::from(d).join("data"))) else {
        eprintln!("ERROR: give --retail <data folder of your extracted v7.1.2> (or set EVT_DUMP)");
        std::process::exit(2);
    };
    let index_path = get("--index")
        .map(PathBuf::from)
        .unwrap_or_else(|| md.parent().unwrap_or(&md).join("audio_engine").join("index").join("audio_index.json"));
    let idx = Index::load(&index_path).unwrap_or_else(|e| {
        eprintln!("ERROR: {e}");
        std::process::exit(1)
    });
    let text = std::fs::read_to_string(md.join("audio.toml")).unwrap_or_else(|e| {
        eprintln!("ERROR: {}\\audio.toml: {e}", md.display());
        std::process::exit(1)
    });
    let t = audio_engine::decl::parse(&text).unwrap_or_else(|e| {
        eprintln!("ERROR: audio.toml: {e}");
        std::process::exit(1)
    });
    let id = md.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let m = ModDir { id: id.clone(), dir: md.clone(), load_index: 0 };
    let plan = build::plan_with(&[(m, t)], &idx, false);
    if args.iter().any(|a| a == "--force") {
        let _ = std::fs::remove_dir_all(md.join("audio").join(".build"));
    }
    let game = DumpFiles(retail);
    let cache = Cache::with_stamps(md.join("files"), md.join("audio").join(".build"));
    let env = Env { game: &game, mods: &[], cache: &cache };
    let t0 = std::time::Instant::now();
    let out = build::execute(&plan, &env, false);
    let mut bad = false;
    for (w, n) in plan.notes.iter().chain(out.notes.iter()) {
        println!("{}{n}", if *w { "WARN " } else { "" });
        bad |= *w;
    }
    println!(
        "{id}: {} bank(s) built, {} unchanged, in {} ms; new banks registered at boot: {}; BGM ids redirected at boot: {}",
        out.rebuilt,
        out.cached,
        t0.elapsed().as_millis(),
        plan.regs.iter().map(|r| r.name.as_str()).collect::<Vec<_>>().join(", "),
        plan.bgm_redirect.len() + plan.bgm_rows.len()
    );
    std::process::exit(if bad { 1 } else { 0 });
}
