//! Plugin `clean_hud` (mod `mods\clean_hud\`, `clean_hud.dll`, «Interfaz limpia» / Clean HUD): **no team editor
//! between the VS screen and the walk-in** in offline matches (docs/game/modes/prematch-lineups.md «Revisión 6» / §14).
//! Port of the built-in loader module `prematch` (`crates/vr-loader/src/prematch/mod.rs`) with the same patch, the same
//! decision and the same log lines; the built-in yields when this plugin is loaded, because the mod
//! `provides = ["prematch"]` (docs/app/modloader-plugins.md). The rest of the mod is Lua patches + interface files
//! (research/mods/clean_hud, docs/app/modloader-split.md §6).
//!
//! `game::soccerscene::BattleSettingFlow` (update `0x1436F50`) **state 6** waits for the VS menu to close, sets the
//! camera up and then opens the pre-kick-off team editor (`soccer_formation_menu`) only when the match info flag
//! `mi & 0x2000000` is set (`mi = [[g_gameRoot]+0x6A58]`):
//!
//! ```text
//! 1437594  80 78 16 02            cmp  byte [rax+16h], 2      ; bl = small 5v5 match (for the backdrop call)
//! 1437598  0F 94 C3               sete bl
//! 143759B  F7 00 00 00 00 02      test dword [rax], 2000000h  ; <- site (8 bytes with the je)
//! 14375A1  74 64                  je   1437607                ; bit off: no editor, state = 7 (retail path)
//! 14375A3  B2 05 ...              pre-match BGM, open the editor, dark backdrop 0x1104DB0, state = 7
//! 1437607  C7 46 20 07 00 00 00   mov  dword [rsi+20h], 7
//! ```
//!
//! The site jumps to a stub that runs the retail test and, when the bit is set, asks `decide`: in an offline,
//! non-observer match the stub takes the retail **bit-off** branch (`1437607`), exactly what the matches without the
//! bit do (state 7 finds no menu open and goes on to the walk-in). Kept retail (editor opens): network session active
//! (`0x1239C70`) or observer (`mi+0x54`); anything unreadable counts as "keep retail". Lineup and formation stay as
//! the match was set up (the skipped editor's «Terminar edición» with nothing changed is the identity, docs §14.2).

use serde::{Deserialize, Serialize};

/// Configuration: `mods\clean_hud\config.toml` and `[mods.clean_hud]` of `evt_loader\config.toml` (merged by the
/// ModLoader, the last one wins).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct CleanHudCfg {
    /// No team editor between the VS screen and the walk-in (offline matches). false = retail editor.
    pub skip_prematch_editor: bool,
}

impl Default for CleanHudCfg {
    fn default() -> Self {
        CleanHudCfg { skip_prematch_editor: true }
    }
}

/// Site (RVA `0x143759B`): `test dword [rax], 0x2000000; je +0x64`.
pub const SITE_RVA: u32 = 0x143759B;
pub const SITE_BYTES: [u8; 8] = [0xF7, 0x00, 0x00, 0x00, 0x00, 0x02, 0x74, 0x64];
/// The 7 bytes before the site (`cmp byte [rax+16h],2; sete bl`), checked too.
pub const CONTEXT_RVA: u32 = 0x1437594;
pub const CONTEXT_BYTES: [u8; 7] = [0x80, 0x78, 0x16, 0x02, 0x0F, 0x94, 0xC3];
/// The instruction after the site (editor path) and the `je` target (bit-off path).
pub const EDITOR_PATH_RVA: u32 = 0x14375A3;
pub const NO_EDITOR_PATH_RVA: u32 = 0x1437607;
/// Network-session predicate `0x1239C70`: `mov rdx, [rip+d32]` (the session global) then the checks below.
pub const NET_FN_RVA: u32 = 0x1239C70;
/// Its bytes after the 7-byte `mov rdx, [rip+d32]` (verifies the offsets [`network_active`] reimplements).
pub const NET_FN_TAIL: [u8; 52] = [
    0x8B, 0x82, 0x10, 0x24, 0x00, 0x00, 0x85, 0xC0, 0x74, 0x27, 0xFF, 0xC8, 0x0F, 0xB6, 0x84, 0x10, 0x14, 0x24, 0x00,
    0x00, 0x3C, 0x02, 0x73, 0x19, 0x48, 0x69, 0xC0, 0x08, 0x12, 0x00, 0x00, 0x48, 0x03, 0xC2, 0x74, 0x0D, 0x0F, 0xB6,
    0x80, 0x00, 0x12, 0x00, 0x00, 0xC0, 0xE8, 0x02, 0x24, 0x01, 0xC3, 0x32, 0xC0, 0xC3,
];
/// Match info fields.
pub const MI_FLAG_EDITOR: u32 = 0x0200_0000;
pub const MI_OFF_TYPE: usize = 0x16;
pub const MI_OFF_OBSERVER: usize = 0x54;

