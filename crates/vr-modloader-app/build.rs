//! Optional ModLoader payload built into the exe: `VRML_LOADER_PAYLOAD=<path to a zip>` (a plain ModLoader folder
//! zipped: winmm.dll, steam_appid.txt, evt_loader\**, modloader.toml; see make_payload.ps1). Without it the exe
//! carries no ModLoader and looks for modloader\ / modloader.zip next to itself.
use std::path::PathBuf;

fn main() {
    println!("cargo:rerun-if-env-changed=VRML_LOADER_PAYLOAD");
    let out = PathBuf::from(std::env::var("OUT_DIR").unwrap()).join("loader_payload.zip");
    match std::env::var("VRML_LOADER_PAYLOAD") {
        Ok(p) if !p.trim().is_empty() => {
            println!("cargo:rerun-if-changed={p}");
            std::fs::copy(&p, &out).unwrap_or_else(|e| panic!("VRML_LOADER_PAYLOAD {p}: {e}"));
        }
        _ => std::fs::write(&out, b"").unwrap(),
    }
    #[cfg(windows)]
    embed_icon_manifest();
}

/// Windows: long-path-aware, per-monitor DPI manifest is left to the default; nothing to embed for now.
#[cfg(windows)]
fn embed_icon_manifest() {}
