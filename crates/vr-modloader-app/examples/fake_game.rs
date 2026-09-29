//! A fake game folder to try VR-ModLoader.exe without touching the real game:
//! `cargo run -p vr-modloader-app --example fake_game -- <folder>` then
//! `VR-ModLoader.exe --game-dir "<folder>\INAZUMA ELEVEN Victory Road"`.
//! Fake `nie.exe` (so «Not v7.1.2»), fake `cpk_list`, 7 mods: a missing dependency, a too-new `loader_min`, a file
//! conflict, an audio cue conflict, a disabled mod and a preview picture.

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
    m("vr_framework", "VR-Framework", "1.2.0", "ExampleAuthor", "Shared engines other mods build on.", "tags = [\"Library\"]\n", &[], None);
    m(
        "story_plus",
        "Story Plus",
        "0.2.0",
        "ExampleAuthor",
        "New story mode, rebalanced teams and a remastered HUD.",
        "requires = [\"vr_framework>=1.0\", \"clean_hud>=1.0\"]\nloader_min = \"1.0.0\"\n",
        &["data/common/ui/hud.g4tx"],
        None,
    );
    m("hd_kits", "HD Team Kits", "2.1.0", "KitMaker", "High resolution kits.", "", &["data/common/ui/hud.g4tx"], None);
    m("spanish_voices", "Voces en español", "0.9.1", "Doblaje FC", "Spanish voice pack.", "", &[], Some("[[sfx]]\nreplace = \"ev60_03380_me\"\nfile = \"a.hca\"\n"));
    m("retro_sfx", "Retro SFX", "1.0.0", "8bit", "Old-school sound effects.", "", &[], Some("[[sfx]]\nreplace = \"ev60_03380_me\"\nfile = \"b.hca\"\n"));
    m("future_mod", "Future Mod", "3.0.0", "Someone", "Needs a newer ModLoader.", "loader_min = \"9.0.0\"\n", &[], None);
    m("quit_fix", "Quit fix", "1.1.0", "ExampleAuthor", "Closes the game cleanly.", "", &[], None);
    let img = image::RgbImage::from_fn(320, 180, |x, y| image::Rgb([(x * 255 / 320) as u8, (y * 255 / 180) as u8, 200]));
    img.save(mods.join("story_plus").join("preview.png")).unwrap();
    write(&mods.join("enabled.toml"), "enabled = [\"story_plus\", \"vr_framework\", \"hd_kits\", \"spanish_voices\", \"retro_sfx\", \"future_mod\"]\n");
    write(
        &mods.join("load_order.toml"),
        "order = [\"story_plus\", \"hd_kits\", \"retro_sfx\", \"spanish_voices\", \"future_mod\", \"vr_framework\", \"quit_fix\"]\n",
    );
    println!("{}", g.display());
}
