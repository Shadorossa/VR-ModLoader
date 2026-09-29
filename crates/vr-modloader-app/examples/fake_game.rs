//! A fake game folder to try VR-ModLoader.exe without touching the real game:
//! `cargo run -p vr-modloader-app --example fake_game -- <folder>` then
//! `VR-ModLoader.exe --game-dir "<folder>\INAZUMA ELEVEN Victory Road"`.
//! Fake `nie.exe` (so «Not v7.1.2»), fake `cpk_list`, 11 mods laid out like a real install: the engine mods
//! (audio_engine, text_engine, match_engine, save_engine) and clean_hud, plus content mods with a dependency
//! that is installed but disabled (Story Plus needs clean_hud: «Won't load» until it is enabled), a
//! dependency that is too old (Side match camera needs match_engine >= 1.1, 1.0.0 installed), a too-new
//! `loader_min`, a file conflict, an audio cue conflict and a preview picture. (quit_fix is part of the loader
//! core now, not a mod.)

use std::path::Path;

fn write(p: &Path, text: &str) {
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(p, text).unwrap();
}

fn main() {
    let root = std::env::args().nth(1).expect("usage: fake_game <folder>");
    let g = Path::new(&root).join("INAZUMA ELEVEN Victory Road");
    if g.exists() {
        std::fs::remove_dir_all(&g).unwrap();
    }
    write(&g.join("nie.exe"), "fake nie.exe");
    write(&g.join("data/cpk_list.cfg.bin"), "fake");
    let mods = g.join("mods");
    let m = |id: &str, name: &str, ver: &str, author: &str, desc: &str, extra: &str, files: &[&str], audio: Option<&str>| {
        let d = mods.join(id);
        write(&d.join("mod.toml"), &format!("id = \"{id}\"\nname = \"{name}\"\nversion = \"{ver}\"\nauthor = \"{author}\"\ndescription = \"\"\"{desc}\"\"\"\n{extra}"));
        for f in files {
            write(&d.join("files").join(f), id);
        }
        if let Some(a) = audio {
            write(&d.join("audio.toml"), a);
        }
    };
    let lib = "tags = [\"Library\"]\n";
    m("audio_engine", "Audio engine", "1.1.0", "ExampleAuthor", "Music and sound effects for other mods: new tracks, replaced cues, per-scene playlists.", lib, &[], None);
    m("text_engine", "Text engine", "1.0.0", "ExampleAuthor", "Replaces and adds game texts by readable key, in every language.", lib, &[], None);
    m("match_engine", "Match engine", "1.0.0", "ExampleAuthor", "Hooks for match rules, cameras and HUD used by match mods.", lib, &[], None);
    m("save_engine", "Save engine", "1.0.0", "ExampleAuthor", "Separate save slots for mods with their own progress.", lib, &[], None);
    m("clean_hud", "Clean HUD", "1.0.0", "ExampleAuthor", "Hides the busy parts of the match HUD.", "requires = [\"match_engine>=1.0\"]\n", &[], None);
    m(
        "story_plus",
        "Story Plus",
        "0.2.0",
        "ExampleAuthor",
        "New story mode, rebalanced teams and a remastered HUD.",
        "requires = [\"audio_engine>=1.0\", \"text_engine>=1.0\", \"save_engine>=1.0\", \"clean_hud>=1.0\"]\nloader_min = \"1.0.0\"\n",
        &["data/common/ui/hud.g4tx"],
        None,
    );
    m("hd_kits", "HD Team Kits", "2.1.0", "KitMaker", "High resolution kits.", "", &["data/common/ui/hud.g4tx"], None);
    let sfx = |file: &str| format!("[[sfx]]\nreplace = \"ev60_03380_me\"\nfile = \"{file}\"\n");
    let needs_audio = "requires = [\"audio_engine>=1.0\"]\n";
    m("spanish_voices", "Voces en español", "0.9.1", "Doblaje FC", "Spanish voice pack.", needs_audio, &[], Some(&sfx("a.hca")));
    m("retro_sfx", "Retro SFX", "1.0.0", "8bit", "Old-school sound effects.", needs_audio, &[], Some(&sfx("b.hca")));
    m("side_camera", "Side match camera", "0.3.0", "ExampleAuthor", "Broadcast-style side camera for matches.", "requires = [\"match_engine>=1.1\"]\n", &[], None);
    m("future_mod", "Future Mod", "3.0.0", "Someone", "Needs a newer ModLoader.", "loader_min = \"9.0.0\"\n", &[], None);
    let img = image::RgbImage::from_fn(320, 180, |x, y| image::Rgb([(x * 255 / 320) as u8, (y * 255 / 180) as u8, 200]));
    img.save(mods.join("story_plus").join("preview.png")).unwrap();
    let order = ["story_plus", "hd_kits", "retro_sfx", "spanish_voices", "side_camera", "future_mod", "clean_hud", "audio_engine", "text_engine", "match_engine", "save_engine"];
    let quoted = |ids: &[&str]| ids.iter().map(|i| format!("\"{i}\"")).collect::<Vec<_>>().join(", ");
    let enabled: Vec<&str> = order.iter().copied().filter(|i| *i != "clean_hud").collect();
    write(&mods.join("enabled.toml"), &format!("enabled = [{}]\n", quoted(&enabled)));
    write(&mods.join("load_order.toml"), &format!("order = [{}]\n", quoted(&order)));
    println!("{}", g.display());
}
