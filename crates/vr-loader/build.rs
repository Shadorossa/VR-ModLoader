//! Build script: winmm.dll proxy stubs + the small SEH shim.
//!
//! * `exports.txt` (checked in, generated from the system `winmm.dll`) lists every export as
//!   `ordinal name` (`-` = exported by ordinal only). For each one we emit a naked asm stub `evt_fwd_<i>` that jumps
//!   through a lazily filled table (`proxy.rs`), and a `/EXPORT:<name>=evt_fwd_<i>,@<ordinal>` linker argument so the
//!   DLL has the same names AND ordinals as the real winmm.dll.
//! * `src/seh.c` gives `__try/__except` wrappers for calls into game code (MSVC only).
use std::{env, fs, path::PathBuf};

fn main() {
    println!("cargo:rerun-if-changed=exports.txt");
    println!("cargo:rerun-if-changed=src/seh.c");
    let target = env::var("TARGET").unwrap_or_default();
    let out = PathBuf::from(env::var("OUT_DIR").unwrap());

    let text = fs::read_to_string("exports.txt").expect("exports.txt");
    let mut exports: Vec<(u32, Option<String>)> = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let mut it = line.split_whitespace();
        let ord: u32 = it.next().unwrap().parse().expect("ordinal");
        let name = it.next().expect("name");
        exports.push((ord, if name == "-" { None } else { Some(name.to_string()) }));
    }

    // Rust side: names/ordinals table used by the resolver.
    let mut rs = String::new();
    rs.push_str(&format!("pub const EXPORT_COUNT: usize = {};\n", exports.len()));
    rs.push_str("pub static EXPORTS: [(u16, &str); EXPORT_COUNT] = [\n");
    for (ord, name) in &exports {
        rs.push_str(&format!("    ({}, \"{}\"),\n", ord, name.as_deref().unwrap_or("")));
    }
    rs.push_str("];\n");

    // Asm stubs (Intel syntax). Table = `{table}` (sym operand), resolver = `{resolve}`.
    let mut asm = String::new();
    asm.push_str(".text\n");
    for (i, _) in exports.iter().enumerate() {
        asm.push_str(&format!(
            ".globl evt_fwd_{i}\n.p2align 4\nevt_fwd_{i}:\n    mov rax, qword ptr [rip + {{table}} + {off}]\n    test rax, rax\n    jz evt_res_{i}\n    jmp rax\nevt_res_{i}:\n    mov r10d, {i}\n    jmp evt_proxy_resolve_common\n",
            off = i * 8
        ));
    }
    asm.push_str(
        ".p2align 4\nevt_proxy_resolve_common:\n    push rcx\n    push rdx\n    push r8\n    push r9\n    sub rsp, 0x68\n    movdqu xmmword ptr [rsp + 0x20], xmm0\n    movdqu xmmword ptr [rsp + 0x30], xmm1\n    movdqu xmmword ptr [rsp + 0x40], xmm2\n    movdqu xmmword ptr [rsp + 0x50], xmm3\n    mov ecx, r10d\n    call {resolve}\n    movdqu xmm0, xmmword ptr [rsp + 0x20]\n    movdqu xmm1, xmmword ptr [rsp + 0x30]\n    movdqu xmm2, xmmword ptr [rsp + 0x40]\n    movdqu xmm3, xmmword ptr [rsp + 0x50]\n    add rsp, 0x68\n    pop r9\n    pop r8\n    pop rdx\n    pop rcx\n    jmp rax\n",
    );
    rs.push_str("#[cfg(all(windows, target_arch = \"x86_64\"))]\n");
    rs.push_str("core::arch::global_asm!(\n    r#\"\n");
    rs.push_str(&asm);
    rs.push_str("\"#,\n    table = sym crate::proxy::EVT_PROXY_TABLE,\n    resolve = sym crate::proxy::evt_proxy_resolve,\n);\n");
    fs::write(out.join("proxy_stubs.rs"), rs).unwrap();

    if target.contains("windows-msvc") && target.starts_with("x86_64") {
        for (i, (ord, name)) in exports.iter().enumerate() {
            match name {
                Some(n) => println!("cargo:rustc-cdylib-link-arg=/EXPORT:{n}=evt_fwd_{i},@{ord}"),
                None => println!("cargo:rustc-cdylib-link-arg=/EXPORT:evt_fwd_{i},@{ord},NONAME"),
            }
        }
        cc::Build::new().file("src/seh.c").compile("evt_seh");
    }
    table_index(&out);
}

