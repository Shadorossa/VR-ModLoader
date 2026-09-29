//! Byte-exact round trip over the real game dump (read-only).
//!
//! ```text
//! set VR_GAME_DATA=...\Extracted\data
//! cargo test -p l5-core --release --test game_data -- --ignored --nocapture
//! ```
//! Walks every `*.cfg.bin`, `.objbin`, `.ptlb`, `.fxbin`, `.mevbin`, `.clobin`, `.linb`
//! (skipping the AES-encrypted `cpk_list.cfg.bin`) and checks, per file:
//! parse → write == original bytes; JSON → model → write == original bytes;
//! for T2B also that regenerating every `__SORT_INDEX` changes nothing and that the
//! counted tree covers every entry exactly once.

use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering::Relaxed};

use l5_core::t2b::{Node, TreeMode, Value, build_tree};
use l5_core::{CfgBin, FileKind, detect};
use rayon::prelude::*;

const DEFAULT_ROOT: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../assets/v7.1.2/data");
const EXTS: [&str; 7] = [
    ".cfg.bin", ".objbin", ".ptlb", ".fxbin", ".mevbin", ".clobin", ".linb",
];

fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    for e in rd.flatten() {
        let p = e.path();
        match e.file_type() {
            Ok(t) if t.is_dir() => walk(&p, out),
            Ok(t) if t.is_file() => {
                let name = p
                    .file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or("")
                    .to_ascii_lowercase();
                if name != "cpk_list.cfg.bin" && EXTS.iter().any(|x| name.ends_with(x)) {
                    out.push(p);
                }
            }
            _ => {}
        }
    }
}

fn is_begin(name: &str) -> bool {
    name == "PTREE"
        || ["_BEGIN", "_BEG", "_BGN"].iter().any(|s| name.contains(s))
        || name.ends_with('_')
}

/// (count == children, count != children) over BEGIN nodes whose first value is an int.
fn count_blocks(nodes: &[Node], doc: &l5_core::T2b, acc: &mut (usize, usize), seen: &mut [u8]) {
    for n in nodes {
        seen[n.entry] += 1;
        n.end
            .iter()
            .chain(n.sort_index.iter().flatten())
            .for_each(|&i| seen[i] += 1);
        let e = &doc.entries[n.entry];
        if let (Some(name), Some(Value::Int(c))) = (&e.name, e.values.first())
            && is_begin(name)
        {
            if *c as usize == n.children.len() {
                acc.0 += 1
            } else {
                acc.1 += 1
            }
        }
        count_blocks(&n.children, doc, acc, seen);
    }
}

