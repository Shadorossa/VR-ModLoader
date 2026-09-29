//! `evt-mod`: command-line tool for mod folders (docs/game/engine/mod-format.md).
//!
//! ```text
//! evt-mod new <id> [<parent dir>]    create <parent>/<id>/ with mod.toml, lua/, files/data/, data/
//! evt-mod check <dir>                validate one mod (folder with mod.toml) or a whole mods folder; list conflicts
//! evt-mod list <game dir>            load plan of <game>/mods (order, enabled, skipped, conflicts)
//! evt-mod pack <dir> <out>           validate, then copy the mod's files into <out>/<id>/ (no zip crate in the
//!                                    workspace: the package is a clean folder, zip it with any tool)
//! ```
//! Exit code: 0 ok, 1 validation errors, 2 usage / I/O error.

use evt_modfmt::{self as fmt, LoadPlan, Severity};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

const USAGE: &str = "usage:
  evt-mod new <id> [<parent dir>]
  evt-mod check <mod dir | mods dir>
  evt-mod list <game dir | mods dir>
  evt-mod pack <mod dir> <out dir>";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let a: Vec<&str> = args.iter().map(String::as_str).collect();
    let r = match a.as_slice() {
        ["new", id] => new(id, Path::new(".")),
        ["new", id, parent] => new(id, Path::new(parent)),
        ["check", dir] => check(Path::new(dir)),
        ["list", dir] => list(Path::new(dir)),
        ["pack", dir, out] => pack(Path::new(dir), Path::new(out)),
        _ => Err(USAGE.to_string()),
    };
    match r {
        Ok(true) => ExitCode::SUCCESS,
        Ok(false) => ExitCode::from(1),
        Err(e) => {
            eprintln!("{e}");
            ExitCode::from(2)
        }
    }
}

fn new(id: &str, parent: &Path) -> Result<bool, String> {
    fmt::validate_id(id)?;
    let dir = parent.join(id);
    if dir.exists() {
        return Err(format!("{} already exists", dir.display()));
    }
    for d in [fmt::LUA_DIR, &format!("{}/data", fmt::FILES_DIR), fmt::DATA_DIR] {
        std::fs::create_dir_all(dir.join(d)).map_err(|e| format!("{}: {e}", dir.join(d).display()))?;
    }
    std::fs::write(dir.join(fmt::MANIFEST), fmt::manifest_template(id)).map_err(|e| e.to_string())?;
    println!("created {}", dir.display());
    println!("  lua/<script>/*.lua       Lua patches (script stem, e.g. lua/title_menu/10_x.lua)");
    println!("  files/data/...           whole-file overrides / new files (logical game path)");
    println!("  data/<table>.toml        cell deltas ([[set]] / [[add]]; validated, applied in a later loader version)");
    Ok(true)
}

fn print_issues(issues: &[fmt::Issue]) {
    for i in issues {
        println!("  {i}");
    }
}

fn print_plan(plan: &LoadPlan) {
    println!("load order ({} enabled):", plan.mods.len());
    for (n, m) in plan.mods.iter().enumerate() {
        let deltas: usize = m.deltas.iter().map(|d| d.delta.set.len() + d.delta.add.len()).sum();
        println!(
            "  {:>2}. {:<24} prio {:>4}  lua {:>2} script(s)  files {:>4}  deltas {:>4}  {}",
            n + 1,
            m.label(),
            m.manifest.priority,
            m.lua_scripts.len(),
            m.files.len(),
            deltas,
            m.manifest.name
        );
    }
    for s in &plan.skipped {
        println!("  skipped {}: {}", s.id, s.reason);
    }
    if !plan.conflicts.is_empty() {
        println!("conflicts ({}):", plan.conflicts.len());
        for c in &plan.conflicts {
            println!("  {c}");
        }
    }
    if !plan.issues.is_empty() {
        println!("problems ({}):", plan.issues.len());
        print_issues(&plan.issues);
    }
}

fn mods_root(dir: &Path) -> PathBuf {
    let m = dir.join(fmt::MODS_DIR);
    if m.is_dir() {
        m
    } else {
        dir.to_path_buf()
    }
}

