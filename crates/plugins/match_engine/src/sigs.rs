//! Signatures and data offsets of the match engine plugin (nie.exe PC v7.1.2): docs/game/modes/match-engine.md §1-§2,
//! §12 and prorroga-penaltis.md §8. Every pattern is unique in `.text` at its RVA (test `sigs_unique_on_dump`); the
//! plugin resolves them through the ModLoader (`sig_find` searches the unpatched code, so a built-in hook on the same
//! function does not hide it).

/// One signature: name (log), IDA pattern, expected RVA (log only).
#[derive(Debug, Clone, Copy)]
pub struct Sig {
    pub name: &'static str,
    pub pattern: &'static str,
    pub rva: u32,
}

/// A RIP-relative operand inside a signature: (signature, displacement offset, next-instruction offset, expected RVA).
#[derive(Debug, Clone, Copy)]
pub struct Rip {
    pub name: &'static str,
    pub sig: &'static Sig,
    pub disp_off: u32,
    pub next_ip_off: u32,
    pub rva: u32,
}

/// `TeamRecordBuild(rcx = team record, rdx = &params) -> bool` 0x16B34F0: builds one team record of the match being
/// set up (`mi_local + 0x60 + team*0x1130`; `MatchSetupA 0x16B4840` / `MatchSetupB 0x16B59D0` build the whole match
/// info in a stack copy and `memcpy` it (0x2C68 bytes) to `[[g_gameRoot]+0x6A58]` after they return, CSceneSoccer init
/// 0x16C4468). `params`: `+0 u32 on-pitch count` (11 / 5 / m_SoccerGameEx own/oppNum), `+4 u8 team`. Called 4 times,
/// only from the two setups, after every rule field of `mi_local` is written (0x16B4BB3 ExRule, 0x16B4CD4 rule bits,
/// 0x16B4CDC game id, 0x16B4D21 period, 0x16B4E79 user side). Prologue 16 bytes, no RIP operand.
pub const TEAM_RECORD_BUILD: Sig = Sig {
    name: "match_engine.TeamRecordBuild",
    pattern: "48 89 5C 24 18 55 56 57 41 54 41 55 41 56 41 57 48 8D AC 24 50 FE FF FF 48 81 EC B0 02 00 00 48 8B 05 ?? ?? ?? ?? 48 33 C4 48 89 85 A0 01 00 00 4C 8B FA",
    rva: 0x16B34F0,
};
pub const TEAM_RECORD_BUILD_STEAL: usize = 16;

/// `TeamBuild(u8 side, TeamSetup* s)` 0xE9C190: builds one side's squad; `s+0 u32` = team id (rival: the difficulty
/// row's c1 `row+0xC`; own side 0 = the user's team unless the row overrides it, `row+0x78`), `+6 u16` level, `+8`
/// AI tier. Called only by the two match setups (0x16B4F1C / 0x16B4FA9, 0x16B64C4 / 0x16B66C7), user side first.
/// The plugin only reads the team ids. Prologue `mov r11,rsp; push rbp; push rdi; lea rbp,[r11-2A8h]; sub rsp,398h`
/// → 19 bytes, no RIP operand.
pub const TEAM_BUILD: Sig = Sig {
    name: "match_engine.TeamBuild",
    pattern: "4C 8B DC 55 57 49 8D AB 58 FD FF FF 48 81 EC 98 03 00 00 48 8B 05 ?? ?? ?? ?? 48 33 C4 48 89 85 50 02 00 00 44 0F B6 C1",
    rva: 0xE9C190,
};
pub const TEAM_BUILD_STEAL: usize = 19;

/// `u8 NextHalf(u8 half)` 0x16CBDE0 (prorroga-penaltis.md §8.1). Its `mov rax, [rip+x]` at +0x1A is g_gameRoot.
/// Prologue: 3 `mov [rsp+x], reg` → 15 bytes stolen.
pub const NEXT_HALF: Sig = Sig {
    name: "match_engine.NextHalf",
    pattern: "48 89 5C 24 08 48 89 6C 24 10 48 89 74 24 18 48 89 7C 24 20 41 56 48 83 EC 20 48 8B 05 ?? ?? ?? ?? 0F B6 F9 48 8B 90 58 6A 00 00 8B 32 8B EE 83",
    rva: 0x16CBDE0,
};
pub const NEXT_HALF_STEAL: usize = 15;
pub const G_GAME_ROOT: Rip = Rip { name: "match_engine.g_gameRoot", sig: &NEXT_HALF, disp_off: 0x1D, next_ip_off: 0x21, rva: 0x21F62A0 };