/// Offsets of the absolute targets in the stub page.
pub const DECIDE_OFF: usize = 0x80;
pub const EDITOR_ABS_OFF: usize = 0x88;
pub const NO_EDITOR_ABS_OFF: usize = 0x90;

fn rel32(from_end: usize, to: usize) -> [u8; 4] {
    ((to as i64 - from_end as i64) as i32).to_le_bytes()
}

/// Stub at `page` (`rax` = match info, as at the site; `rsp` 16-aligned there):
///
/// ```text
///   test dword [rax], 2000000h ; je no_editor            ; retail bit test
///   mov rcx, rax ; sub rsp, 20h ; call [rip+decide] ; add rsp, 20h
///   test al, al ; jne no_editor                          ; decide(mi) = 1: skip the editor
///   jmp [rip+editor]                                     ; retail editor path
/// no_editor:
///   jmp [rip+no_editor]
/// ```
///
/// Only `rax`, `rcx`, `rdx`, `r8`–`r11` and the flags change (volatile; none is live on either target: both set
/// what they read). The callee keeps `rbx` (`bl`), `rsi` and `xmm6`/`xmm7` (non-volatile).
pub fn stub_code(page: usize) -> Vec<u8> {
    let mut c: Vec<u8> = SITE_BYTES[..6].to_vec();
    let je_at = c.len();
    c.extend_from_slice(&[0x74, 0x00]);
    c.extend_from_slice(&[0x48, 0x89, 0xC1]); // mov rcx, rax
    c.extend_from_slice(&[0x48, 0x83, 0xEC, 0x20]); // sub rsp, 0x20
    let at = page + c.len();
    c.extend_from_slice(&[0xFF, 0x15]); // call [rip+d32]
    c.extend_from_slice(&rel32(at + 6, page + DECIDE_OFF));
    c.extend_from_slice(&[0x48, 0x83, 0xC4, 0x20]); // add rsp, 0x20
    c.extend_from_slice(&[0x84, 0xC0]); // test al, al
    let jne_at = c.len();
    c.extend_from_slice(&[0x75, 0x00]);
    let at = page + c.len();
    c.extend_from_slice(&[0xFF, 0x25]); // jmp [rip+editor]
    c.extend_from_slice(&rel32(at + 6, page + EDITOR_ABS_OFF));
    let no_editor = c.len();
    c[je_at + 1] = (no_editor - (je_at + 2)) as u8;
    c[jne_at + 1] = (no_editor - (jne_at + 2)) as u8;
    let at = page + c.len();
    c.extend_from_slice(&[0xFF, 0x25]); // jmp [rip+no_editor]
    c.extend_from_slice(&rel32(at + 6, page + NO_EDITOR_ABS_OFF));
    c
}

/// The 8 bytes written over the site: `jmp stub` + 3 NOPs; None when the stub is out of rel32 reach.
pub fn site_patch(site: usize, stub: usize) -> Option<[u8; 8]> {
    let d = stub as i64 - (site as i64 + 5);
    let r = i32::try_from(d).ok()?.to_le_bytes();
    let mut b = [0x90u8; 8];
    b[0] = 0xE9;
    b[1..5].copy_from_slice(&r);
    Some(b)
}

