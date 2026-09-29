//! Module `chara_legal`: every character counts as legal.
//!
//! `IsCharaIllegal 0xE74450` always returns false. Without it every character whose `chara_base` row fails the
//! engine's legality rule (`0xE2EBF0`: model `c` + 8 digits **and** a `chara_param` row with id
//! `crc32("pc_para_" + model)`) is drawn with the yellow placeholder face (`DrawCharaFaceIcon 0x10513B0` →
//! `0x1052400`) and gets the roster flag `+0x128`. Characters added by mods usually fail it. One byte: the `jz` at
//! `0xE7448B` becomes `jmp` (`74` → `EB`), the same patch Ultimate Victory Road's proxy `D3DCOMPILER_47.dll` applies
//! (its "friend patch"); if that proxy already patched the byte, the module only logs it.
//!
//! Applied in DllMain, before any game thread (and before another patcher's thread) can run.

pub mod sigs;

/// Retail and patched opcode of the `jz` in `IsCharaIllegal`.
pub const JZ: u8 = 0x74;
pub const JMP: u8 = 0xEB;

/// Current state of the patch site.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    Retail,
    Patched,
    Unexpected,
}

pub fn chara_state(jcc: u8) -> State {
    match jcc {
        JZ => State::Retail,
        JMP => State::Patched,
        _ => State::Unexpected,
    }
}

/// Ultimate Victory Road's proxy `D3DCOMPILER_47.dll` (it exports `ApplyFormationPatch`) is loaded in this process.
#[cfg(all(windows, target_arch = "x86_64"))]
pub fn uvr_proxy_loaded() -> bool {
    use windows_sys::Win32::System::LibraryLoader::{GetModuleHandleW, GetProcAddress};
    let name: Vec<u16> = "D3DCOMPILER_47.dll\0".encode_utf16().collect();
    let h = unsafe { GetModuleHandleW(name.as_ptr()) };
    !h.is_null() && unsafe { GetProcAddress(h, b"ApplyFormationPatch\0".as_ptr()) }.is_some()
}

/// Address of the site: the registry signature, else the fixed v7.1.2 RVA if the pattern matches there.
#[cfg(all(windows, target_arch = "x86_64"))]
fn site(text: &crate::game::Text, s: &crate::sigs::Sig) -> Option<usize> {
    if let Some(a) = text.resolve(s) {
        return Some(a);
    }
    let p = crate::scan::Pattern::parse(s.pattern).ok()?;
    let off = (text.base + s.rva as usize).checked_sub(text.text_va)?;
    let hay = text.text.get(off..off + p.len())?;
    if p.find(hay, 1).first() == Some(&0) {
        crate::warn!("chara_legal: {} signature not unique, fixed v7.1.2 RVA 0x{:X} used", s.name, s.rva);
        Some(text.base + s.rva as usize)
    } else {
        None
    }
}

/// DllMain: apply the patch. True when every character is legal afterwards (patched now or already).
#[cfg(all(windows, target_arch = "x86_64"))]
pub fn apply(text: &crate::game::Text) -> bool {
    use crate::game::read;
    use crate::{info, warn};
    if uvr_proxy_loaded() {
        info!("chara_legal: UVR proxy D3DCOMPILER_47.dll is loaded (its patcher finds no retail bytes after this module)");
    }
    match site(text, &sigs::CL_CHARA_LEGAL).map(|a| a + sigs::CHARA_LEGAL_JCC) {
        None => {
            warn!("chara_legal: site not found: added characters show the placeholder face");
            false
        }
        Some(at) => match read::<u8>(at).map(chara_state) {
            Some(State::Retail) => {
                if unsafe { crate::hook::write_code(at, &[JMP]) } {
                    info!("chara_legal: applied at RVA 0x{:X} (jz -> jmp): every character is legal", at - text.base);
                    true
                } else {
                    warn!("chara_legal: write failed");
                    false
                }
            }
            Some(State::Patched) => {
                info!("chara_legal: already applied at RVA 0x{:X} (another patcher): skipped", at - text.base);
                true
            }
            _ => {
                warn!("chara_legal: unexpected byte at RVA 0x{:X}: not patched", at - text.base);
                false
            }
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn states() {
        assert_eq!(chara_state(0x74), State::Retail);
        assert_eq!(chara_state(0xEB), State::Patched);
        assert_eq!(chara_state(0xE9), State::Unexpected);
    }
}
