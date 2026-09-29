//! Machine code of a `watch` logpoint (module `debug`). Pure byte builders; `watch.rs` places them in executable
//! memory and `tests` there run them.
//!
//! Stub (rbp frame, registered with `RtlAddFunctionTable` so exceptions can unwind through it):
//! ```text
//! push rbp; mov rbp, rsp                       ; prolog (4 bytes, see UNWIND_INFO)
//! push rcx; push rdx; push r8; push r9         ; regs[3..0] at rbp-8 .. rbp-0x20
//! sub rsp, 0xA0; movdqu xmm0..3 -> [rbp-0x30 .. rbp-0x60]
//! eax = enter(id, &regs, &return_address)     ; 0 = pass through, 1 = also log the return value
//! restore xmm0..3, rcx, rdx, r8, r9
//! eax == 0: mov rsp, rbp; pop rbp; jmp [tramp] ; tail jump, the original returns to the caller itself
//! eax != 0: copy 12 stack arguments into a new frame; call tramp; exit(id, rax, xmm0 bits); return rax/xmm0
//! ```

/// `mov rsp, rbp; pop rbp` + epilogue helpers.
fn epilogue(c: &mut Vec<u8>) {
    c.extend_from_slice(&[0x48, 0x89, 0xEC, 0x5D]);
}

fn mov_rax_imm(c: &mut Vec<u8>, v: u64) {
    c.extend_from_slice(&[0x48, 0xB8]);
    c.extend_from_slice(&v.to_le_bytes());
}

fn mov_rcx_imm(c: &mut Vec<u8>, v: u64) {
    c.extend_from_slice(&[0x48, 0xB9]);
    c.extend_from_slice(&v.to_le_bytes());
}

/// Number of stack arguments (5th and later) copied for the "log return value" path.
pub const STACK_ARGS: usize = 12;
/// Size of the prolog described by [`UNWIND_INFO`].
pub const PROLOG: u8 = 4;

pub fn watch_stub(id: u64, enter: u64, exit: u64, tramp: u64) -> Vec<u8> {
    let mut c = Vec::with_capacity(0x180);
    c.push(0x55); // push rbp
    c.extend_from_slice(&[0x48, 0x89, 0xE5]); // mov rbp, rsp
    debug_assert_eq!(c.len(), PROLOG as usize);
    c.extend_from_slice(&[0x51, 0x52, 0x41, 0x50, 0x41, 0x51]); // push rcx, rdx, r8, r9
    c.extend_from_slice(&[0x48, 0x81, 0xEC, 0xA0, 0x00, 0x00, 0x00]); // sub rsp, 0xA0
    for (i, disp) in [0xD0u8, 0xC0, 0xB0, 0xA0].iter().enumerate() {
        c.extend_from_slice(&[0xF3, 0x0F, 0x7F, 0x45 | ((i as u8) << 3), *disp]); // movdqu [rbp-x], xmmI
    }
    mov_rcx_imm(&mut c, id);
    c.extend_from_slice(&[0x48, 0x8D, 0x55, 0xE0]); // lea rdx, [rbp-0x20]
    c.extend_from_slice(&[0x4C, 0x8D, 0x45, 0x08]); // lea r8, [rbp+8]
    mov_rax_imm(&mut c, enter);
    c.extend_from_slice(&[0xFF, 0xD0]); // call rax
    c.extend_from_slice(&[0x89, 0x45, 0x98]); // mov [rbp-0x68], eax
    for (i, disp) in [0xD0u8, 0xC0, 0xB0, 0xA0].iter().enumerate() {
        c.extend_from_slice(&[0xF3, 0x0F, 0x6F, 0x45 | ((i as u8) << 3), *disp]); // movdqu xmmI, [rbp-x]
    }
    c.extend_from_slice(&[0x4C, 0x8B, 0x4D, 0xE0]); // mov r9, [rbp-0x20]
    c.extend_from_slice(&[0x4C, 0x8B, 0x45, 0xE8]); // mov r8, [rbp-0x18]
    c.extend_from_slice(&[0x48, 0x8B, 0x55, 0xF0]); // mov rdx, [rbp-0x10]
    c.extend_from_slice(&[0x48, 0x8B, 0x4D, 0xF8]); // mov rcx, [rbp-8]
    c.extend_from_slice(&[0x83, 0x7D, 0x98, 0x00]); // cmp dword [rbp-0x68], 0
    c.extend_from_slice(&[0x75, 0x00]); // jne LOG (patched below)
    let jne_at = c.len() - 1;
    epilogue(&mut c);
    c.extend_from_slice(&[0xFF, 0x25, 0, 0, 0, 0]); // jmp [rip+0]
    c.extend_from_slice(&tramp.to_le_bytes());
    let log = c.len();
    c[jne_at] = (log - (jne_at + 1)) as u8;
    c.extend_from_slice(&[0x48, 0x81, 0xEC, 0x80, 0x00, 0x00, 0x00]); // sub rsp, 0x80
    for i in 0..STACK_ARGS as u32 {
        c.extend_from_slice(&[0x48, 0x8B, 0x85]); // mov rax, [rbp + 0x30 + 8i]
        c.extend_from_slice(&(0x30 + 8 * i).to_le_bytes());
        c.extend_from_slice(&[0x48, 0x89, 0x84, 0x24]); // mov [rsp + 0x20 + 8i], rax
        c.extend_from_slice(&(0x20 + 8 * i).to_le_bytes());
    }
    mov_rax_imm(&mut c, tramp);
    c.extend_from_slice(&[0xFF, 0xD0]); // call rax
    c.extend_from_slice(&[0x48, 0x89, 0x45, 0x90]); // mov [rbp-0x70], rax
    c.extend_from_slice(&[0xF3, 0x0F, 0x7F, 0x45, 0xD0]); // movdqu [rbp-0x30], xmm0
    mov_rcx_imm(&mut c, id);
    c.extend_from_slice(&[0x48, 0x89, 0xC2]); // mov rdx, rax
    c.extend_from_slice(&[0x66, 0x49, 0x0F, 0x7E, 0xC0]); // movq r8, xmm0
    mov_rax_imm(&mut c, exit);
    c.extend_from_slice(&[0xFF, 0xD0]); // call rax
    c.extend_from_slice(&[0x48, 0x8B, 0x45, 0x90]); // mov rax, [rbp-0x70]
    c.extend_from_slice(&[0xF3, 0x0F, 0x6F, 0x45, 0xD0]); // movdqu xmm0, [rbp-0x30]
    epilogue(&mut c);
    c.push(0xC3); // ret
    c
}

