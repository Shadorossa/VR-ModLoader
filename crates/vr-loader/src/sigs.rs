//! Byte signatures used by the loader (nie.exe PC v7.1.2).
//!
//! Every pattern must match **exactly once** in `.text`; the loader refuses to use a signature that matches 0 or
//! ≥ 2 times (fail safe). `rva` is the expected hit in v7.1.2 and is checked statically by
//! `tests/signatures.rs` against the dump `nie.exe`; at run time only the pattern is used (ASLR-safe).
//! Sources: the project's hook map (docs/hook-map.md).

/// A code signature.
#[derive(Debug, Clone, Copy)]
pub struct Sig {
    pub name: &'static str,
    pub pattern: &'static str,
    /// Hook address = match - offset (always 0 here).
    pub offset: u32,
    /// Expected RVA in v7.1.2 (static check only).
    pub rva: u32,
}

/// A RIP-relative operand inside a signature: `target = match + next_ip_off + disp32@disp_off`.
#[derive(Debug, Clone, Copy)]
pub struct RipRef {
    pub name: &'static str,
    pub sig: &'static Sig,
    pub disp_off: u32,
    pub next_ip_off: u32,
    /// Expected target RVA in v7.1.2 (static check only).
    pub rva: u32,
}

// ---------------------------------------------------------------- Lua (hook-map §2)
pub const LUA_COMMAND_DISPATCH: Sig = Sig {
    name: "lua.CommandDispatch",
    pattern: "48 89 5C 24 10 48 89 74 24 18 48 89 7C 24 20 41 56 48 83 EC 20 41 8B F1",
    offset: 0,
    rva: 0xCA7550,
};
/// Bytes stolen by the inline hook on `lua.CommandDispatch`: three 5-byte `mov [rsp+x], reg` (no relative operands).
pub const LUA_COMMAND_DISPATCH_STEAL: usize = 15;

pub const LUA_GETTOP: Sig = Sig {
    name: "lua.lua_gettop",
    pattern: "48 8B 41 20 48 8B 51 10 48 2B 10 48 8D 42 F0 48 C1 F8 04 C3 CC CC CC CC",
    offset: 0,
    rva: 0x5E7920,
};
pub const LUA_TONUMBERX: Sig = Sig {
    name: "lua.lua_tonumberx",
    pattern: "40 53 48 83 EC 30 49 8B D8 E8 ?? ?? ?? ?? 83 78 08 03 74 22 48 8D 54 24 20",
    offset: 0,
    rva: 0x5E88C0,
};
pub const LUA_TOLSTRING: Sig = Sig {
    name: "lua.lua_tolstring",
    pattern: "48 89 5C 24 08 48 89 74 24 10 57 48 83 EC 20 49 8B D8 8B F2 48 8B F9 E8 ?? ?? ?? ??",
    offset: 0,
    rva: 0x5E8820,
};
pub const LUA_TYPE: Sig = Sig {
    name: "lua.lua_type",
    pattern: "48 83 EC 28 E8 ?? ?? ?? ?? 48 8D 0D ?? ?? ?? ?? 48 3B C1 74 0B 8B 40 08 83 E0 0F 48 83 C4 28 C3",
    offset: 0,
    rva: 0x5E8A80,
};
pub const LUA_PUSHNUMBER: Sig = Sig {
    name: "lua.lua_pushnumber",
    pattern: "48 8B 41 10 F2 0F 11 08 C7 40 08 03 00 00 00 48 83 41 10 10 C3 CC CC CC",
    offset: 0,
    rva: 0x5E7FA0,
};
pub const LUA_PUSHBOOLEAN: Sig = Sig {
    name: "lua.lua_pushboolean",
    pattern: "4C 8B 41 10 33 C0 85 D2 0F 95 C0 41 89 00 41 C7 40 08 01 00 00 00 48 83 41 10 10",
    offset: 0,
    rva: 0x5E7DB0,
};
pub const LUA_PUSHSTRING: Sig = Sig {
    name: "lua.lua_pushstring",
    pattern: "48 89 5C 24 08 57 48 83 EC 20 48 8B FA 48 8B D9 48 85 D2 75 19 48 8B 41 10",
    offset: 0,
    rva: 0x5E7FC0,
};


