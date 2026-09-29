//! Tiny hooking toolkit: pointer patch (IAT / vtable slots) and an inline hook whose stolen bytes are verified
//! against the signature (no relative operands allowed in them, so they can be copied verbatim).
//!
//! Hang diagnostics (`crate::debug::watchdog`): every inline hook gets an id; the patched jump goes to a small thunk
//! that records "detour entered" in [`HOOK_RING`] and jumps to the detour, and the trampoline records "original
//! entered" ([`ORIG_FLAG`]) before the stolen bytes. Both preserve every register and the flags (pushfq / push /
//! pop), so they are safe at mid-function sites too. [`hooks`] lists every hook (target, detour, trampoline, thunk).

use std::ptr;
use std::sync::atomic::{AtomicU32, Ordering as AOrd};
use std::sync::Mutex;
use windows_sys::Win32::System::Diagnostics::Debug::FlushInstructionCache;
use windows_sys::Win32::System::Memory::{
    VirtualAlloc, VirtualProtect, MEM_COMMIT, MEM_RESERVE, PAGE_EXECUTE_READWRITE, PAGE_PROTECTION_FLAGS,
    PAGE_READWRITE,
};
use windows_sys::Win32::System::Threading::GetCurrentProcess;

/// Write `bytes` at `addr` (any protection), restoring the protection and flushing the i-cache.
pub unsafe fn write_code(addr: usize, bytes: &[u8]) -> bool {
    let mut old: PAGE_PROTECTION_FLAGS = 0;
    if VirtualProtect(addr as _, bytes.len(), PAGE_EXECUTE_READWRITE, &mut old) == 0 {
        return false;
    }
    ptr::copy_nonoverlapping(bytes.as_ptr(), addr as *mut u8, bytes.len());
    let mut tmp: PAGE_PROTECTION_FLAGS = 0;
    VirtualProtect(addr as _, bytes.len(), old, &mut tmp);
    FlushInstructionCache(GetCurrentProcess(), addr as _, bytes.len());
    true
}

/// Replace a pointer-sized slot (IAT entry, vtable entry). Returns the old value.
pub unsafe fn patch_ptr(slot: *mut usize, value: usize) -> Option<usize> {
    let mut old: PAGE_PROTECTION_FLAGS = 0;
    if VirtualProtect(slot as _, 8, PAGE_READWRITE, &mut old) == 0 {
        return None;
    }
    let prev = ptr::read_volatile(slot);
    ptr::write_volatile(slot, value);
    let mut tmp: PAGE_PROTECTION_FLAGS = 0;
    VirtualProtect(slot as _, 8, old, &mut tmp);
    Some(prev)
}

fn abs_jmp(to: usize) -> [u8; 14] {
    let mut j = [0u8; 14];
    j[0] = 0xFF;
    j[1] = 0x25; // jmp qword ptr [rip+0]
    j[6..14].copy_from_slice(&(to as u64).to_le_bytes());
    j
}

/// Ring of the last hook events (any thread): `idx` = event counter, `ids[idx % 16]` = newest hook id (| [`ORIG_FLAG`] when
/// the trampoline, i.e. the original function called from the detour, was entered). Written by the thunks' machine
/// code (not atomically across threads: diagnostics only).
#[repr(C)]
pub struct HookRing {
    pub idx: AtomicU32,
    pub ids: [AtomicU32; 16],
}
pub static HOOK_RING: HookRing = HookRing { idx: AtomicU32::new(0), ids: [const { AtomicU32::new(0) }; 16] };
/// Set in a ring entry when the event is "original (trampoline) entered".
pub const ORIG_FLAG: u32 = 0x8000_0000;
static NEXT_ID: AtomicU32 = AtomicU32::new(1);