#[test]
#[ignore = "needs the extracted game dump (VR_GAME_DATA)"]
fn round_trip_game_dump() {
    let root = std::env::var("VR_GAME_DATA").unwrap_or_else(|_| DEFAULT_ROOT.to_owned());
    let root = PathBuf::from(root);
    if !root.is_dir() {
        eprintln!("skipping: {} not found (set VR_GAME_DATA)", root.display());
        return;
    }
    let mut files = Vec::new();
    walk(&root, &mut files);

    let t2b_ok = AtomicUsize::new(0);
    let t2b_n = AtomicUsize::new(0);
    let rdbn_ok = AtomicUsize::new(0);
    let rdbn_n = AtomicUsize::new(0);
    let other = AtomicUsize::new(0);
    let json_ok = AtomicUsize::new(0);
    let sort_ok = AtomicUsize::new(0);
    let sort_lists = AtomicUsize::new(0);
    let tree_ok = AtomicUsize::new(0);
    let blocks = Mutex::new((0usize, 0usize));
    let trailing = Mutex::new(Vec::new());
    let failures = Mutex::new(Vec::new());
    let unknown = Mutex::new(Vec::new());
    let by_ext = Mutex::new(std::collections::BTreeMap::<&str, [usize; 4]>::new());
    let sort_entries = AtomicUsize::new(0);
    let sort_collected = AtomicUsize::new(0);

    files.par_iter().for_each(|p| {
        let rel = p.strip_prefix(&root).unwrap_or(p).display().to_string();
        let data = match std::fs::read(p) {
            Ok(d) => d,
            Err(e) => {
                return failures
                    .lock()
                    .unwrap()
                    .push(format!("{rel}: read error {e}"));
            }
        };
        let kind = detect(&data);
        let lname = rel.to_ascii_lowercase();
        let ext = EXTS
            .iter()
            .copied()
            .find(|x| lname.ends_with(x))
            .unwrap_or("?");
        let ext_slot = match kind {
            Some(FileKind::T2b) => 0,
            Some(FileKind::Rdbn) => 2,
            None => 3,
        };
        by_ext.lock().unwrap().entry(ext).or_default()[ext_slot] += 1;
        match kind {
            Some(FileKind::T2b) => t2b_n.fetch_add(1, Relaxed),
            Some(FileKind::Rdbn) => rdbn_n.fetch_add(1, Relaxed),
            None => {
                other.fetch_add(1, Relaxed);
                unknown.lock().unwrap().push(rel);
                return;
            }
        };
        let doc = match CfgBin::parse(&data) {
            Ok(d) => d,
            Err(e) => {
                return failures
                    .lock()
                    .unwrap()
                    .push(format!("{rel}: parse error {e}"));
            }
        };
        match doc.to_bytes() {
            Ok(b) if b == data => {
                match kind {
                    Some(FileKind::T2b) => t2b_ok.fetch_add(1, Relaxed),
                    _ => rdbn_ok.fetch_add(1, Relaxed),
                };
            }
            Ok(b) => failures.lock().unwrap().push(format!(
                "{rel}: DIFF ({} vs {} bytes)",
                b.len(),
                data.len()
            )),
            Err(e) => failures
                .lock()
                .unwrap()
                .push(format!("{rel}: write error {e}")),
        }
        // JSON path used by the frontend
        let json = serde_json::to_vec(&doc).expect("serialise");
        match serde_json::from_slice::<CfgBin>(&json).map(|d| d.to_bytes()) {
            Ok(Ok(b)) if b == data => {
                json_ok.fetch_add(1, Relaxed);
            }
            other => failures.lock().unwrap().push(format!(
                "{rel}: JSON round trip failed ({})",
                match other {
                    Err(e) => e.to_string(),
                    Ok(Err(e)) => e.to_string(),
                    Ok(Ok(_)) => "bytes differ".into(),
                }
            )),
        }
        match &doc {
            CfgBin::T2b(t) => {
                let tree = build_tree(&t.entries, TreeMode::Counted);
                let mut seen = vec![0u8; t.entries.len()];
                let mut acc = (0, 0);
                count_blocks(&tree, t, &mut acc, &mut seen);
                fn collected(n: &[Node]) -> usize {
                    n.iter()
                        .map(|x| x.sort_index.as_ref().map_or(0, Vec::len) + collected(&x.children))
                        .sum()
                }
                sort_collected.fetch_add(collected(&tree), Relaxed);
                sort_entries.fetch_add(
                    t.entries
                        .iter()
                        .filter(|e| e.name.as_deref() == Some("__SORT_INDEX"))
                        .count(),
                    Relaxed,
                );
                if seen.iter().all(|&s| s == 1) {
                    tree_ok.fetch_add(1, Relaxed);
                } else {
                    failures
                        .lock()
                        .unwrap()
                        .push(format!("{rel}: tree does not cover every entry once"));
                }
                {
                    let mut b = blocks.lock().unwrap();
                    b.0 += acc.0;
                    b.1 += acc.1;
                }
                let mut t2 = t.clone();
                match t2.rebuild_sort_indexes() {
                    Ok(n) => {
                        sort_lists.fetch_add(n, Relaxed);
                        if t2.entries == t.entries {
                            sort_ok.fetch_add(1, Relaxed);
                        } else {
                            failures
                                .lock()
                                .unwrap()
                                .push(format!("{rel}: __SORT_INDEX regeneration differs"));
                        }
                    }
                    Err(e) => failures
                        .lock()
                        .unwrap()
                        .push(format!("{rel}: sort rebuild error {e}")),
                }
            }
            CfgBin::Rdbn(r) => {
                if !r.trailing_strings.is_empty() {
                    trailing
                        .lock()
                        .unwrap()
                        .push(format!("{rel} ({} bytes)", r.trailing_strings.len()));
                }
            }
        }
    });

    let (t2b_ok, t2b_n) = (t2b_ok.into_inner(), t2b_n.into_inner());
    let (rdbn_ok, rdbn_n) = (rdbn_ok.into_inner(), rdbn_n.into_inner());
    let blocks = blocks.into_inner().unwrap();
    let mut failures = failures.into_inner().unwrap();
    failures.sort();
    println!("files scanned:            {}", files.len());
    println!("T2B byte-exact:           {t2b_ok} / {t2b_n}");
    println!("RDBN byte-exact:          {rdbn_ok} / {rdbn_n}");
    println!(
        "JSON round trip exact:    {} / {}",
        json_ok.into_inner(),
        t2b_n + rdbn_n
    );
    println!(
        "T2B tree covers all:      {} / {t2b_n}",
        tree_ok.into_inner()
    );
    println!(
        "T2B __SORT_INDEX regen:   {} / {t2b_n} files unchanged ({} indexed lists)",
        sort_ok.into_inner(),
        sort_lists.into_inner()
    );
    println!(
        "per extension [T2B, -, RDBN, other]: {:?}",
        by_ext.into_inner().unwrap()
    );
    println!(
        "__SORT_INDEX entries:     {} total, {} collected into list sort indexes",
        sort_entries.into_inner(),
        sort_collected.into_inner()
    );
    println!(
        "T2B counted blocks:       {} count==children, {} not",
        blocks.0, blocks.1
    );
    println!(
        "RDBN with trailing pool:  {:?}",
        trailing.into_inner().unwrap()
    );
    println!(
        "unrecognised files:       {} {:?}",
        other.into_inner(),
        unknown.into_inner().unwrap()
    );
    for f in failures.iter().take(50) {
        println!("FAIL {f}");
    }
    assert!(failures.is_empty(), "{} failure(s)", failures.len());
    assert_eq!(t2b_ok, t2b_n);
    assert_eq!(rdbn_ok, rdbn_n);
}
