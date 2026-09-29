//! Module `chara_legal`: static checks against the v7.1.2 dump `nie.exe` (read-only; skipped when `EVT_NIE_EXE` is
//! not set or the file is absent).

use vr_loader::chara_legal::{self, sigs, State};
use vr_loader::pe::parse_headers;
use vr_loader::scan::Pattern;

fn load() -> Option<Vec<u8>> {
    std::fs::read(std::env::var("EVT_NIE_EXE").ok()?).ok()
}

#[test]
fn site_unique_retail_and_uvr_compatible() {
    let Some(file) = load() else {
        eprintln!("EVT_NIE_EXE not set / nie.exe dump not found: skipped");
        return;
    };
    let h = parse_headers(&file).expect("PE32+");
    assert!(vr_loader::gate::header_matches(h.time_date_stamp, h.size_of_image), "dump is not v7.1.2");
    let t = h.section(".text").unwrap();
    let text = &file[t.raw_off as usize..(t.raw_off + t.raw_size.min(t.vsize)) as usize];
    let off_of = |rva: u32| (rva - t.rva) as usize;
    for s in sigs::ALL {
        let hits = Pattern::parse(s.pattern).unwrap().find(text, 3);
        assert_eq!(hits.len(), 1, "{}: {} hits", s.name, hits.len());
        assert_eq!(t.rva + hits[0] as u32, s.rva, "{}", s.name);
    }
    // UVR's SIG_FRIEND_PATCH matches once, at our site; the jz is retail
    let uvr_friend = Pattern::parse("48 85 C0 74 12 80 78 6B 00 0F 94 C0").unwrap().find(text, 3);
    assert_eq!(uvr_friend, vec![off_of(sigs::CL_CHARA_LEGAL.rva)]);
    let jcc = off_of(sigs::CL_CHARA_LEGAL.rva) + sigs::CHARA_LEGAL_JCC;
    assert_eq!(chara_legal::chara_state(text[jcc]), State::Retail);
    // the jz skips to `xor al, al` (return false): jz rel8 0x12 -> 0xE7449F
    assert_eq!(text[jcc + 1], 0x12);
    assert_eq!(&text[jcc + 2 + 0x12 + 5..jcc + 2 + 0x12 + 7], &[0x32, 0xC0]);
    // after the patch UVR's patcher finds nothing (no double patch) and our signature still matches
    let mut img = text.to_vec();
    img[jcc] = chara_legal::JMP;
    assert!(Pattern::parse("48 85 C0 74 12 80 78 6B 00 0F 94 C0").unwrap().find(&img, 3).is_empty());
    for s in sigs::ALL {
        assert_eq!(Pattern::parse(s.pattern).unwrap().find(&img, 3).len(), 1, "{} after patch", s.name);
    }
    assert_eq!(chara_legal::chara_state(img[jcc]), State::Patched);
}