/// One installed inline hook.
#[derive(Debug, Clone, Copy)]
pub struct HookInfo {
    pub id: u32,
    pub target: usize,
    pub detour: usize,
    /// Trampoline (runs the original): `[tramp, tramp + TRAMP_SIZE)`.
    pub tramp: usize,
    /// Entry thunk (records the event, jumps to the detour): `[thunk, thunk + THUNK_SIZE)`.
    pub thunk: usize,
}
pub const TRAMP_SIZE: usize = 128;
pub const THUNK_SIZE: usize = 128;
static HOOKS: Mutex<Vec<HookInfo>> = Mutex::new(Vec::new());

/// Every inline hook installed so far.
pub fn hooks() -> Vec<HookInfo> {
    HOOKS.lock().unwrap_or_else(|e| e.into_inner()).clone()
}

/// Last hook events, newest first (`(id, original entered)`), at most 16.
pub fn recent_events() -> Vec<(u32, bool)> {
    let n = HOOK_RING.idx.load(AOrd::Relaxed);
    (0..n.min(16))
        .map(|k| {
            // the thunk increments `idx` first, then writes `ids[idx % 16]`
            let v = HOOK_RING.ids[(n.wrapping_sub(k) & 15) as usize].load(AOrd::Relaxed);
            (v & !ORIG_FLAG, v & ORIG_FLAG != 0)
        })
        .collect()
}

/// Machine code: record `id` in [`HOOK_RING`] (all registers and flags preserved).
fn marker(id: u32) -> Vec<u8> {
    let ring = &HOOK_RING as *const HookRing as u64;
    let mut c = vec![0x9C, 0x50, 0x51]; // pushfq; push rax; push rcx
    c.extend_from_slice(&[0x48, 0xB8]); // mov rax, imm64
    c.extend_from_slice(&ring.to_le_bytes());
    c.extend_from_slice(&[0x8B, 0x08]); // mov ecx, [rax]
    c.extend_from_slice(&[0xFF, 0xC1]); // inc ecx
    c.extend_from_slice(&[0x89, 0x08]); // mov [rax], ecx
    c.extend_from_slice(&[0x83, 0xE1, 0x0F]); // and ecx, 15
    c.extend_from_slice(&[0xC7, 0x44, 0x88, 0x04]); // mov dword [rax + rcx*4 + 4], imm32
    c.extend_from_slice(&id.to_le_bytes());
    c.extend_from_slice(&[0x59, 0x58, 0x9D]); // pop rcx; pop rax; popfq
    c
}

// ---------------------------------------------------------------- chained hooks (ModLoader plugins)
//
// Every inline hook is a **chain**: the patched jump goes to the entry thunk, whose `jmp [slot]` reads an aligned
// 8-byte slot holding the first (outermost) detour; each detour's `next` cell (its `orig` static) holds the next
// detour, the last one's holds the trampoline (the original function). Adding or removing a detour only rewrites
// those cells atomically (inner cells first, the head last), so the code bytes of the target are patched once.
// Order: key `(priority, rank, seq)` descending runs first. Built-in modules use `(0, 0, seq)`; a plugin uses its own
// priority and `rank = 1 + load index`, so by default plugins wrap the loader's own hooks and a mod loaded later runs
// before one loaded earlier.

/// One detour of a chain.
#[derive(Debug, Clone)]
struct Link {
    detour: usize,
    /// Address of the `AtomicUsize` the detour calls to continue (a static: lives forever).
    next: usize,
    priority: i32,
    rank: u32,
    seq: u64,
    owner: String,
}

#[derive(Debug)]
struct Chain {
    target: usize,
    tramp: usize,
    /// Aligned 8-byte cell read by the thunk's `jmp [slot]`.
    slot: usize,
    links: Vec<Link>,
}

static CHAINS: Mutex<Vec<Chain>> = Mutex::new(Vec::new());
/// Owner of the built-in modules' links (not a valid mod id, so no plugin can remove them).
pub const BUILTIN_OWNER: &str = "<loader>";
static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

