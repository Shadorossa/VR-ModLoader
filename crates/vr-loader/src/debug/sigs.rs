//! Signatures of module `debug` (nie.exe PC v7.1.2). Kept here (not in `crate::sigs`) so the module is
//! self-contained; `tests/debug.rs` checks each one is unique at its expected RVA in the dump and that the stolen
//! bytes are relocatable. Generated with `research/scripts/exe_common.py` (`make_sig`); the Lua 5.2 API functions
//! were identified from the alphabetical layout of `lapi.c` exports (docs/game/engine/debugger.md §2).

use crate::sigs::Sig;

/// `int luaL_loadbufferx(lua_State*, const char* buf, size_t sz, const char* name, const char* mode)`: every chunk
/// (script object 0x4D6B20, INCLUDE 0x177C740, base `load` 0x5FCCF0, 0x6051B2). **Hooked** (pre + post): names
/// the chunk, registers `print`/`PRINT`/`WARNING`/`ASSERT` in a fresh VM, logs load errors.
/// Stolen: `sub rsp,48h; mov rax,[rsp+70h]; mov [rsp+30h],rdx` = 14 bytes (the next `lea rdx,[rip+x]` stays).
pub const DBG_LOADBUFFERX: Sig = Sig {
    name: "debug.luaL_loadbufferx",
    pattern: "48 83 EC 48 48 8B 44 24 70 48 89 54 24 30 48 8D 15 ?? ?? ?? ?? 4C 89 44 24 38",
    offset: 0,
    rva: 0x5EA510,
};
pub const DBG_LOADBUFFERX_STEAL: usize = 14;

/// `int lua_pcallk(lua_State*, int nargs, int nresults, int errfunc, int ctx, lua_CFunction k)`: 6 callers, every
/// protected call (chunk runs, the engine's call-a-Lua-function helper 0x4D6BE0, INCLUDE, base `pcall`/`xpcall`).
/// The engine callers pop the error message and continue (errors are silent). **Hooked**: adds a traceback message
/// handler (engine calls: `errfunc == 0 && k == NULL`) and logs every error status.
/// Stolen: `mov [rsp+18h],rsi; push rdi; sub rsp,40h; xor esi,esi; mov [rsp+58h],rbp` = 17 bytes.
pub const DBG_PCALLK: Sig = Sig {
    name: "debug.lua_pcallk",
    pattern: "48 89 74 24 18 57 48 83 EC 40 33 F6 48 89 6C 24 58 41 8B E8 44 8B DA 48 8B F9",
    offset: 0,
    rva: 0x5E7CA0,
};
pub const DBG_PCALLK_STEAL: usize = 17;

/// `int lua_checkstack(lua_State*, int n)` (`LUAI_MAXSTACK` 1000000 = `0xF4240` in its body).
pub const DBG_CHECKSTACK: Sig = Sig {
    name: "debug.lua_checkstack",
    pattern: "48 89 5C 24 08 89 54 24 10 57 48 83 EC 20 4C 8B 49 10 48 8B D9 4C 8B 41 30 48 8B 79 20",
    offset: 0,
    rva: 0x5E7260,
};
/// `void lua_insert(lua_State*, int idx)`: shift-up loop, then top -> idx.
pub const DBG_INSERT: Sig = Sig {
    name: "debug.lua_insert",
    pattern: "48 83 EC 28 4C 8B D9 E8 ?? ?? ?? ?? 49 8B 53 10 4C 8B D0 48 3B D0 76 25",
    offset: 0,
    rva: 0x5E7A30,
};
/// `void lua_remove(lua_State*, int idx)`: shift-down loop, `top--`.
pub const DBG_REMOVE: Sig = Sig {
    name: "debug.lua_remove",
    pattern: "48 83 EC 28 4C 8B D1 E8 ?? ?? ?? ?? 49 8B 4A 10 48 83 C0 10 48 3B C1 73 21",
    offset: 0,
    rva: 0x5E8370,
};
/// `void lua_pushcclosure(lua_State*, lua_CFunction, int n)` (light C function when n == 0: tag 0x16).
pub const DBG_PUSHCCLOSURE: Sig = Sig {
    name: "debug.lua_pushcclosure",
    pattern: "48 89 5C 24 08 48 89 74 24 10 57 48 83 EC 20 49 63 F8 48 8B F2 48 8B D9 45 85 C0 75 23",
    offset: 0,
    rva: 0x5E7DD0,
};
/// `void lua_settop(lua_State*, int idx)`.
pub const DBG_SETTOP: Sig = Sig {
    name: "debug.lua_settop",
    pattern: "85 D2 78 34 48 8B 41 20 48 63 D2 48 FF C2 48 C1 E2 04 48 03 10",
    offset: 0,
    rva: 0x5E8600,
};
/// `int lua_toboolean(lua_State*, int idx)`.
pub const DBG_TOBOOLEAN: Sig = Sig {
    name: "debug.lua_toboolean",
    pattern: "48 83 EC 28 E8 ?? ?? ?? ?? 8B 48 08 85 C9 74 14 83 F9 01 75 05 83 38 00 74 0A",
    offset: 0,
    rva: 0x5E87A0,
};
/// `void lua_setglobal(lua_State*, const char* name)` (registry `LUA_RIDX_GLOBALS` = 2, then settable). Its first 72
/// bytes equal `lua_getglobal` 0x5E7810; the two differ at +0x5C (`lea r9,[r8-20h]` vs `add r8,-10h`).
pub const DBG_SETGLOBAL: Sig = Sig {
    name: "debug.lua_setglobal",
    pattern: "48 89 5C 24 08 48 89 6C 24 10 48 89 74 24 18 57 48 83 EC 20 48 8B 41 18 48 8B FA 48 8B E9 BA 02 00 00 00 48 8B 48 40 E8 ?? ?? ?? ?? 48 8B 5D 10 48 8B D7 48 8B CD 48 8B F0 4C 8D 43 10 4C 89 45 10 E8 ?? ?? ?? ?? 48 89 03 48 8B D6 0F B6 48 08 83 C9 40 89 4B 08 48 8B CD 4C 8B 45 10 4D 8D 48 E0",
    offset: 0,
    rva: 0x5E84A0,
};
/// `void luaL_traceback(lua_State* L, lua_State* L1, const char* msg, int level)` (references
/// `"stack traceback:"` at `.rdata 0x1877388`).
pub const DBG_TRACEBACK: Sig = Sig {
    name: "debug.luaL_traceback",
    pattern: "40 53 56 57 41 55 41 56 41 57 48 81 EC 48 01 00 00 48 8B 05 ?? ?? ?? ?? 48 33 C4 48 89 84 24 20 01 00 00 4C 89 A4 24 38 01 00 00 45 8B F9 4D 8B E0",
    offset: 0,
    rva: 0x5EB150,
};

pub const ALL: &[&Sig] = &[
    &DBG_LOADBUFFERX,
    &DBG_PCALLK,
    &DBG_CHECKSTACK,
    &DBG_INSERT,
    &DBG_REMOVE,
    &DBG_PUSHCCLOSURE,
    &DBG_SETTOP,
    &DBG_TOBOOLEAN,
    &DBG_SETGLOBAL,
    &DBG_TRACEBACK,
];

/// Inline hooks of the module and their stolen byte counts.
pub const INLINE_HOOKS: &[(&Sig, usize)] = &[(&DBG_LOADBUFFERX, DBG_LOADBUFFERX_STEAL), (&DBG_PCALLK, DBG_PCALLK_STEAL)];