/// `0x1239C70` reimplemented: `g` = the session object, `rd8` / `rd32` guarded reads. None = unreadable.
pub fn network_active(g: usize, rd8: impl Fn(usize) -> Option<u8>, rd32: impl Fn(usize) -> Option<u32>) -> Option<bool> {
    let n = rd32(g + 0x2410)?;
    if n == 0 {
        return Some(false);
    }
    let kind = rd8(g + 0x2414 + (n as usize - 1))?;
    if kind >= 2 {
        return Some(false);
    }
    let entry = g + kind as usize * 0x1208;
    Some(rd8(entry + 0x1200)? & 4 != 0)
}

/// Skip only a readable, offline, non-observer match (anything unknown keeps the retail editor).
pub fn should_skip(observer: Option<u8>, network: Option<bool>) -> bool {
    observer == Some(0) && network == Some(false)
}

/// Lowest start address to try for a stub page below the image, so that every byte of an image of `size_of_image`
/// bytes at `base` stays within rel32 reach of it (same margin as the built-in `office::alloc_near`).
pub fn near_floor(base: usize, size_of_image: usize) -> usize {
    (base + size_of_image).saturating_sub(0x7000_0000)
}

#[cfg(all(windows, target_arch = "x86_64"))]
mod rt;

#[cfg(test)]
mod tests {
    use super::*;