/// One detour of a chain, as listed by [`chain_of`] (first = runs first).
#[derive(Debug, Clone, PartialEq)]
pub struct LinkInfo {
    pub detour: usize,
    pub priority: i32,
    pub rank: u32,
    pub owner: String,
}

unsafe fn relink(c: &mut Chain) {
    c.links.sort_by(|a, b| (b.priority, b.rank, b.seq).cmp(&(a.priority, a.rank, a.seq)));
    let n = c.links.len();
    for i in (0..n).rev() {
        let nx = if i + 1 < n { c.links[i + 1].detour } else { c.tramp };
        (*(c.links[i].next as *const std::sync::atomic::AtomicUsize)).store(nx, AOrd::Release);
    }
    let head = c.links.first().map_or(c.tramp, |l| l.detour);
    (*(c.slot as *const std::sync::atomic::AtomicUsize)).store(head, AOrd::Release);
}

/// Chained inline hook. A target hooked for the first time is patched as [`inline_hook`] describes (`expected` = its
/// current first bytes); a target already hooked (by a built-in module or a plugin) only gets `detour` added to its
/// chain (`expected` ignored). `next` must be a static (it is written whenever the chain changes). Returns the value
/// `next` holds now. Err when the same `next` cell or detour is already in the chain.
///
/// # Safety
/// `detour` must match the target's calling convention and continue through `next`; `next` must live forever.
pub unsafe fn chain_hook(
    target: usize,
    expected: &[u8],
    detour: usize,
    next: &std::sync::atomic::AtomicUsize,
    priority: i32,
    rank: u32,
    owner: &str,
) -> Result<usize, String> {
    let mut chains = CHAINS.lock().unwrap_or_else(|e| e.into_inner());
    let link = Link {
        detour,
        next: next as *const _ as usize,
        priority,
        rank,
        seq: SEQ.fetch_add(1, AOrd::Relaxed),
        owner: owner.to_string(),
    };
    if let Some(c) = chains.iter_mut().find(|c| c.target == target) {
        if let Some(l) = c.links.iter().find(|l| l.next == link.next || l.detour == detour) {
            return Err(format!("0x{target:X} is already hooked by {} with this detour / next cell", l.owner));
        }
        c.links.push(link);
        relink(c);
        return Ok(next.load(AOrd::Acquire));
    }
    let (tramp, slot) = install(target, expected, detour, next)?;
    chains.push(Chain { target, tramp, slot, links: vec![link] });
    Ok(tramp)
}

/// Remove every detour of `owner` from every chain (a plugin whose init failed). Returns how many were removed. The
/// target stays patched; with no detour left its thunk jumps straight to the original.
pub fn chain_remove_owner(owner: &str) -> usize {
    let mut chains = CHAINS.lock().unwrap_or_else(|e| e.into_inner());
    let mut n = 0;
    for c in chains.iter_mut() {
        let before = c.links.len();
        c.links.retain(|l| l.owner != owner);
        if c.links.len() != before {
            n += before - c.links.len();
            unsafe { relink(c) };
        }
    }
    n
}

/// The chain of `target`, first detour first (empty = not hooked).
pub fn chain_of(target: usize) -> Vec<LinkInfo> {
    let chains = CHAINS.lock().unwrap_or_else(|e| e.into_inner());
    chains
        .iter()
        .find(|c| c.target == target)
        .map(|c| c.links.iter().map(|l| LinkInfo { detour: l.detour, priority: l.priority, rank: l.rank, owner: l.owner.clone() }).collect())
        .unwrap_or_default()
}