/// `ApplyReservedOutOfPlay` 0x16CBEB0: only for its `mov rcx, [rip+x]` (g_soccerGame 0x21F69D8).
pub const APPLY_RESERVED: Sig = Sig {
    name: "match_engine.ApplyReservedOutOfPlay",
    pattern: "48 83 EC 28 48 8B 0D ?? ?? ?? ?? 48 85 C9 0F 84 9C 00 00 00 8B 81 68 01 00 00 0F 57 C0 89 81 50",
    rva: 0x16CBEB0,
};
pub const G_SOCCER_GAME: Rip = Rip { name: "match_engine.g_soccerGame", sig: &APPLY_RESERVED, disp_off: 7, next_ip_off: 11, rva: 0x21F69D8 };

/// `FlagEntry* FindFlag(u8 bank, u32* hash)` 0xE8B260 (restart-type temp byte flag lookup).
pub const FIND_FLAG: Sig = Sig {
    name: "match_engine.FindFlag",
    pattern: "48 89 5C 24 08 48 89 74 24 10 57 48 83 EC 20 48 8B 05 ?? ?? ?? ?? 0F B6 F1 48 8B FA 48 8B 98 40 02 00 00 48 8B CB 48 8B 03 FF 50 10 84 C0 74 27",
    rva: 0xE8B260,
};

/// `bool InPlayState::Update(state, request)` 0x1432B80, slot 2 of the InPlay state vtable `.rdata 0x1A32480`.
pub const INPLAY_UPDATE: Sig = Sig {
    name: "match_engine.InPlayUpdate",
    pattern: "48 89 6C 24 20 56 57 41 56 48 83 EC 60 4C 8B 35 ?? ?? ?? ?? 48 8B FA 48 8B 2D ?? ?? ?? ?? 4D 85 F6 48 8B F1 41 0F 94 C0",
    rva: 0x1432B80,
};
pub const INPLAY_VTABLE_RVA: usize = 0x1A32480;
pub const VTABLE_UPDATE_SLOT: usize = 2;

/// Scene update call site (g_sceneSoccer `[rip+x]` at +0): the clock object.
pub const SCENE_UPDATE_CALL: Sig = Sig {
    name: "match_engine.SceneUpdateCall",
    pattern: "48 8B 0D ?? ?? ?? ?? 48 85 C9 74 09 41 0F 28 C8 E8 ?? ?? ?? ?? 4C 8D 9C 24 A0 00 00 00",
    rva: 0x1423EA5,
};
pub const G_SCENE: Rip = Rip { name: "match_engine.g_sceneSoccer", sig: &SCENE_UPDATE_CALL, disp_off: 3, next_ip_off: 7, rva: 0x21F6350 };

/// `CalcUnitMoveSpeed` 0x154DAA0: its `mov r9, [rip+x]` at +0xC = the soccer actor array.
pub const CALC_MOVE_SPEED: Sig = Sig {
    name: "match_engine.CalcUnitMoveSpeed",
    pattern: "4C 8B DC 53 55 57 41 55 48 83 EC 48 4C 8B 0D ?? ?? ?? ??",
    rva: 0x154DAA0,
};
pub const G_ACTORS: Rip = Rip { name: "match_engine.g_soccerActors", sig: &CALC_MOVE_SPEED, disp_off: 0x0F, next_ip_off: 0x13, rva: 0x21F69C8 };

