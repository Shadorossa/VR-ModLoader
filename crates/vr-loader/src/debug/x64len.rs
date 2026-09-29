//! Minimal x86-64 instruction length decoder for function prologues (module `debug`, `watch`).
//!
//! Only a whitelist of position-independent instructions is accepted (push/pop, mov/lea/arithmetic with a ModRM
//! operand, `sub rsp, imm`, SSE moves, ...). Anything that depends on its own address (RIP-relative operands,
//! relative calls/jumps, `ret`, `int3`) is refused, so the accepted bytes can be copied verbatim into a trampoline.

/// Length of the instruction at the start of `code`, or why it cannot be relocated.
pub fn insn_len(code: &[u8]) -> Result<usize, String> {
    let at = |i: usize| code.get(i).copied().ok_or_else(|| "truncated instruction".to_string());
    let mut i = 0usize;
    let mut opsize16 = false;
    // legacy prefixes
    loop {
        match at(i)? {
            0x66 => opsize16 = true,
            0xF2 | 0xF3 | 0x2E | 0x3E | 0x26 | 0x36 | 0x64 | 0x65 => {}
            0xF0 => {}
            _ => break,
        }
        i += 1;
        if i > 4 {
            return Err("too many prefixes".into());
        }
    }
    let mut rex_w = false;
    let b = at(i)?;
    if (0x40..=0x4F).contains(&b) {
        rex_w = b & 8 != 0;
        i += 1;
    }
    let op = at(i)?;
    i += 1;
    let imm_z = if opsize16 { 2 } else { 4 };
    let n = match op {
        0x50..=0x5F => i,
        0x90 => i,
        0x00..=0x03 | 0x08..=0x0B | 0x10..=0x13 | 0x18..=0x1B | 0x20..=0x23 | 0x28..=0x2B | 0x30..=0x33
        | 0x38..=0x3B | 0x63 | 0x84..=0x8B | 0x8D | 0xD1 | 0xD3 => i + modrm_len(code, i)?,
        0x80 | 0x82 | 0x83 | 0xC0 | 0xC1 | 0xC6 | 0x6B => i + modrm_len(code, i)? + 1,
        0x81 | 0xC7 | 0x69 => i + modrm_len(code, i)? + imm_z,
        0xF6 => {
            let reg = (at(i)? >> 3) & 7;
            i + modrm_len(code, i)? + if reg <= 1 { 1 } else { 0 }
        }
        0xF7 => {
            let reg = (at(i)? >> 3) & 7;
            i + modrm_len(code, i)? + if reg <= 1 { imm_z } else { 0 }
        }
        0xFF => {
            let reg = (at(i)? >> 3) & 7;
            if !matches!(reg, 0 | 1 | 6) {
                return Err(format!("indirect call/jmp (FF /{reg}) cannot be relocated"));
            }
            i + modrm_len(code, i)?
        }
        0xB0..=0xB7 => i + 1,
        0xB8..=0xBF => i + if rex_w { 8 } else { imm_z },
        0x04 | 0x0C | 0x14 | 0x1C | 0x24 | 0x2C | 0x34 | 0x3C | 0xA8 => i + 1,
        0x05 | 0x0D | 0x15 | 0x1D | 0x25 | 0x2D | 0x35 | 0x3D | 0xA9 => i + imm_z,
        0x0F => {
            let op2 = at(i)?;
            i += 1;
            match op2 {
                0x10..=0x17 | 0x1F | 0x28..=0x2F | 0x40..=0x4F | 0x51..=0x5F | 0x6E | 0x6F | 0x7E | 0x7F
                | 0x90..=0x9F | 0xAF | 0xB6 | 0xB7 | 0xBE | 0xBF | 0xD6 | 0xEF | 0xC6 => {
                    i + modrm_len(code, i)? + if op2 == 0xC6 { 1 } else { 0 }
                }
                0x80..=0x8F => return Err("conditional jump cannot be relocated".into()),
                _ => return Err(format!("unsupported opcode 0F {op2:02X}")),
            }
        }
        0xE8 => return Err("relative call cannot be relocated".into()),
        0xE9 | 0xEB | 0x70..=0x7F | 0xE3 => return Err("relative jump cannot be relocated".into()),
        0xC3 | 0xC2 => return Err("function ends before 14 bytes (ret)".into()),
        0xCC => return Err("int3 (padding / breakpoint)".into()),
        0xC4 | 0xC5 => return Err("VEX instruction not supported".into()),
        _ => return Err(format!("unsupported opcode {op:02X}")),
    };
    if n > code.len() {
        return Err("truncated instruction".into());
    }
    Ok(n)
}

/// Bytes taken by ModRM (+ SIB + displacement) at `code[i]`. RIP-relative operands are refused.
fn modrm_len(code: &[u8], i: usize) -> Result<usize, String> {
    let m = *code.get(i).ok_or("truncated ModRM")?;
    let md = m >> 6;
    let rm = m & 7;
    if md == 3 {
        return Ok(1);
    }
    let mut n = 1;
    if rm == 4 {
        let sib = *code.get(i + 1).ok_or("truncated SIB")?;
        n += 1;
        if md == 0 && sib & 7 == 5 {
            n += 4;
        }
    } else if md == 0 && rm == 5 {
        return Err("RIP-relative operand cannot be copied".into());
    }
    n += match md {
        1 => 1,
        2 => 4,
        _ => 0,
    };
    Ok(n)
}