/// Module `mods` (data deltas, `src/mods/tables.rs`): compact index of the T2B table layouts so the loader can turn
/// `character/chara_param` + a column name into a game file + a value index. One line per table:
/// `folder/id <TAB> list <TAB> key column <TAB> file globs (;) <TAB> index:name,...`. Built from a `schemas/` folder
/// (`schemas/<folder>/*.toml`) next to the workspace when there is one, else the checked-in `table_index.tsv`
/// (field layouts only: table / list names, key column, file globs, column indices and names).
fn table_index(out: &std::path::Path) {
    let root = PathBuf::from("../../schemas");
    println!("cargo:rerun-if-changed=../../schemas");
    println!("cargo:rerun-if-changed=table_index.tsv");
    if !root.is_dir() {
        let text = fs::read_to_string("table_index.tsv").unwrap_or_default();
        fs::write(out.join("table_index.tsv"), text).unwrap();
        return;
    }
    let mut lines: Vec<String> = Vec::new();
    let mut folders: Vec<PathBuf> = fs::read_dir(&root).map(|rd| rd.flatten().map(|e| e.path()).filter(|p| p.is_dir()).collect()).unwrap_or_default();
    folders.sort();
    for dir in folders {
        let folder = dir.file_name().unwrap().to_string_lossy().into_owned();
        let mut files: Vec<PathBuf> = fs::read_dir(&dir)
            .map(|rd| rd.flatten().map(|e| e.path()).filter(|p| p.extension().is_some_and(|x| x == "toml")).collect())
            .unwrap_or_default();
        files.sort();
        for f in files {
            let Ok(text) = fs::read_to_string(&f) else { continue };
            let Ok(doc) = text.parse::<toml::Table>() else { continue };
            let Some(t) = doc.get("table").and_then(|v| v.as_table()) else { continue };
            if t.get("format").and_then(|v| v.as_str()) != Some("t2b") {
                continue;
            }
            let (Some(id), Some(list)) = (t.get("id").and_then(|v| v.as_str()), t.get("list").and_then(|v| v.as_str())) else { continue };
            let key = t.get("key").and_then(|v| v.as_integer()).map(|k| k.to_string()).unwrap_or_default();
            let globs: Vec<&str> = t.get("files").and_then(|v| v.as_array()).map(|a| a.iter().filter_map(|x| x.as_str()).collect()).unwrap_or_default();
            let cols: Vec<String> = doc
                .get("column")
                .and_then(|v| v.as_array())
                .map(|a| {
                    a.iter()
                        .filter_map(|c| Some(format!("{}:{}", c.get("index")?.as_integer()?, c.get("name")?.as_str()?)))
                        .collect()
                })
                .unwrap_or_default();
            let clean = |s: &str| s.replace(['\t', '\n', '\r', ';', ','], "_");
            lines.push(format!(
                "{folder}/{id}\t{}\t{key}\t{}\t{}",
                clean(list),
                globs.iter().map(|g| clean(g)).collect::<Vec<_>>().join(";"),
                cols.iter().map(|c| c.replace(['\t', '\n', '\r'], "_")).collect::<Vec<_>>().join(",")
            ));
        }
    }
    fs::write(out.join("table_index.tsv"), lines.join("\n")).unwrap();
}