fn check_one(dir: &Path) -> Result<(Option<fmt::ModInfo>, Vec<fmt::Issue>), String> {
    if !dir.join(fmt::MANIFEST).is_file() {
        return Err(format!("{}: no {}", dir.display(), fmt::MANIFEST));
    }
    Ok(fmt::scan_mod(dir))
}

fn check(dir: &Path) -> Result<bool, String> {
    if !dir.is_dir() {
        return Err(format!("{}: not a folder", dir.display()));
    }
    if dir.join(fmt::MANIFEST).is_file() {
        let (m, issues) = check_one(dir)?;
        let Some(m) = m else {
            print_issues(&issues);
            println!("INVALID");
            return Ok(false);
        };
        let plan = fmt::build_plan(vec![m], None);
        let mut plan = plan;
        plan.issues.splice(0..0, issues);
        print_plan(&plan);
        let ok = !plan.has_errors();
        println!("{}", if ok { "OK" } else { "ERRORS" });
        return Ok(ok);
    }
    let plan = fmt::plan_root(&mods_root(dir));
    print_plan(&plan);
    let ok = !plan.has_errors();
    println!("{}", if ok { "OK" } else { "ERRORS" });
    Ok(ok)
}

fn list(dir: &Path) -> Result<bool, String> {
    let root = mods_root(dir);
    if !root.is_dir() {
        return Err(format!("{}: not a folder", root.display()));
    }
    println!("{}", root.display());
    match fmt::read_enabled(&root) {
        Ok(None) => println!("enabled.toml: missing (every mod enabled)"),
        Ok(Some(v)) => println!("enabled.toml: {}", v.join(", ")),
        Err(e) => println!("enabled.toml: unreadable ({e})"),
    }
    let plan = fmt::plan_root(&root);
    print_plan(&plan);
    Ok(!plan.has_errors())
}

fn copy_tree(from: &Path, to: &Path, n: &mut usize) -> Result<(), String> {
    let rd = std::fs::read_dir(from).map_err(|e| format!("{}: {e}", from.display()))?;
    std::fs::create_dir_all(to).map_err(|e| format!("{}: {e}", to.display()))?;
    for e in rd.flatten() {
        let name = e.file_name();
        let lower = name.to_string_lossy().to_ascii_lowercase();
        if ["thumbs.db", "desktop.ini", ".ds_store"].contains(&lower.as_str()) {
            continue;
        }
        let p = e.path();
        if p.is_dir() {
            copy_tree(&p, &to.join(&name), n)?;
        } else {
            std::fs::copy(&p, to.join(&name)).map_err(|e| format!("{}: {e}", p.display()))?;
            *n += 1;
        }
    }
    Ok(())
}

fn pack(dir: &Path, out: &Path) -> Result<bool, String> {
    let (m, issues) = check_one(dir)?;
    let errors = issues.iter().any(|i| i.severity == Severity::Error);
    if errors || m.is_none() {
        print_issues(&issues);
        println!("not packed: fix the errors first");
        return Ok(false);
    }
    let m = m.unwrap();
    if out.extension().is_some_and(|e| e.eq_ignore_ascii_case("zip")) {
        return Err("zip output is not supported (no zip crate in the workspace): give an output folder".into());
    }
    let dest = out.join(m.id());
    if dest.exists() {
        return Err(format!("{} already exists", dest.display()));
    }
    let mut n = 0;
    std::fs::create_dir_all(&dest).map_err(|e| e.to_string())?;
    std::fs::copy(dir.join(fmt::MANIFEST), dest.join(fmt::MANIFEST)).map_err(|e| e.to_string())?;
    n += 1;
    for sub in [fmt::LUA_DIR, fmt::FILES_DIR, fmt::DATA_DIR] {
        if dir.join(sub).is_dir() {
            copy_tree(&dir.join(sub), &dest.join(sub), &mut n)?;
        }
    }
    // native plugin (docs/app/modloader-plugins.md), the mod's own config.toml and the README next to mod.toml
    for f in [m.manifest.plugin.as_str(), "config.toml", "README.md"] {
        if !f.is_empty() && dir.join(f).is_file() {
            std::fs::copy(dir.join(f), dest.join(f)).map_err(|e| e.to_string())?;
            n += 1;
        }
    }
    print_issues(&issues);
    println!("packed {} ({n} files) into {}", m.label(), dest.display());
    Ok(true)
}