// ---------------------------------------------------------------- soccer scene (core: match_state, console)
/// Site in the scene driver `0x14237D0`: `mov rcx,[rip+g_sceneSoccer]; test rcx,rcx; je; movaps xmm1,xmm8;
/// call CSceneSoccer::Update 0x16830E0`.
pub const SCENE_UPDATE_CALL: Sig = Sig {
    name: "scene.SceneUpdateCall",
    pattern: "48 8B 0D ?? ?? ?? ?? 48 85 C9 74 09 41 0F 28 C8 E8 ?? ?? ?? ?? 4C 8D 9C 24 A0 00 00 00",
    offset: 0,
    rva: 0x1423EA5,
};
/// `CSceneSoccer*` global (clock f32 `+0x2208` = game seconds of the current half; `[+0]` = soccer actor array;
/// byte `+0x222D` = soccer mode).
pub const G_SCENE_SOCCER: RipRef = RipRef {
    name: "g_sceneSoccer",
    sig: &SCENE_UPDATE_CALL,
    disp_off: 3,
    next_ip_off: 7,
    rva: 0x21F6350,
};
/// Site in `CSceneSoccer::Update`: `movzx r13d, byte [rip+g_soccerState]; mov r12d,3Ah; cmp r13d,1Ah` (the actor
/// loop is skipped in state 0x1A = replay).
pub const SOCCER_STATE_SITE: Sig = Sig {
    name: "scene.SoccerStateSite",
    pattern: "44 0F B6 2D ?? ?? ?? ?? 41 BC 3A 00 00 00 41 83 FD 1A",
    offset: 0,
    rva: 0x16833AC,
};
/// Current soccer scene state id (byte, see [`soccer_state_name`]).
pub const G_SOCCER_STATE: RipRef = RipRef {
    name: "g_soccerState",
    sig: &SOCCER_STATE_SITE,
    disp_off: 4,
    next_ip_off: 8,
    rva: 0x21F65AA,
};
/// Scene (`[g_sceneSoccer]`) f32: game seconds of the current half.
pub const SCENE_CLOCK: usize = 0x2208;

/// Soccer scene state names (`g_soccerState`).
pub fn soccer_state_name(s: u8) -> &'static str {
    match s {
        0 => "none",
        1 => "Init",
        2 => "End",
        3 => "EndWait",
        4 => "Error",
        5 => "BattleSetting",
        6 => "ResultEvent",
        7 => "ResultMenu",
        8 => "GameOver",
        9 => "InPlay",
        10 => "Restart",
        11 => "HalfTime",
        12 => "GoalNet",
        13 => "Goal",
        14 => "FocusBtl",
        15 => "Scramble",
        16 => "Zone",
        17 => "OutOfPlay",
        20 => "Menu",
        21 => "NetworkError",
        22 => "Retire",
        23 => "TrainingStart",
        24 => "TrainingEnd",
        25 => "TrainingReset",
        26 => "Replay",
        27 => "RematchSetting",
        28 => "TournamentFinalGameEnd",
        _ => "?",
    }
}

/// Every code signature (for the static test and the startup self-check).
pub const ALL: &[&Sig] = &[
    &LUA_COMMAND_DISPATCH,
    &LUA_GETTOP,
    &LUA_TONUMBERX,
    &LUA_TOLSTRING,
    &LUA_TYPE,
    &LUA_PUSHNUMBER,
    &LUA_PUSHBOOLEAN,
    &LUA_PUSHSTRING,
    &SCENE_UPDATE_CALL,
    &SOCCER_STATE_SITE,
];
pub const ALL_RIP: &[&RipRef] = &[&G_SCENE_SOCCER, &G_SOCCER_STATE];

/// Inline-hook targets and their stolen byte counts (the steal must be wildcard-free, whole instructions).
pub const INLINE_HOOKS: &[(&Sig, usize)] = &[(&LUA_COMMAND_DISPATCH, LUA_COMMAND_DISPATCH_STEAL)];