/// Ball-owner site 0xC2A98A: `[rip+x]` = pointer to the ball-owner block (`+0x1494` owner handle, `+0x1498` team).
pub const BALL_OWNER_SITE: Sig = Sig {
    name: "match_engine.BallOwnerSite",
    pattern: "48 8B 05 ?? ?? ?? ?? 33 D2 48 85 C0 74 09 39 88 94 14 00 00 0F 94 C2",
    rva: 0xC2A98A,
};
pub const G_BALL: Rip = Rip { name: "match_engine.g_ballOwner", sig: &BALL_OWNER_SITE, disp_off: 3, next_ip_off: 7, rva: 0x1E62AA0 };

pub const ALL: &[&Sig] =
    &[&TEAM_RECORD_BUILD, &TEAM_BUILD, &NEXT_HALF, &APPLY_RESERVED, &FIND_FLAG, &INPLAY_UPDATE, &SCENE_UPDATE_CALL, &CALC_MOVE_SPEED, &BALL_OWNER_SITE];
pub const ALL_RIP: &[&Rip] = &[&G_GAME_ROOT, &G_SOCCER_GAME, &G_SCENE, &G_ACTORS, &G_BALL];
pub const HOOKS: &[(&Sig, usize)] =
    &[(&TEAM_RECORD_BUILD, TEAM_RECORD_BUILD_STEAL), (&TEAM_BUILD, TEAM_BUILD_STEAL), (&NEXT_HALF, NEXT_HALF_STEAL)];

/// Network predicate 0x1239C70 (`mov rdx, [rip+x]` + the tail below; it has an identical twin at 0x1239ED0, so it is
/// checked at its RVA instead of searched): same check as the loader's `prematch`.
pub const NET_FN_RVA: usize = 0x1239C70;
pub const NET_FN_HEAD: [u8; 3] = [0x48, 0x8B, 0x15];
pub const NET_FN_TAIL: [u8; 52] = [
    0x8B, 0x82, 0x10, 0x24, 0x00, 0x00, 0x85, 0xC0, 0x74, 0x27, 0xFF, 0xC8, 0x0F, 0xB6, 0x84, 0x10, 0x14, 0x24, 0x00, 0x00,
    0x3C, 0x02, 0x73, 0x19, 0x48, 0x69, 0xC0, 0x08, 0x12, 0x00, 0x00, 0x48, 0x03, 0xC2, 0x74, 0x0D, 0x0F, 0xB6, 0x80, 0x00,
    0x12, 0x00, 0x00, 0xC0, 0xE8, 0x02, 0x24, 0x01, 0xC3, 0x32, 0xC0, 0xC3,
];

/// `network_active` of the loader's `prematch` (reimplements the predicate on the object `g = [0x21F6348]`).
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

// ---------------------------------------------------------------- match info `mi = [[g_gameRoot]+0x6A58]`

pub const OFF_MATCH: usize = 0x6A58;
/// `u32` rule bits: bit 0 extra time (difficulty c21 `+0x76`, 0 in every retail row), bit 1 two halves (set when
/// difficulty c22 `+0x77` is 0: full matches; small matches have c22 = 1 = one period), bit 17 story V-goal
/// (SOCCER_GAME_INFO `+0x1F`), bit 18 (`+0x20`, unknown); written by the row reader 0xEBA72B.
pub const MI_RULES: usize = 0x0;
/// `u32` SOCCER_GAME_INFO row of the match (crc32 of its name; the redirected row when the difficulty row redirects).
pub const MI_GAME: usize = 0x4;
/// `u32` row asked for by `CMND_RESERVE_SOCCER` when redirected (0 = not redirected).
pub const MI_ORIG_GAME: usize = 0x8;
pub const MI_TYPE: usize = 0x16;
pub const MI_PERIOD_SECS: usize = 0x1C;
/// `u8` observer (network).
pub const MI_OBSERVER: usize = 0x54;
pub const MI_TEAMS: usize = 0x60;
pub const TEAM_STRIDE: usize = 0x1130;
/// `u32` ExRule bits (bit n = type n of the m_SoccerGameEx exRule list; 0x16B4BB3).
pub const MI_EXRULE: usize = 0x2BF4;
pub const MI_HALF: usize = 0x2BFB;
pub const MI_ADDED: usize = 0x2C00;
pub const MI_SCORE: usize = 0x2C26;
pub const MI_USER: usize = 0x2C29;
/// Play mode byte `[[g_gameRoot]+0x69C8]+0x2CAC6F`: 1 VS / friendlies, 2 and 6 story, 3 Kizuna town, 4 Chronicle,
/// 5 Victory Road (outfit-slot.md §2).
pub const ROOT_PLAYDATA: usize = 0x69C8;
pub const PLAYDATA_MODE: usize = 0x2CAC6F;

