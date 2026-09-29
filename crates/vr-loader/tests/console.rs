//! Module `console`: static checks against the v7.1.2 dump `nie.exe` (read-only; skipped when absent).
//! Path: env `EVT_NIE_EXE`, else the default dump location. docs/app/modloader-consola.md.

use vr_loader::console::sigs;
use vr_loader::pe::parse_headers;
use vr_loader::scan::Pattern;

const DEFAULT_DUMP: &str =
    concat!(env!("CARGO_MANIFEST_DIR"), "/../../assets/v7.1.2/nie.exe");

fn load() -> Option<Vec<u8>> {
    let p = std::env::var("EVT_NIE_EXE").unwrap_or_else(|_| DEFAULT_DUMP.to_string());
    std::fs::read(&p).ok()
}

#[test]
fn open_menu_unique_and_steal_bytes() {
    let Some(file) = load() else {
        eprintln!("nie.exe dump not found: skipped");
        return;
    };
    let h = parse_headers(&file).expect("PE32+");
    let t = h.section(".text").unwrap();
    let text = &file[t.raw_off as usize..(t.raw_off + t.raw_size.min(t.vsize)) as usize];
    let pat = Pattern::parse(sigs::CO_OPEN_MENU.pattern).unwrap();
    let hits = pat.find(text, 3);
    assert_eq!(hits.len(), 1);
    assert_eq!(t.rva + hits[0] as u32, sigs::CO_OPEN_MENU.rva);
    // 16 stolen bytes = mov [rsp+10h],rbx + push rbp/rsi/rdi/r12/r13/r14/r15 (whole, position-independent)
    let steal = pat.fixed_prefix(sigs::OPEN_MENU_STEAL).expect("fixed prefix");
    assert_eq!(
        steal,
        vec![0x48, 0x89, 0x5C, 0x24, 0x10, 0x55, 0x56, 0x57, 0x41, 0x54, 0x41, 0x55, 0x41, 0x56, 0x41, 0x57]
    );
    // the next instruction starts right after them: lea rbp, [rsp-150h]
    let o = hits[0] + sigs::OPEN_MENU_STEAL;
    assert_eq!(&text[o..o + 4], &[0x48, 0x8D, 0xAC, 0x24]);
}
