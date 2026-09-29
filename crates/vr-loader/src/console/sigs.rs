//! Signatures of module `console` (nie.exe PC v7.1.2). `tests/console.rs` checks them on the dump. Every tap is a
//! chain link (`crate::hook::chain_hook`), so plugins can hook the same functions; `lua_patch.CCriFileOperate_Open`
//! (category `files`) is `lua_patch`'s signature.

use crate::sigs::Sig;

/// `bool CMenuController::OpenMenu(this, const u32* nameHash, const OpenMenuParam* p)` 0x10DEC90 (vtable
/// `game::CMenuController` 0x1A03068 slot 7; singleton `[0x21F6EC0]`; docs/game/engine/hook-map.md, menu). Called by
/// `CMND_OPEN_MENU_OBJECT` / `CMND_BUILD_MENU_OBJECT` and by the engine (e.g. the skill banner `common_skill_telop`).
/// `p = {u64 0; u32 param; u32 0; u16 buildOnly}`.
///
/// ```text
/// 10DEC90  48 89 5C 24 10          mov  [rsp+10h], rbx
/// 10DEC95  55 56 57                push rbp; push rsi; push rdi
/// 10DEC98  41 54 41 55 41 56 41 57 push r12; push r13; push r14; push r15   <- 16 stolen bytes end here
/// 10DECA0  48 8D AC 24 B0 FE FF FF lea  rbp, [rsp-150h]
/// ```
pub const CO_OPEN_MENU: Sig = Sig {
    name: "console.OpenMenu",
    pattern: "48 89 5C 24 10 55 56 57 41 54 41 55 41 56 41 57 48 8D AC 24 B0 FE FF FF 48 81 EC 50 02 00 00 48 8B 05 ?? ?? ?? ?? 48 33 C4 48 89 85 40 01 00 00",
    offset: 0,
    rva: 0x10DEC90,
};
/// `mov [rsp+10h],rbx` + 7 pushes (no relative operand).
pub const OPEN_MENU_STEAL: usize = 16;
/// `OpenMenuParam.param` (u32).
pub const OMP_PARAM: usize = 8;

/// `PlayCharaVoice(mgr, u32* handle, bank, suffix, ...)` 0x16FC7E0: plays the character voice cue `<bank>_<suffix>`
/// (`snprintf "%s_%s"` + crc32 + `PlayCue 0x16F8D00`; `*handle = 0` when no loaded sheet has the cue). Category
/// `sound`; plugins (audio_engine) chain on it too.
pub const CO_PLAY_CHARA_VOICE: Sig = Sig {
    name: "console.PlayCharaVoice",
    pattern: "40 55 53 56 57 48 8D AC 24 98 FE FF FF 48 81 EC 68 02 00 00 48 8B 05 ?? ?? ?? ?? 48 33 C4               48 89 85 50 01 00 00 48 8B DA 48 8B F1 48 8B BD B0 01 00 00 48 8B 05 ?? ?? ?? ?? 48 8B 88 50 6A 00 00",
    offset: 0,
    rva: 0x16FC7E0,
};
/// Stolen bytes: `push rbp; push rbx; push rsi; push rdi; lea rbp,[rsp-168h]; sub rsp,268h` (no relative operand).
pub const PLAY_CHARA_VOICE_STEAL: usize = 20;

pub const ALL: &[&Sig] = &[&CO_OPEN_MENU, &CO_PLAY_CHARA_VOICE];
/// Inline hooks of the module (the `files` tap is a chain link on `lua_patch`'s hook, listed there).
pub const HOOKS: &[(&Sig, usize)] = &[(&CO_OPEN_MENU, OPEN_MENU_STEAL), (&CO_PLAY_CHARA_VOICE, PLAY_CHARA_VOICE_STEAL)];

/// Retail Lua commands the console observes (filters that never answer; docs/game/engine/lua-commands.md).
pub const CMD_CLOSE_MENU_OBJECT: u32 = 0x1319_3DE3;
pub const CMD_DELETE_MENU_OBJECT: u32 = 0x9231_3999;
/// `CMND_RESERVE_MENU(crc32(menu), crc32(menu), 0, 1, param, …)`.
pub const CMD_RESERVE_MENU: u32 = 0xD81D_6B3D;
/// `CMND_RESERVE_SOCCER(game, stadium, difficulty, …)`.
pub const CMD_RESERVE_SOCCER: u32 = 0x6097_CD41;
/// `crc32("common_skill_telop")`: the skill-name banner opened for every hissatsu.
pub const MENU_SKILL_TELOP: u32 = 0x4A33_CE3A;

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn command_hashes() {
        for (n, h) in [
            ("CMND_CLOSE_MENU_OBJECT", CMD_CLOSE_MENU_OBJECT),
            ("CMND_DELETE_MENU_OBJECT", CMD_DELETE_MENU_OBJECT),
            ("CMND_RESERVE_MENU", CMD_RESERVE_MENU),
            ("CMND_RESERVE_SOCCER", CMD_RESERVE_SOCCER),
            ("common_skill_telop", MENU_SKILL_TELOP),
        ] {
            assert_eq!(crc32fast::hash(n.as_bytes()), h, "{n}");
        }
        let fixed = crate::scan::Pattern::parse(CO_OPEN_MENU.pattern).unwrap().fixed_prefix(OPEN_MENU_STEAL).unwrap();
        assert_eq!(fixed.len(), OPEN_MENU_STEAL);
    }
}