/// Member `k` of a team record at `+ (k + 1) * 0x80`.
pub const TEAM_MEMBER_STRIDE: usize = 0x80;
pub const MEMBER_SLOTS: usize = 0x1D;
pub const MEM_CHARA: usize = 0x50;
/// `+0x64` = its soccer actor handle (0xEBAE48).
pub const MEM_ACTOR: usize = 0x64;
pub const MEM_POSITION: usize = 0x71;
pub const MEM_FLAGS: usize = 0x7A;
/// Member flags: 0x800 empty, 0x08 bench (0xEB2900).
pub const MEM_FLAG_EMPTY: u16 = 0x800;
pub const MEM_NOT_PLAYING: u16 = 0x808;
pub const TEAM_FORMATION: usize = 0x1038;
pub const TEAM_ON_PITCH: usize = 0x1050;
pub const TEAM_VALID_MEMBERS: usize = 0x1054;

/// `params` of TeamRecordBuild.
pub const PARAM_COUNT: usize = 0x0;
pub const PARAM_TEAM: usize = 0x4;
/// TeamBuild setup: team id.
pub const SETUP_TEAM_ID: usize = 0x0;
/// Team setup TeamRecordBuild reads: `[[g_gameRoot] + 0x69A8] + 0x1E0` + team*0x4A8; formation id at `+0x28C`.
pub const ROOT_SETUP: usize = 0x69A8;
pub const SETUP_TEAMS: usize = 0x1E0;
pub const SETUP_TEAM_STRIDE: usize = 0x4A8;
pub const SETUP_FORMATION: usize = 0x28C;

// ---------------------------------------------------------------- end of match (prorroga-penaltis.md §8)

/// Clock object `[g_sceneSoccer]`: clock `+0x2208` (f32 game seconds of the period), flags `+0x2024`.
pub const SCENE_CLOCK: usize = 0x2208;
pub const SCENE_FLAGS: usize = 0x2024;
/// «V-goal» flag of the clock object (golden goal).
pub const FLAG_VGOAL: u32 = 0x200;
/// Reserved restart in `[g_soccerGame]` (CMND 0x20390615 handler 0xC2ABE0).
pub const RSV_X: usize = 0x168;
pub const RSV_Z: usize = 0x16C;
pub const RSV_F: usize = 0x170;
pub const RSV_ACTOR: usize = 0x174;
pub const RSV_ARG5: usize = 0x178;
pub const RSV_TEAM: usize = 0x17C;
pub const RSV_KIND: usize = 0x17D;
pub const RSV_ARG6: usize = 0x17E;
pub const RSV_PAD: usize = 0x17F;
pub const KIND_FOUL: u8 = 6;
pub const RESTART_PENALTY: u8 = 8;
pub const FLAG_BANK: u8 = 0x15;
pub const FLAG_RESTART_TYPE: u32 = 0x74C9A8FD;
pub const ROOT_FLAGS: usize = 0x69B8;
pub const FLAGS_BYTES: usize = 0x19728;
/// Soccer actors `[0x21F69C8] + i * 0x570`, i < 0x3A: handle `+0x94`, on the pitch `+0x497`.
pub const ACTOR_STRIDE: usize = 0x570;
pub const ACTOR_COUNT: usize = 0x3A;
pub const ACTOR_HDL: usize = 0x94;
pub const ACTOR_IN_PLAY: usize = 0x497;
/// Ball-owner block.
pub const BALL_OWNER: usize = 0x1494;
pub const BALL_OWNER_TEAM: usize = 0x1498;

#[cfg(test)]
mod tests {
    use super::*;