/// Number of whole, relocatable instruction bytes at the start of `code` covering at least `min` bytes.
pub fn relocatable_prefix(code: &[u8], min: usize) -> Result<usize, String> {
    let mut n = 0;
    while n < min {
        let l = insn_len(&code[n..]).map_err(|e| format!("+0x{n:X}: {e}"))?;
        n += l;
    }
    Ok(n)
}

fn hex(s: &str) -> Vec<u8> {
    s.split_whitespace().map(|t| u8::from_str_radix(t, 16).unwrap()).collect()
}

/// Test helper (also used by the docs examples): prefix length of a hex byte string.
pub fn prefix_of_hex(s: &str, min: usize) -> Result<usize, String> {
    let mut b = hex(s);
    b.extend_from_slice(&[0xCC; 16]);
    relocatable_prefix(&b, min)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_prologues() {
        // lua.CommandDispatch: 3 x mov [rsp+x], reg
        assert_eq!(prefix_of_hex("48 89 5C 24 10 48 89 74 24 18 48 89 7C 24 20 41 56 48 83 EC 20", 14), Ok(15));
        // stats.GetCharaParamStatus: mov [rsp+18h], rbx + pushes (boundary at 14; the stats hook steals 16)
        assert_eq!(prefix_of_hex("48 89 5C 24 18 55 56 57 41 54 41 55 41 56 41 57 48 81 EC 30 01 00 00", 14), Ok(14));
        assert_eq!(prefix_of_hex("48 89 5C 24 18 55 56 57 41 54 41 55 41 56 41 57 48 81 EC 30 01 00 00", 16), Ok(16));
        // stamina.UnitTick: push rsi; sub rsp,50h; movaps [rsp+20h],xmm8; mov rsi,rcx
        assert_eq!(prefix_of_hex("40 56 48 83 EC 50 44 0F 29 44 24 20 48 8B F1 44 0F 28 C1", 14), Ok(15));
        // lua_pcallk (17) and luaL_loadbufferx (14)
        assert_eq!(prefix_of_hex("48 89 74 24 18 57 48 83 EC 40 33 F6 48 89 6C 24 58 41 8B E8", 14), Ok(17));
        assert_eq!(prefix_of_hex("48 83 EC 48 48 8B 44 24 70 48 89 54 24 30 48 8D 15 00 00 00 00", 14), Ok(14));
        // lua_gettop (leaf): mov rax,[rcx+20h]; mov rdx,[rcx+10h]; sub rdx,[rax]; lea rax,[rdx-10h]
        assert_eq!(prefix_of_hex("48 8B 41 20 48 8B 51 10 48 2B 10 48 8D 42 F0 48 C1 F8 04 C3", 14), Ok(15));
        // mov r11, rsp; push rbp; push rsi; sub rsp, 88h; mov rsi, rcx
        assert_eq!(prefix_of_hex("4C 8B DC 55 56 48 81 EC 88 00 00 00 48 8B F1 8B 0A", 14), Ok(15));
    }

    #[test]
    fn refuses_position_dependent_code() {
        // skill_rank.OnHissatsuUsed: mov r8, [rip+x] at +8
        assert!(prefix_of_hex("40 53 41 54 48 83 EC 38 4C 8B 05 11 22 33 44 4C 8B E2", 14).unwrap_err().contains("RIP"));
        // lua_insert: call index2addr at +7
        assert!(prefix_of_hex("48 83 EC 28 4C 8B D9 E8 00 00 00 00 49 8B 53 10", 14).unwrap_err().contains("call"));
        // short function: ret before 14 bytes
        assert!(prefix_of_hex("48 8B 41 10 C3", 14).is_err());
        // jne inside the steal
        assert!(prefix_of_hex("40 53 48 83 EC 60 48 8B D9 48 85 C9 75 0E F3 0F 10 05", 14).is_err());
        // padding
        assert!(prefix_of_hex("CC CC CC", 14).is_err());
    }

    #[test]
    fn modrm_forms() {
        assert_eq!(insn_len(&hex("48 89 84 24 20 01 00 00")), Ok(8)); // mov [rsp+120h], rax
        assert_eq!(insn_len(&hex("0F 29 BC 24 A0 00 00 00")), Ok(8)); // movaps [rsp+0A0h], xmm7
        assert_eq!(insn_len(&hex("48 B8 01 02 03 04 05 06 07 08")), Ok(10)); // mov rax, imm64
        assert_eq!(insn_len(&hex("B8 01 00 00 00")), Ok(5));
        assert_eq!(insn_len(&hex("C7 44 24 34 FF FF FF 7F")), Ok(8)); // mov dword [rsp+34h], imm32
        assert_eq!(insn_len(&hex("66 C7 44 24 34 FF FF")), Ok(7)); // mov word [rsp+34h], imm16
        assert_eq!(insn_len(&hex("F3 0F 7F 45 D0")), Ok(5)); // movdqu [rbp-30h], xmm0
        assert_eq!(insn_len(&hex("8B 04 25 00 10 00 00")), Ok(7)); // mov eax, [abs32] (SIB, no base)
        assert!(insn_len(&hex("FF 15 00 00 00 00")).is_err()); // call [rip+x]
        assert!(insn_len(&hex("FF 25 00 00 00 00")).is_err()); // jmp [rip+x] (a hook)
    }
}