    fn rd32(b: &[u8], o: usize) -> i64 {
        i32::from_le_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]]) as i64
    }

    #[test]
    fn config_default_and_parse() {
        assert!(CleanHudCfg::default().skip_prematch_editor);
        let c: CleanHudCfg = toml::from_str("skip_prematch_editor = false").unwrap();
        assert!(!c.skip_prematch_editor);
        let c: CleanHudCfg = toml::from_str("").unwrap();
        assert!(c.skip_prematch_editor);
    }

    #[test]
    fn site_patch_jumps_to_stub() {
        let p = site_patch(0x1_4143_759B, 0x1_4100_0000).unwrap();
        assert_eq!(p[0], 0xE9);
        assert_eq!(0x1_4143_759Bi64 + 5 + rd32(&p, 1), 0x1_4100_0000);
        assert_eq!(&p[5..], &[0x90, 0x90, 0x90]);
        assert!(site_patch(0x1_4143_759B, 0x3_0000_0000).is_none());
        // the je rel8 of the site lands on the bit-off path
        assert_eq!(SITE_RVA as usize + 8 + SITE_BYTES[7] as usize, NO_EDITOR_PATH_RVA as usize);
        assert_eq!(SITE_RVA as usize + 8, EDITOR_PATH_RVA as usize);
        // context + site are the 15 contiguous bytes 1437594..14375A3
        assert_eq!(CONTEXT_RVA as usize + CONTEXT_BYTES.len(), SITE_RVA as usize);
    }

    #[test]
    fn stub_fits_before_data() {
        let c = stub_code(0x1_4000_0000);
        assert!(c.len() <= DECIDE_OFF, "{}", c.len());
    }

    #[test]
    fn near_floor_keeps_reach() {
        let base = 0x1_4000_0000usize;
        let size = 0x2400_0000usize;
        let f = near_floor(base, size);
        assert!(site_patch(base + size - 8, f).is_some());
        assert_eq!(near_floor(0x1000, 0x1000), 0);
    }

    #[test]
    fn network_predicate() {
        let g = 0x1000usize;
        let mk = |n: u32, kind: u8, flags: u8| {
            move |a: usize| -> Option<u32> {
                if a == g + 0x2410 {
                    Some(n)
                } else if a == g + 0x2414 + (n.max(1) as usize - 1) {
                    Some(kind as u32)
                } else if a == g + kind as usize * 0x1208 + 0x1200 {
                    Some(flags as u32)
                } else {
                    None
                }
            }
        };
        let run = |n, kind, flags| {
            let r = mk(n, kind, flags);
            network_active(g, |a| r(a).map(|v| v as u8), &r)
        };
        assert_eq!(run(0, 0, 4), Some(false)); // no session
        assert_eq!(run(1, 0, 4), Some(true));
        assert_eq!(run(2, 1, 4), Some(true));
        assert_eq!(run(1, 0, 0), Some(false));
        assert_eq!(run(1, 2, 4), Some(false)); // kind >= 2
        assert_eq!(network_active(g, |_| None, |_| None), None);
        assert!(should_skip(Some(0), Some(false)));
        assert!(!should_skip(Some(1), Some(false))); // observer
        assert!(!should_skip(Some(0), Some(true))); // network match
        assert!(!should_skip(None, Some(false)));
        assert!(!should_skip(Some(0), None));
    }

    /// Runs the real stub bytes: harness `mov rax, rcx; sub rsp, 8; jmp stub` (rsp 16-aligned as at the site), the
    /// two targets return 1 (editor) / 2 (no editor) after `add rsp, 8`.
    #[cfg(all(windows, target_arch = "x86_64"))]
    #[test]
    fn stub_runs() {
        use std::sync::atomic::{AtomicU8, AtomicUsize, Ordering};
        use windows_sys::Win32::System::Memory::{VirtualAlloc, MEM_COMMIT, MEM_RESERVE, PAGE_EXECUTE_READWRITE};
        static ANSWER: AtomicU8 = AtomicU8::new(0);
        static SEEN: AtomicUsize = AtomicUsize::new(0);
        extern "C" fn fake_decide(mi: usize) -> u8 {
            SEEN.store(mi, Ordering::SeqCst);
            ANSWER.load(Ordering::SeqCst)
        }
        unsafe {
            let mem = VirtualAlloc(std::ptr::null(), 0x1000, MEM_COMMIT | MEM_RESERVE, PAGE_EXECUTE_READWRITE) as usize;
            assert_ne!(mem, 0);
            let stub = mem;
            let code = stub_code(stub);
            std::ptr::copy_nonoverlapping(code.as_ptr(), stub as *mut u8, code.len());
            let editor = mem + 0x100;
            let no_editor = mem + 0x110;
            let target = |ret: u8| vec![0x48, 0x83, 0xC4, 0x08, 0xB8, ret, 0, 0, 0, 0xC3];
            std::ptr::copy_nonoverlapping(target(1).as_ptr(), editor as *mut u8, 10);
            std::ptr::copy_nonoverlapping(target(2).as_ptr(), no_editor as *mut u8, 10);
            let decide_fn: extern "C" fn(usize) -> u8 = fake_decide;
            std::ptr::write((stub + DECIDE_OFF) as *mut u64, decide_fn as usize as u64);
            std::ptr::write((stub + EDITOR_ABS_OFF) as *mut u64, editor as u64);
            std::ptr::write((stub + NO_EDITOR_ABS_OFF) as *mut u64, no_editor as u64);
            let harness = mem + 0x200;
            let mut h = vec![0x48, 0x89, 0xC8, 0x48, 0x83, 0xEC, 0x08, 0xE9];
            h.extend_from_slice(&rel32(harness + h.len() + 4, stub));
            std::ptr::copy_nonoverlapping(h.as_ptr(), harness as *mut u8, h.len());
            let f: extern "C" fn(*const u32) -> u32 = std::mem::transmute(harness);

            let off = [0u32; 4];
            let on = [MI_FLAG_EDITOR | 0x100, 0, 0, 0];
            ANSWER.store(1, Ordering::SeqCst);
            SEEN.store(0, Ordering::SeqCst);
            assert_eq!(f(off.as_ptr()), 2, "bit off: retail no-editor path, decide not called");
            assert_eq!(SEEN.load(Ordering::SeqCst), 0);
            assert_eq!(f(on.as_ptr()), 2, "bit on + decide 1: skipped");
            assert_eq!(SEEN.load(Ordering::SeqCst), on.as_ptr() as usize, "decide gets the match info");
            ANSWER.store(0, Ordering::SeqCst);
            assert_eq!(f(on.as_ptr()), 1, "bit on + decide 0: retail editor");
        }
    }
}
