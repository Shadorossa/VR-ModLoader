//! All modules enabled together: every signature and RIP reference of every module must resolve on the clean dump
//! `.text` (that is what `Text::prime_all` does in DllMain before the first patch), and the values must be the
//! expected RVAs. Then every inline hook of every module is applied to a copy of `.text`: the test shows which
//! signatures a late scan would lose — the loader never re-scans, it reads the
//! primed cache. Read-only; skipped when the dump is absent (env `EVT_NIE_EXE` or the default path).

use vr_loader::pe::parse_headers;
use vr_loader::registry::{self, rip_key, sig_key};

const DEFAULT_DUMP: &str =
    concat!(env!("CARGO_MANIFEST_DIR"), "/../../assets/v7.1.2/nie.exe");

fn load() -> Option<Vec<u8>> {
    let p = std::env::var("EVT_NIE_EXE").unwrap_or_else(|_| DEFAULT_DUMP.to_string());
    std::fs::read(&p).ok()
}

#[test]
fn every_module_resolves_before_any_patch() {
    let Some(file) = load() else {
        eprintln!("nie.exe dump not found: skipped");
        return;
    };
    let h = parse_headers(&file).expect("PE32+");
    assert!(vr_loader::gate::header_matches(h.time_date_stamp, h.size_of_image), "dump is not v7.1.2");
    let t = h.section(".text").unwrap();
    let text = file[t.raw_off as usize..(t.raw_off + t.raw_size.min(t.vsize)) as usize].to_vec();

    // 1. clean .text: everything resolves, at the expected RVA
    let clean = registry::resolve_all(&text, t.rva);
    for s in registry::all_sigs() {
        let off = clean.sigs[&sig_key(s)].clone().unwrap_or_else(|e| panic!("{}: {e}", s.name));
        assert_eq!(t.rva + off as u32, s.rva, "{}", s.name);
    }
    for r in registry::all_rips() {
        let v = clean.rips[&rip_key(r)].clone().unwrap_or_else(|e| panic!("{}: {e}", r.name));
        assert_eq!(v, r.rva, "{}", r.name);
    }

    // 2. apply every module's inline hook (14-byte absolute jump + nops over the stolen bytes)
    let mut patched = text.clone();
    for (s, steal) in registry::all_hooks() {
        let off = clean.sigs[&sig_key(s)].clone().unwrap();
        let mut j = vec![0x90u8; steal];
        j[..6].copy_from_slice(&[0xFF, 0x25, 0, 0, 0, 0]);
        j[6..14].copy_from_slice(&0x7FF0_0000_0000u64.to_le_bytes());
        patched[off..off + steal].copy_from_slice(&j);
    }
    let late = registry::resolve_all(&patched, t.rva);
    let lost: Vec<&str> =
        registry::all_sigs().into_iter().filter(|s| late.sigs[&sig_key(s)].is_err()).map(|s| s.name).collect();
    println!("signatures a scan after all patches would lose: {lost:?}");
    // RIP references read through a patched prologue would change: the primed value is the one in use
    for r in registry::all_rips() {
        let a = clean.rips[&rip_key(r)].clone().unwrap();
        if late.rips[&rip_key(r)].as_ref().ok() != Some(&a) {
            println!("reference {} would be wrong after the patches (primed value 0x{a:X} kept)", r.name);
        }
    }
}
