//! Module `mods`: static checks against the v7.1.2 dump `nie.exe` (read-only; skipped when absent).
//! Path: env `EVT_NIE_EXE`, else the default dump location.

use vr_loader::debug::x64len;
use vr_loader::mods::sigs;
use vr_loader::pe::parse_headers;
use vr_loader::scan::Pattern;

const DEFAULT_DUMP: &str =
    concat!(env!("CARGO_MANIFEST_DIR"), "/../../assets/v7.1.2/nie.exe");

fn load() -> Option<Vec<u8>> {
    let p = std::env::var("EVT_NIE_EXE").unwrap_or_else(|_| DEFAULT_DUMP.to_string());
    std::fs::read(&p).ok()
}

#[test]
fn overlay_hook_is_unique_and_open_reads_out_loose() {
    let Some(file) = load() else {
        eprintln!("nie.exe dump not found: skipped");
        return;
    };
    let h = parse_headers(&file).expect("PE32+");
    assert!(vr_loader::gate::header_matches(h.time_date_stamp, h.size_of_image), "dump is not v7.1.2");
    let t = h.section(".text").unwrap();
    let text = &file[t.raw_off as usize..(t.raw_off + t.raw_size.min(t.vsize)) as usize];
    for s in sigs::ALL {
        let hits = Pattern::parse(s.pattern).unwrap().find(text, 3);
        assert_eq!(hits.len(), 1, "{}: {} hits", s.name, hits.len());
        assert_eq!(t.rva + hits[0] as u32 - s.offset, s.rva, "{}", s.name);
    }
    for (s, steal) in sigs::HOOKS {
        let p = Pattern::parse(s.pattern).unwrap();
        assert!(*steal >= 14 && p.fixed_prefix(*steal).is_some(), "{}: steal", s.name);
        let off = (s.rva - t.rva) as usize;
        assert_eq!(x64len::relocatable_prefix(&text[off..off + 64], 14), Ok(*steal), "{}", s.name);
    }
    let at = |rva: u32| (rva - t.rva) as usize;
    // CCriFileOperate::Open 0x4E70C0: `lea r8,[rbp+290h]` (out), call ResolveOverlayPath at 0x4E714A, then
    // `lea rdi,[rbp+290h]; test rax,rax; cmove rdi,r15` = only a NULL test, `out` becomes the path to open
    let call = at(0x4E714A);
    assert_eq!(text[call], 0xE8);
    let d = i32::from_le_bytes(text[call + 1..call + 5].try_into().unwrap());
    assert_eq!((0x4E714Fi64 + d as i64) as u32, sigs::MODS_RESOLVE_OVERLAY.rva);
    // lea r8,[rbp+290h]; mov rdx,r15; mov rcx,rsi
    assert_eq!(&text[call - 13..call], &[0x4C, 0x8D, 0x85, 0x90, 0x02, 0x00, 0x00, 0x49, 0x8B, 0xD7, 0x48, 0x8B, 0xCE]);
    assert_eq!(&text[call + 5..call + 18], &[0x48, 0x8D, 0xBD, 0x90, 0x02, 0x00, 0x00, 0x48, 0x85, 0xC0, 0x49, 0x0F, 0x44]);
    // the buffer ends at the stack cookie [rbp+390h]: 0x100 bytes, as sigs::OUT_BUF says
    let open = at(0x4E70C0);
    let cookie = [0x48, 0x89, 0x85, 0x90, 0x03, 0x00, 0x00]; // mov [rbp+390h],rax
    assert!(text[open..open + 0x40].windows(7).any(|w| w == cookie));
    assert_eq!(0x390 - 0x290, sigs::OUT_BUF);
    // every caller of ResolveOverlayPath tests its result against NULL right after the call (never dereferences it)
    let target = sigs::MODS_RESOLVE_OVERLAY.rva;
    let mut callers = 0;
    for i in 0..text.len() - 5 {
        if text[i] == 0xE8 {
            let d = i32::from_le_bytes(text[i + 1..i + 5].try_into().unwrap());
            if (t.rva as i64 + i as i64 + 5 + d as i64) as u32 == target {
                callers += 1;
                let after = &text[i + 5..i + 40];
                assert!(after.windows(3).any(|w| w == [0x48, 0x85, 0xC0]), "caller at 0x{:X}", t.rva as usize + i);
            }
        }
    }
    assert_eq!(callers, 12);
}

