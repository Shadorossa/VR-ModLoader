//! Signature of module `chara_legal` (nie.exe PC v7.1.2). `tests/chara_legal.rs` checks it is unique at its expected
//! RVA in the dump. The byte the module rewrites is wildcarded, so the signature also matches an image that another
//! patcher (Ultimate Victory Road's proxy `D3DCOMPILER_47.dll`) already patched.

use crate::sigs::Sig;

/// `IsCharaIllegal(const u32* charaBaseId)` `0xE74450`: `rec = GDSCharaBase.Find(id); return rec && rec[+0x6B] == 0`
/// (`+0x6B` = "legal" flag built by `0xE2EBF0`). The site is the tail after the lookup call:
///
/// ```text
/// 0xE74488  48 85 C0        test rax, rax
/// 0xE7448B  74 12           jz   ret_false        <- patched to EB 12 (jmp): the function always returns false
/// 0xE7448D  80 78 6B 00     cmp  byte [rax+6Bh], 0
///           0F 94 C0        sete al
/// ```
///
/// The `jz` opcode is wildcarded; the following `mov rbx, [rsp+38h]` is part of the pattern.
pub const CL_CHARA_LEGAL: Sig = Sig {
    name: "chara_legal.CharaIllegalTest",
    pattern: "48 85 C0 ?? 12 80 78 6B 00 0F 94 C0 48 8B 5C 24 38",
    offset: 0,
    rva: 0xE74488,
};
/// Offset of the `jz` opcode inside [`CL_CHARA_LEGAL`].
pub const CHARA_LEGAL_JCC: usize = 3;

pub const ALL: &[&Sig] = &[&CL_CHARA_LEGAL];