/// Inline hook: overwrite the first `expected.len()` (≥ 14) bytes of `target` with an absolute jump to `detour`
/// (through the event thunk, see the module docs). `expected` must equal the current bytes (whole instructions, no
/// relative operands — taken from the fixed prefix of a unique signature). The trampoline (runs the original
/// function) is stored in `orig` before the patch goes live, and returned.
///
/// Built-in module hooks are chain links of key `(0, 0)` ([`chain_hook`]): when the target is already hooked (e.g.
/// by a plugin), `detour` joins its chain and `orig` receives the next detour instead. `orig` must be a static.
pub unsafe fn inline_hook(
    target: usize,
    expected: &[u8],
    detour: usize,
    orig: &std::sync::atomic::AtomicUsize,
) -> Result<usize, String> {
    chain_hook(target, expected, detour, orig, 0, 0, BUILTIN_OWNER)
}

/// Patch `target` (the first hook of a chain): trampoline + thunk. Returns (trampoline, thunk slot).
unsafe fn install(target: usize, expected: &[u8], detour: usize, orig: &std::sync::atomic::AtomicUsize) -> Result<(usize, usize), String> {
    let n = expected.len();
    if n < 14 {
        return Err("steal length < 14".into());
    }
    let cur = std::slice::from_raw_parts(target as *const u8, n);
    if cur != expected {
        return Err(format!("prologue mismatch at 0x{target:X} (already hooked by something else?)"));
    }
    let id = NEXT_ID.fetch_add(1, AOrd::Relaxed);
    let mem = VirtualAlloc(ptr::null(), TRAMP_SIZE + THUNK_SIZE, MEM_COMMIT | MEM_RESERVE, PAGE_EXECUTE_READWRITE) as usize;
    if mem == 0 {
        return Err("VirtualAlloc failed".into());
    }
    // trampoline: marker(id | ORIG) + stolen bytes + jmp back
    let tramp = mem;
    let mut t = marker(id | ORIG_FLAG);
    t.extend_from_slice(expected);
    t.extend_from_slice(&abs_jmp(target + n));
    if t.len() > TRAMP_SIZE {
        return Err("trampoline too long".into());
    }
    ptr::copy_nonoverlapping(t.as_ptr(), tramp as *mut u8, t.len());
    // thunk: marker(id) + jmp [slot] (slot 8-aligned: the chain head is swapped with one atomic store)
    let thunk = mem + TRAMP_SIZE;
    let mut k = marker(id);
    let jmp_at = k.len();
    let slot_off = (jmp_at + 6 + 7) & !7;
    k.extend_from_slice(&[0xFF, 0x25]);
    k.extend_from_slice(&((slot_off - (jmp_at + 6)) as u32).to_le_bytes());
    k.resize(slot_off, 0xCC);
    k.extend_from_slice(&(detour as u64).to_le_bytes());
    if k.len() > THUNK_SIZE {
        return Err("thunk too long".into());
    }
    let slot = thunk + slot_off;
    ptr::copy_nonoverlapping(k.as_ptr(), thunk as *mut u8, k.len());
    FlushInstructionCache(GetCurrentProcess(), mem as _, TRAMP_SIZE + THUNK_SIZE);
    // Publish the trampoline before the jump goes live.
    orig.store(tramp, std::sync::atomic::Ordering::Release);
    HOOKS.lock().unwrap_or_else(|e| e.into_inner()).push(HookInfo { id, target, detour, tramp, thunk });

    let mut patch = vec![0x90u8; n];
    patch[..14].copy_from_slice(&abs_jmp(thunk));
    if !write_code(target, &patch) {
        return Err("VirtualProtect failed".into());
    }
    Ok((tramp, slot))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    static ORIG: AtomicUsize = AtomicUsize::new(0);

    extern "C" fn detour(a: i32, b: i32) -> i32 {
        let orig: extern "C" fn(i32, i32) -> i32 = unsafe { std::mem::transmute(ORIG.load(Ordering::Acquire)) };
        orig(a, b) * 10
    }

    #[test]
    fn inline_hook_on_dispatch_like_prologue() {
        // Same 15-byte prologue as lua.CommandDispatch, then `lea eax,[rcx+rdx]; ret`.
        let prologue = [0x48, 0x89, 0x5C, 0x24, 0x10, 0x48, 0x89, 0x74, 0x24, 0x18, 0x48, 0x89, 0x7C, 0x24, 0x20];
        let body = [0x8D, 0x04, 0x11, 0xC3];
        unsafe {
            let mem = VirtualAlloc(ptr::null(), 64, MEM_COMMIT | MEM_RESERVE, PAGE_EXECUTE_READWRITE) as usize;
            ptr::copy_nonoverlapping(prologue.as_ptr(), mem as *mut u8, 15);
            ptr::copy_nonoverlapping(body.as_ptr(), (mem + 15) as *mut u8, 4);
            let f: extern "C" fn(i32, i32) -> i32 = std::mem::transmute(mem);
            assert_eq!(f(2, 3), 5);
            assert!(inline_hook(mem, &[0u8; 15], detour as *const () as usize, &ORIG).is_err());
            inline_hook(mem, &prologue, detour as *const () as usize, &ORIG).unwrap();
            assert_eq!(f(2, 3), 50);
            // the thunk and the trampoline recorded their events (ring shared with other tests: search it)
            let h = hooks().into_iter().find(|h| h.target == mem).unwrap();
            let ev = recent_events();
            assert!(ev.contains(&(h.id, false)) && ev.contains(&(h.id, true)), "{ev:?}");
        }
    }

    static C_BUILTIN: AtomicUsize = AtomicUsize::new(0);
    static C_OUTER: AtomicUsize = AtomicUsize::new(0);
    static C_INNER: AtomicUsize = AtomicUsize::new(0);
    type F2 = extern "C" fn(i32, i32) -> i32;
    fn next(c: &AtomicUsize) -> F2 {
        unsafe { std::mem::transmute(c.load(Ordering::Acquire)) }
    }
    /// built-in module: + 100
    extern "C" fn d_builtin(a: i32, b: i32) -> i32 {
        next(&C_BUILTIN)(a, b) + 100
    }
    /// plugin (priority 0, loads later): x 2
    extern "C" fn d_outer(a: i32, b: i32) -> i32 {
        next(&C_OUTER)(a, b) * 2
    }
    /// plugin with priority -5 (inside the built-in): a == 0 answers -1 without calling the original
    extern "C" fn d_inner(a: i32, b: i32) -> i32 {
        if a == 0 {
            return -1;
        }
        next(&C_INNER)(a, b)
    }

    #[test]
    fn chained_hooks_dispatch_in_priority_order() {
        let prologue = [0x48, 0x89, 0x5C, 0x24, 0x10, 0x48, 0x89, 0x74, 0x24, 0x18, 0x48, 0x89, 0x7C, 0x24, 0x20];
        let body = [0x8D, 0x04, 0x11, 0xC3]; // lea eax,[rcx+rdx]; ret
        unsafe {
            let mem = VirtualAlloc(ptr::null(), 64, MEM_COMMIT | MEM_RESERVE, PAGE_EXECUTE_READWRITE) as usize;
            ptr::copy_nonoverlapping(prologue.as_ptr(), mem as *mut u8, 15);
            ptr::copy_nonoverlapping(body.as_ptr(), (mem + 15) as *mut u8, 4);
            let f: F2 = std::mem::transmute(mem);
            // a plugin hooks first (the target is not hooked yet: its prologue is checked)
            assert!(chain_hook(mem, &[0u8; 15], d_outer as *const () as usize, &C_OUTER, 0, 3, "p_outer").is_err());
            chain_hook(mem, &prologue, d_outer as *const () as usize, &C_OUTER, 0, 3, "p_outer").unwrap();
            assert_eq!(f(2, 3), 10);
            // a built-in module hooks the same function later: it joins the chain inside the plugin (rank 0 < 3)
            let nx = inline_hook(mem, &prologue, d_builtin as *const () as usize, &C_BUILTIN).unwrap();
            assert_ne!(nx, d_outer as *const () as usize);
            assert_eq!(f(2, 3), (5 + 100) * 2);
            // a plugin with a negative priority runs after the built-in, right before the original
            chain_hook(mem, &[], d_inner as *const () as usize, &C_INNER, -5, 1, "p_inner").unwrap();
            assert_eq!(f(2, 3), 210);
            assert_eq!(f(0, 3), (-1 + 100) * 2);
            let owners: Vec<String> = chain_of(mem).into_iter().map(|l| l.owner).collect();
            assert_eq!(owners, vec!["p_outer", BUILTIN_OWNER, "p_inner"]);
            // the same next cell / detour twice is refused (it would loop)
            assert!(chain_hook(mem, &[], d_inner as *const () as usize, &C_INNER, 9, 9, "again").is_err());
            // a failed plugin is unlinked: the rest of the chain keeps working
            assert_eq!(chain_remove_owner("p_outer"), 1);
            assert_eq!(f(2, 3), 105);
            assert_eq!(f(0, 3), 99);
            assert_eq!(chain_remove_owner("p_inner"), 1);
            assert_eq!(f(0, 3), 103);
            // (BUILTIN_OWNER is not removed here: other tests' hooks share it)
            // a chain whose only detour is removed: the thunk jumps straight to the original
            let mem2 = VirtualAlloc(ptr::null(), 64, MEM_COMMIT | MEM_RESERVE, PAGE_EXECUTE_READWRITE) as usize;
            ptr::copy_nonoverlapping(prologue.as_ptr(), mem2 as *mut u8, 15);
            ptr::copy_nonoverlapping(body.as_ptr(), (mem2 + 15) as *mut u8, 4);
            let g: F2 = std::mem::transmute(mem2);
            chain_hook(mem2, &prologue, d_solo as *const () as usize, &C_SOLO, 0, 1, "p_solo").unwrap();
            assert_eq!(g(2, 3), 5 - 1000);
            assert_eq!(chain_remove_owner("p_solo"), 1);
            assert_eq!(g(2, 3), 5, "empty chain: the thunk jumps straight to the original");
            assert!(chain_of(mem2).is_empty());
        }
    }

    static C_SOLO: AtomicUsize = AtomicUsize::new(0);
    extern "C" fn d_solo(a: i32, b: i32) -> i32 {
        next(&C_SOLO)(a, b) - 1000
    }

    #[test]
    fn patch_pointer_slot() {
        let mut slot: usize = 7;
        let old = unsafe { patch_ptr(&mut slot, 9) };
        assert_eq!((old, slot), (Some(7), 9));
    }
}

/// One IAT patch of `base`'s import `dll!func` (stores the bound target in `store`, then points the slot at
/// `detour`); refuses when the slot is not bound yet (value inside the image itself). Used by native plugins
/// (`hook_iat`) and the built-in `quit_fix`.
pub fn iat_hook(base: usize, dll: &str, func: &str, detour: usize, store: &std::sync::atomic::AtomicUsize) -> bool {
    use crate::{debug, error, warn};
    let size = unsafe { crate::pe::live::headers(base) }.map_or(0, |h| h.size_of_image as usize);
    let Some(slot) = (unsafe { crate::pe::live::iat_slot(base, dll, func) }) else {
        warn!("iat: {dll}!{func} not imported");
        return false;
    };
    let cur = unsafe { std::ptr::read_volatile(slot) };
    if cur < 0x10000 || (base..base + size).contains(&cur) {
        error!("iat: {dll}!{func} not bound yet (0x{cur:X}); not hooked");
        return false;
    }
    store.store(cur, AOrd::Release);
    match unsafe { patch_ptr(slot, detour) } {
        Some(_) => {
            debug!("iat: {dll}!{func} hooked");
            true
        }
        None => {
            error!("iat: {dll}!{func} VirtualProtect failed");
            false
        }
    }
}