    /// Minimal PE reader: (.text RVA, .text bytes) of the dump.
    fn text_of(file: &[u8]) -> Option<(u32, Vec<u8>)> {
        let rd32 = |o: usize| u32::from_le_bytes(file[o..o + 4].try_into().unwrap());
        let rd16 = |o: usize| u16::from_le_bytes(file[o..o + 2].try_into().unwrap());
        let pe = rd32(0x3C) as usize;
        let nsec = rd16(pe + 6) as usize;
        let opt = rd16(pe + 20) as usize;
        let sec0 = pe + 24 + opt;
        (0..nsec).find_map(|i| {
            let s = sec0 + i * 40;
            (&file[s..s + 5] == b".text").then(|| {
                let (vsize, rva, raw_size, raw_off) = (rd32(s + 8), rd32(s + 12), rd32(s + 16), rd32(s + 20));
                (rva, file[raw_off as usize..(raw_off + raw_size.min(vsize)) as usize].to_vec())
            })
        })
    }

    fn find(text: &[u8], pat: &[Option<u8>], max: usize) -> Vec<usize> {
        let mut v = Vec::new();
        if pat.is_empty() || text.len() < pat.len() {
            return v;
        }
        for i in 0..=text.len() - pat.len() {
            if pat.iter().enumerate().all(|(k, b)| b.map_or(true, |b| text[i + k] == b)) {
                v.push(i);
                if v.len() >= max {
                    break;
                }
            }
        }
        v
    }

    /// Every signature is unique at its RVA in the v7.1.2 dump, the stolen bytes are fixed, the RIP operands point at
    /// the documented globals and the network predicate bytes match (skipped without the dump; EVT_NIE_EXE = path).
    #[test]
    fn sigs_unique_on_dump() {
        let p = std::env::var("EVT_NIE_EXE").unwrap_or_else(|_| {
            concat!(env!("CARGO_MANIFEST_DIR"), "/../../../assets/v7.1.2/nie.exe").to_string()
        });
        let Ok(file) = std::fs::read(&p) else {
            eprintln!("nie.exe dump not found: skipped");
            return;
        };
        let (trva, text) = text_of(&file).expect(".text");
        let at = |rva: u32| (rva - trva) as usize;
        for s in ALL {
            let pat = crate::pattern_bytes(s.pattern).unwrap();
            let hits = find(&text, &pat, 3);
            assert_eq!(hits.len(), 1, "{}: {} hits", s.name, hits.len());
            assert_eq!(trva + hits[0] as u32, s.rva, "{}", s.name);
        }
        for (s, steal) in HOOKS {
            let fixed = crate::fixed_prefix(s.pattern, *steal).expect("stolen bytes must be fixed");
            assert_eq!(&text[at(s.rva)..at(s.rva) + steal], &fixed[..], "{}", s.name);
        }
        for r in ALL_RIP {
            let insn = at(r.sig.rva);
            let d = i32::from_le_bytes(text[insn + r.disp_off as usize..insn + r.disp_off as usize + 4].try_into().unwrap());
            let target = (r.sig.rva as i64 + r.next_ip_off as i64 + d as i64) as u32;
            assert_eq!(target, r.rva, "{}", r.name);
        }
        let n = at(NET_FN_RVA as u32);
        assert_eq!(&text[n..n + 3], &NET_FN_HEAD);
        assert_eq!(&text[n + 7..n + 7 + NET_FN_TAIL.len()], &NET_FN_TAIL);
        // the setup writes the game id / period / user side before the TeamRecordBuild calls (MatchSetupA)
        assert_eq!(&text[at(0x16B4CDC)..at(0x16B4CDC) + 5], &[0x41, 0x89, 0x44, 0x24, 0x04]); // mov [r12+4], eax
        assert_eq!(&text[at(0x16B4D21)..at(0x16B4D21) + 6], &[0x66, 0x41, 0x89, 0x44, 0x24, 0x1C]); // mov [r12+1Ch], ax
        // TeamRecordBuild: count -> team+0x1050 (0x16B35E0)
        assert_eq!(&text[at(0x16B35E0)..at(0x16B35E0) + 10], &[0x41, 0x8B, 0x07, 0x41, 0x89, 0x86, 0x50, 0x10, 0, 0]);
    }
}
