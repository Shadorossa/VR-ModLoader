//! Signatures of module `lua_patch` (nie.exe PC v7.1.2), docs/game/engine/hook-map.md §1 (fs) and §2 (Lua).
//! `tests/lua_patch.rs` checks each one is unique at its expected RVA in the dump and that the stolen bytes are
//! whole, position-independent instructions. The three Lua API entries repeat module `debug`'s patterns (the
//! registry de-duplicates by pattern; only `debug` hooks them, this module **calls** them).

use crate::sigs::Sig;

/// `void* CCriFileOperate::Open(this, const char* path, u32 mode, u8, u32 flags@rsp+0x28)` 0x4E70C0: the single open
/// used by every loader (through the vtable). **Hooked** (pre, pass-through): remembers the last `.lua.bin` path per
/// thread. Stolen: `mov [rsp+18h],rbx; push rbp; push rsi; push rdi; push r12; push r13; push r14` = 14 bytes.
pub const LP_FILE_OPEN: Sig = Sig {
    name: "lua_patch.CCriFileOperate_Open",
    pattern: "48 89 5C 24 18 55 56 57 41 54 41 55 41 56 41 57 48 8D AC 24 60 FC FF FF 48 81 EC A0 04 00 00 48 8B 05 ?? ?? ?? ??",
    offset: 0,
    rva: 0x4E70C0,
};
pub const FILE_OPEN_STEAL: usize = 14;

/// `bool ScriptObject_LoadChunk(holder, resource)` 0x4D6B20: `resource->vt[2]()` (read the file), then
/// `luaL_loadbufferx([holder+0x50], [resource+0x20], resource->vt[5](), NULL, NULL)` and `lua_pcallk(L,0,0,0,0,NULL)`
/// (`0x4D6B6E` / `0x4D6B8F`); on error `lua_settop(L,-2)` + `lua_gc(L, LUA_GCCOLLECT)` and `al = 0`, else `al = 1`.
/// The loader of every script object's **own** chunk (INCLUDE uses `0x177C740`). **Hooked** (post): runs the patch
/// files in `L` when it returns 1. Stolen: `mov [rsp+10h],rsi; push rdi; sub rsp,30h; mov rax,[rdx]; mov rsi,rcx`
/// = 16 bytes.
pub const LP_LOAD_CHUNK: Sig = Sig {
    name: "lua_patch.ScriptObject_LoadChunk",
    pattern: "48 89 74 24 10 57 48 83 EC 30 48 8B 02 48 8B F1 48 8B CA 48 8B FA FF 50 10",
    offset: 0,
    rva: 0x4D6B20,
};
pub const LOAD_CHUNK_STEAL: usize = 16;
/// `holder + 0x50` = the script's `lua_State*` (read by `ScriptObject_LoadChunk` before each Lua call).
pub const HOLDER_L: usize = 0x50;

/// `int luaL_loadbufferx(lua_State*, const char* buf, size_t sz, const char* name, const char* mode)` 0x5EA510
/// (called, never hooked here; module `debug` hooks it).
pub const LP_LOADBUFFERX: Sig = Sig {
    name: "lua_patch.luaL_loadbufferx",
    pattern: "48 83 EC 48 48 8B 44 24 70 48 89 54 24 30 48 8D 15 ?? ?? ?? ?? 4C 89 44 24 38",
    offset: 0,
    rva: 0x5EA510,
};
/// `int lua_pcallk(lua_State*, int nargs, int nresults, int errfunc, int ctx, lua_CFunction k)` 0x5E7CA0 (called).
pub const LP_PCALLK: Sig = Sig {
    name: "lua_patch.lua_pcallk",
    pattern: "48 89 74 24 18 57 48 83 EC 40 33 F6 48 89 6C 24 58 41 8B E8 44 8B DA 48 8B F9",
    offset: 0,
    rva: 0x5E7CA0,
};
/// `void lua_settop(lua_State*, int idx)` 0x5E8600 (pops the error message / restores the stack).
pub const LP_SETTOP: Sig = Sig {
    name: "lua_patch.lua_settop",
    pattern: "85 D2 78 34 48 8B 41 20 48 63 D2 48 FF C2 48 C1 E2 04 48 03 10",
    offset: 0,
    rva: 0x5E8600,
};

pub const ALL: &[&Sig] = &[&LP_FILE_OPEN, &LP_LOAD_CHUNK, &LP_LOADBUFFERX, &LP_PCALLK, &LP_SETTOP];
/// Inline hooks of the module and their stolen byte counts.
pub const HOOKS: &[(&Sig, usize)] = &[(&LP_FILE_OPEN, FILE_OPEN_STEAL), (&LP_LOAD_CHUNK, LOAD_CHUNK_STEAL)];
