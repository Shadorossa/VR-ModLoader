//! `evt-text-engine`: offline helper of the text engine (game closed).
//!
//! ```text
//! evt-text-engine prepare <game folder>   merge the texts of every active mod and write the served files (may resize
//!                                         them: the next start shows every text, no extra restart)
//! evt-text-engine check <mod folder>      parse a mod's text files: warnings, new keys and their ids, replace keys
//! ```

use std::path::{Path, PathBuf};
use std::process::ExitCode;
use text_engine::boot::{self, BootIn};
use text_engine::decl::{self, ModText};
use text_engine::fw::game::GameSource;
use text_engine::fw::{discover, Lvl, ModDir};
use text_engine::keys;

const ENGINE_ID: &str = "text_engine";

fn prepare(game: &Path) -> Result<bool, String> {
    let mods_dir = game.join("mods");
    let plan = evt_modfmt::plan_root_for(&mods_dir, None);
    let engine = plan
        .mods
        .iter()
        .find(|m| m.manifest.id == ENGINE_ID || m.manifest.provides.iter().any(|p| p.split('=').next() == Some(ENGINE_ID)))
        .ok_or_else(|| format!("no active mod provides {ENGINE_ID} in {}", mods_dir.display()))?;
    let self_id = engine.manifest.id.clone();
    let self_dir = engine.dir.clone();
    let mods: Vec<ModDir> = plan.mods.iter().enumerate().map(|(i, m)| ModDir { id: m.manifest.id.clone(), dir: m.dir.clone(), load_index: i as u32 }).collect();
    let inactive: Vec<ModDir> = discover::installed(&mods_dir)
        .into_iter()
        .filter(|(id, _)| !mods.iter().any(|m| m.id == *id))
        .map(|(id, dir)| ModDir { id, dir, load_index: 0 })
        .collect();
    let cfg_text = std::fs::read_to_string(self_dir.join("config.toml")).unwrap_or_default();
    let (cfg, _) = text_engine::Cfg::from_text(&cfg_text);
    let loader = game.join("evt_loader");
    let inp = BootIn { self_id: &self_id, self_dir: &self_dir, loader_dir: &loader, mods: &mods, inactive: &inactive, policy: cfg.policy(false) };
    let out = boot::run(&inp, &mut GameSource::new(game));
    let mut errors = false;
    for (l, s) in &out.notes.0 {
        errors |= *l == Lvl::Error;
        println!("{:5} {s}", format!("{l:?}").to_uppercase());
    }
    println!(
        "{} mod(s) with texts ({}); {} served file(s); {} new key(s){}",
        out.text_mods.len(),
        out.text_mods.join(", "),
        out.slots.len(),
        out.index.keys.len(),
        if out.from_cache { " (already up to date)" } else { "" }
    );
    for (k, v) in &out.index.keys {
        println!("  {k} = {} ({:#010X}) in {}", v.id, v.id, v.table);
    }
    Ok(!errors)
}

fn check(dir: &Path) -> Result<bool, String> {
    let id = dir.file_name().map(|n| n.to_string_lossy().into_owned()).ok_or("bad folder")?;
    let mut files = Vec::new();
    let mut ok = true;
    for r in discover::read(dir, decl::NAME) {
        match r {
            Ok(f) => files.push(f),
            Err(e) => {
                println!("ERROR {e}");
                ok = false;
            }
        }
    }
    if files.is_empty() {
        println!("no text.toml / text\\*.toml in {}", dir.display());
        return Ok(ok);
    }
    let m = ModText::parse(&id, &files);
    for w in &m.warnings {
        println!("WARN  {w}");
    }
    println!("default language: {}", m.default_lang.unwrap_or("none"));
    let news = m.new_names();
    for n in &news {
        let full = decl::full_key(&id, n);
        let (t, k, w) = m.new_meta(n);
        for x in w {
            println!("WARN  {x}");
        }
        let crc = l5_core_crc(&full);
        println!("new      {full} ({t}, {k:?}): id {crc} ({crc:#010X}) unless it collides (the game log says so)");
    }
    let mine = |k: &str| news.iter().any(|n| decl::full_key(&id, n) == k);
    for k in m.replace_keys() {
        match keys::parse_key(k, &mine) {
            Ok(r) => println!("replace  {k}: {r:?}"),
            Err(e) => {
                println!("WARN  replace `{k}`: {e}");
                ok = false;
            }
        }
    }
    Ok(ok && m.warnings.is_empty())
}

fn l5_core_crc(s: &str) -> u32 {
    crc32fast::hash(s.as_bytes())
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let r = match (args.first().map(String::as_str), args.get(1)) {
        (Some("prepare"), Some(g)) => prepare(&PathBuf::from(g)),
        (Some("check"), Some(d)) => check(&PathBuf::from(d)),
        _ => {
            eprintln!("usage: evt-text-engine prepare <game folder> | check <mod folder>");
            return ExitCode::from(2);
        }
    };
    match r {
        Ok(true) => ExitCode::SUCCESS,
        Ok(false) => ExitCode::from(1),
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::from(1)
        }
    }
}
