//! Signature of module `mods` (nie.exe PC v7.1.2), docs/game/engine/hook-map.md §1. `tests/mods.rs` checks it is
//! unique at its RVA, that the stolen bytes are whole position-independent instructions, and that
//! `CCriFileOperate::Open` calls it and only null-tests its result before reading `out` loose.

use crate::sigs::Sig;

/// `OverrayPath* ResolveOverlayPath(CCriFileOperate* this, const char* path, char out[256])` 0x4E8730: the engine's
/// (empty in retail) path-overlay table. A non-NULL result makes `Open` skip the cpk_list lookup and read `out` as a
/// **loose** file; its 12 callers (Open, exists, size, delete, rename, ...) only test the result against NULL.
/// **Hooked** (replace-or-forward): a path overridden by an enabled mod gets the mod file's absolute path.
/// Stolen: `mov [rsp+18h],r8; mov [rsp+8],rcx; push rbx; push rbp; push rsi; push rdi` = 14 bytes.
pub const MODS_RESOLVE_OVERLAY: Sig = Sig {
    name: "mods.ResolveOverlayPath",
    pattern: "4C 89 44 24 18 48 89 4C 24 08 53 55 56 57 41 54 41 55 41 56 41 57 48 83 EC 28 48 8B FA",
    offset: 0,
    rva: 0x4E8730,
};
pub const RESOLVE_OVERLAY_STEAL: usize = 14;
/// Size of every caller's `out` buffer (`Open`: `rbp+0x290 .. rbp+0x390`, the others 0x100 stack bytes).
pub const OUT_BUF: usize = 0x100;

/// `lives::CCriFileOperate` fields the in-memory cpk_list registration uses (hook-map.md §1; `tests/mods.rs` checks
/// each against the code that reads it):
/// data root C string (`<root>/<path>` = loose file), read by `Open` / `FindCpkListEntry`.
pub const FO_ROOT: usize = 0x18;
/// cpk_list string pool / records (0x1C each, sorted by crc) / record count (u64), published by `fs.LoadCpkList`.
pub const FO_POOL: usize = 0x120;
pub const FO_RECS: usize = 0x128;
pub const FO_COUNT: usize = 0x130;
/// Byte: non-zero = an overlay hit is still looked up in cpk_list (with the rewritten path). 0 in retail.
pub const FO_OVERLAY_USES_LIST: usize = 0x170;
/// `CRITICAL_SECTION` held by `Open`, `GetFileSize` and `Read` around every cpk_list / pool access.
pub const FO_LOCK: usize = 0x180;
/// Room for the root string (`+0x18` .. the pool pointer at `+0x120`).
pub const FO_ROOT_MAX: usize = FO_POOL - FO_ROOT;

pub const ALL: &[&Sig] = &[&MODS_RESOLVE_OVERLAY];
pub const HOOKS: &[(&Sig, usize)] = &[(&MODS_RESOLVE_OVERLAY, RESOLVE_OVERLAY_STEAL)];
