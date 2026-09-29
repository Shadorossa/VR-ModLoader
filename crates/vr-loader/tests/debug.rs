//! Module `debug`: static checks against the v7.1.2 dump `nie.exe` (read-only; skipped when absent).
//! Path: env `EVT_NIE_EXE`, else the default dump location.

use vr_loader::debug::{sigs, x64len};
use vr_loader::pe::parse_headers;
use vr_loader::scan::Pattern;

const DEFAULT_DUMP: &str =
    concat!(env!("CARGO_MANIFEST_DIR"), "/../../assets/v7.1.2/nie.exe");

fn load() -> Option<Vec<u8>> {
    let p = std::env::var("EVT_NIE_EXE").unwrap_or_else(|_| DEFAULT_DUMP.to_string());
    std::fs::read(&p).ok()
}

#[test]
fn signatures_are_unique_and_hooks_relocatable() {
    let Some(file) = load() else {
        eprintln!("nie.exe dump not found: skipped");
        return;
    };
    let h = parse_headers(&file).expect("PE32+");
    assert!(vr_loader::gate::header_matches(h.time_date_stamp, h.size_of_image), "dump is not v7.1.2");
    let t = h.section(".text").unwrap();
    let text = &file[t.raw_off as usize..(t.raw_off + t.raw_size.min(t.vsize)) as usize];
    for s in sigs::ALL {
        let p = Pattern::parse(s.pattern).unwrap();
        let hits = p.find(text, 3);
        assert_eq!(hits.len(), 1, "{}: {} hits", s.name, hits.len());
        assert_eq!(t.rva + hits[0] as u32 - s.offset, s.rva, "{}", s.name);
    }
    for (s, steal) in sigs::INLINE_HOOKS {
        let p = Pattern::parse(s.pattern).unwrap();
        assert!(*steal >= 14 && p.fixed_prefix(*steal).is_some(), "{}: steal", s.name);
        let off = (s.rva - t.rva) as usize;
        // the decoder agrees: exactly `steal` bytes of whole, position-independent instructions
        assert_eq!(x64len::relocatable_prefix(&text[off..off + 64], 14), Ok(*steal), "{}", s.name);
    }
    // the two engine chunk loaders (script object 0x4D6B20, INCLUDE 0x177C740) call the hooked function
    for call in [0x4D6B6Eu32, 0x177C883] {
        let o = (call - t.rva) as usize;
        assert_eq!(text[o], 0xE8);
        let d = i32::from_le_bytes(text[o + 1..o + 5].try_into().unwrap());
        assert_eq!((call as i64 + 5 + d as i64) as u32, sigs::DBG_LOADBUFFERX.rva);
    }
}