/// Trampoline: the stolen bytes, then `jmp [rip+0]` back to `resume`.
pub fn trampoline(stolen: &[u8], resume: u64) -> Vec<u8> {
    let mut c = stolen.to_vec();
    c.extend_from_slice(&[0xFF, 0x25, 0, 0, 0, 0]);
    c.extend_from_slice(&resume.to_le_bytes());
    c
}

/// Patch written at the hooked function: `jmp [rip+0]; dq stub`, padded with `nop` to `len` bytes.
pub fn patch(stub: u64, len: usize) -> Vec<u8> {
    let mut c = vec![0xFF, 0x25, 0, 0, 0, 0];
    c.extend_from_slice(&stub.to_le_bytes());
    c.resize(len.max(14), 0x90);
    c
}

/// UNWIND_INFO of the stub: version 1, prolog 4 bytes, 2 codes, frame register rbp (offset 0);
/// codes: at +4 UWOP_SET_FPREG, at +1 UWOP_PUSH_NONVOL(rbp).
pub const UNWIND_INFO: [u8; 8] = [0x01, PROLOG, 0x02, 0x05, 0x04, 0x03, 0x01, 0x50];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stub_layout() {
        let s = watch_stub(7, 0x1111, 0x2222, 0x3333);
        assert_eq!(&s[..4], &[0x55, 0x48, 0x89, 0xE5]);
        assert_eq!(*s.last().unwrap(), 0xC3);
        // the jne lands on `sub rsp, 0x80`
        let jne = s.windows(4).position(|w| w == [0x83, 0x7D, 0x98, 0x00]).unwrap() + 4;
        assert_eq!(s[jne], 0x75);
        let target = jne + 2 + s[jne + 1] as usize;
        assert_eq!(&s[target..target + 7], &[0x48, 0x81, 0xEC, 0x80, 0x00, 0x00, 0x00]);
        assert!(s.len() < 0x200);
        assert_eq!(patch(0x1234, 15).len(), 15);
        assert_eq!(patch(0x1234, 15)[14], 0x90);
        assert_eq!(trampoline(&[1, 2], 5).len(), 16);
    }
}