/// The engine facts the in-memory cpk_list registration (`mods::cpklist`, `mods::hooks::publish`) rests on.
#[test]
fn cpk_list_fields_and_loose_record_shape() {
    let Some(file) = load() else {
        eprintln!("nie.exe dump not found: skipped");
        return;
    };
    let h = parse_headers(&file).expect("PE32+");
    let t = h.section(".text").unwrap();
    let text = &file[t.raw_off as usize..(t.raw_off + t.raw_size.min(t.vsize)) as usize];
    let at = |rva: u32, n: usize| &text[(rva - t.rva) as usize..(rva - t.rva) as usize + n];
    let le = |v: usize| (v as u32).to_le_bytes();
    // Open 0x4E70C0: `lea rbx,[rcx+178h]; ...; lea r13,[rbx+8]; call [EnterCriticalSection]` = the lock at +0x180
    assert_eq!(at(0x4E7108, 3), [0x48, 0x8D, 0x99]);
    assert_eq!(at(0x4E710B, 4), le(sigs::FO_LOCK - 8));
    assert_eq!(at(0x4E7113, 4), [0x4C, 0x8D, 0x6B, 0x08]);
    // overlay hit: `xor r15d,r15d; test rax,rax; je lookup; cmp [rsi+170h],r14b; je skip_lookup`: with the byte set the
    // rewritten path is looked up in cpk_list
    assert_eq!(at(0x4E7178, 11), [0x45, 0x33, 0xFF, 0x48, 0x85, 0xC0, 0x74, 0x09, 0x44, 0x38, 0xB6]);
    assert_eq!(at(0x4E7183, 4), le(sigs::FO_OVERLAY_USES_LIST));
    // same byte in the `exists` slot (0x4E68B0)
    assert_eq!(at(0x4E6900, 2), [0x80, 0xBE]);
    assert_eq!(at(0x4E6902, 4), le(sigs::FO_OVERLAY_USES_LIST));
    // record found: its size (+0x18) goes to the handle (+0x134); GetFileSize 0x4E8020 answers from there at once
    assert_eq!(at(0x4E71E3, 4), [0x41, 0x8B, 0x47, 0x18]);
    assert_eq!(at(0x4E79A9, 7), [0x44, 0x89, 0xA7, 0x34, 0x01, 0x00, 0x00]);
    assert_eq!(at(0x4E806A, 10), [0x8B, 0x87, 0x34, 0x01, 0x00, 0x00, 0x85, 0xC0, 0x79, 0x1D]);
    // FindCpkListEntry 0x4E83D0: count +0x130 (qword), root +0x18, records +0x128 (0x1C each, crc at +0x10), pool +0x120
    assert_eq!(at(0x4E83EC, 3), [0x48, 0x83, 0xB9]);
    assert_eq!(at(0x4E83EF, 4), le(sigs::FO_COUNT));
    assert_eq!(at(0x4E8400, 4), [0x48, 0x8D, 0x51, sigs::FO_ROOT as u8]);
    assert_eq!(at(0x4E8481, 3), [0x4D, 0x8B, 0x95]);
    assert_eq!(at(0x4E8484, 4), le(sigs::FO_RECS));
    assert_eq!(at(0x4E84A4, 9), [0x4C, 0x6B, 0xC9, 0x1C, 0x43, 0x3B, 0x5C, 0x11, 0x10]);
    assert_eq!(at(0x4E8514, 3), [0x49, 0x8B, 0x85]);
    assert_eq!(at(0x4E8517, 4), le(sigs::FO_POOL));
    // list loader's record builder 0x1765B08: all four offsets -1, crc/cpk crc/size 0, then the cpk offsets stay -1
    // and the cpk crc 0 when the cpk strings are null or "" (= a loose record, `cpklist::Rec::loose`)
    assert_eq!(
        at(0x1765C20, 24),
        [
            0x33, 0xC0, 0x49, 0xC7, 0x02, 0xFF, 0xFF, 0xFF, 0xFF, 0x49, 0xC7, 0x42, 0x08, 0xFF, 0xFF, 0xFF, 0xFF, 0x49,
            0x89, 0x42, 0x10, 0x41, 0x89, 0x42
        ]
    );
    assert_eq!(at(0x1765CCF, 4), [0x41, 0x89, 0x42, 0x14]);
    assert_eq!(at(0x1765CE4, 9), [0xBE, 0xFF, 0xFF, 0xFF, 0xFF, 0x41, 0x89, 0x72, 0x08]);
    assert_eq!(at(0x1765D06, 9), [0xBF, 0xFF, 0xFF, 0xFF, 0xFF, 0x41, 0x89, 0x7A, 0x0C]);
}
