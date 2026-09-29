//! `VR-ModLoader.exe`: the public mod manager (sections «Mods», «Studio», «More»; newspaper-dark look).
//!
//! ```text
//! VR-ModLoader.exe [--game-dir <folder>] [--lang en|es] [--tab mods|studio|more] [--section <more section>]
//!                  [--select <mod id>] [<vrmodloader:link> | <archive.zip>]
//! ```
//! `--section`: conflicts, problems, log, profiles, game, links, trash, language, about (implies `--tab more`;
//! the old `--tab settings` opens «More» on «Game & ModLoader»).
//! `--game-dir` is not saved (safe manual tests on a copy of the game); a `vrmodloader:` link is what Windows
//! passes for a 1-click install; an archive path is what «Open with» / dropping on the exe passes.
#![cfg_attr(not(test), windows_subsystem = "windows")]

mod theme;
mod ui;

use eframe::egui;
use vr_modloader_app::APP_NAME;

fn parse_args() -> ui::Args {
    let mut a = ui::Args::default();
    let mut it = std::env::args().skip(1);
    while let Some(x) = it.next() {
        match x.as_str() {
            "--game-dir" | "--game" => a.game_dir = it.next().map(Into::into),
            "--lang" => a.lang = it.next(),
            "--tab" => a.tab = it.next(),
            "--select" => a.select = it.next(),
            "--section" => a.section = it.next(),
            _ if !x.starts_with("--") && a.open.is_none() => a.open = Some(x),
            _ => {}
        }
    }
    a
}

fn main() -> eframe::Result {
    let args = parse_args();
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title(format!("{APP_NAME} {}", vr_modloader_app::APP_VERSION))
            .with_inner_size([1200.0, 780.0])
            .with_min_inner_size([920.0, 560.0])
            .with_drag_and_drop(true)
            .with_icon(ui::icon()),
        ..Default::default()
    };
    eframe::run_native(APP_NAME, options, Box::new(|cc| Ok(Box::new(ui::ManagerApp::new(cc, args)))))
}
