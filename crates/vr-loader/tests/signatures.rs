//! Static check of every loader signature against the v7.1.2 dump `nie.exe` (read-only).
//! Path: env `EVT_NIE_EXE`, else the default dump location; the test is skipped when the file is absent.

use vr_loader::pe::parse_headers;
use vr_loader::scan::Pattern;
use vr_loader::sigs;

const DEFAULT_DUMP: &str =
    concat!(env!("CARGO_MANIFEST_DIR"), "/../../assets/v7.1.2/nie.exe");

fn load() -> Option<Vec<u8>> {
    let p = std::env::var("EVT_NIE_EXE").unwrap_or_else(|_| DEFAULT_DUMP.to_string());
    std::fs::read(&p).ok()
}

#[test]
fn every_signature_is_unique_at_expected_rva() {
    let Some(file) = load() else {
        eprintln!("nie.exe dump not found: skipped");
        return;
    };
    let h = parse_headers(&file).expect("PE32+");
    assert!(vr_loader::gate::header_matches(h.time_date_stamp, h.size_of_image), "dump is not v7.1.2");
    let t = h.section(".text").unwrap();
    let text = &file[t.raw_off as usize..(t.raw_off + t.raw_size.min(t.vsize)) as usize];
    let rva_of = |off: usize| t.rva + off as u32;
    for s in sigs::ALL {
        let p = Pattern::parse(s.pattern).unwrap();
        let hits = p.find(text, 3);
        assert_eq!(hits.len(), 1, "{}: {} hits", s.name, hits.len());
        assert_eq!(rva_of(hits[0]) - s.offset, s.rva, "{}", s.name);
        println!("{:<28} 0x{:X} unique", s.name, s.rva);
    }
    for r in sigs::ALL_RIP {
        let p = Pattern::parse(r.sig.pattern).unwrap();
        let off = p.find_unique(text).unwrap();
        let d = off + r.disp_off as usize;
        let disp = i32::from_le_bytes(text[d..d + 4].try_into().unwrap());
        let target = (rva_of(off) as i64 + r.next_ip_off as i64 + disp as i64) as u32;
        assert_eq!(target, r.rva, "{}", r.name);
        println!("{:<28} 0x{:X}", r.name, r.rva);
    }
    // inline-hook steal bytes must be fixed (no relative operands) and at least one absolute jump long
    for (s, steal) in sigs::INLINE_HOOKS {
        let p = Pattern::parse(s.pattern).unwrap();
        assert!(*steal >= 14, "{}: steal {steal} < 14", s.name);
        assert!(p.fixed_prefix(*steal).is_some(), "{}: steal bytes contain wildcards", s.name);
    }
}

#[test]
fn command_hashes_are_frozen() {
    let h = |s: &str| crc32fast::hash(s.as_bytes());
    assert_eq!(h("CMND_EVT_LOADER_VERSION"), 0x842A418F);
    assert_eq!(h("CMND_EVT_LOG"), 0xDA3F4763);
}
