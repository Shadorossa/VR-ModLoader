//! Addresses resolved in the running nie.exe (signature scan of `.text`, unique match required).

use crate::scan::Pattern;
use crate::sigs::{self, RipRef, Sig};
use crate::{error, info, warn};
use std::collections::HashMap;

/// Addresses resolved once (before any module patches a prologue), keyed by pattern (`crate::registry`).
static SIG_CACHE: std::sync::Mutex<Option<HashMap<(&'static str, u32), Option<usize>>>> = std::sync::Mutex::new(None);
static RIP_CACHE: std::sync::Mutex<Option<HashMap<(&'static str, u32, u32), Option<usize>>>> = std::sync::Mutex::new(None);

/// Copy of nie.exe `.text` taken in DllMain before the first patch ([`Text::prime_all`]).
static CLEAN_TEXT: std::sync::OnceLock<Vec<u8>> = std::sync::OnceLock::new();

/// The unpatched `.text` copy (None before [`Text::prime_all`] ran).
pub fn clean_text() -> Option<&'static [u8]> {
    CLEAN_TEXT.get().map(|v| v.as_slice())
}

pub struct Text {
    pub base: usize,
    pub text_va: usize,
    pub text: &'static [u8],
}

impl Text {
    pub fn current() -> Option<Text> {
        let base = crate::pe::live::exe_base();
        let (text_va, text) = unsafe { crate::pe::live::text(base)? };
        Some(Text { base, text_va, text })
    }

    /// Resolve every signature and RIP reference of every module (`crate::registry`) on the still unpatched
    /// `.text`. Called in DllMain before the first hook; later lookups hit the cache and never re-scan code that a
    /// module has patched since.
    pub fn prime_all(&self) {
        // copy of the still unpatched .text: plugins resolve their signatures on it later (sig_find, rip_target)
        let _ = CLEAN_TEXT.set(self.text.to_vec());
        let sigs = crate::registry::all_sigs();
        let rips = crate::registry::all_rips();
        let ok = sigs.iter().filter(|s| self.resolve(s).is_some()).count();
        let rok = rips.iter().filter(|r| self.resolve_rip(r).is_some()).count();
        info!("signatures primed before patching: {ok}/{} code, {rok}/{} references", sigs.len(), rips.len());
    }

    /// Address of a signature's function (unique match), logged on first resolution, then cached.
    pub fn resolve(&self, s: &Sig) -> Option<usize> {
        let key = crate::registry::sig_key(s);
        if let Some(v) = SIG_CACHE.lock().unwrap_or_else(|e| e.into_inner()).as_ref().and_then(|m| m.get(&key).copied()) {
            return v;
        }
        let v = self.scan(s);
        SIG_CACHE.lock().unwrap_or_else(|e| e.into_inner()).get_or_insert_with(HashMap::new).insert(key, v);
        v
    }

    fn scan(&self, s: &Sig) -> Option<usize> {
        let pat = match Pattern::parse(s.pattern) {
            Ok(p) => p,
            Err(e) => {
                error!("sig {}: bad pattern: {e}", s.name);
                return None;
            }
        };
        match pat.find_unique(self.text) {
            Ok(off) => {
                let addr = self.text_va + off - s.offset as usize;
                let rva = (addr - self.base) as u32;
                if rva == s.rva {
                    info!("sig {:<28} -> RVA 0x{rva:X}", s.name);
                } else {
                    warn!("sig {:<28} -> RVA 0x{rva:X} (expected 0x{:X})", s.name, s.rva);
                }
                Some(addr)
            }
            Err(e) => {
                error!("sig {}: {e}; feature disabled", s.name);
                None
            }
        }
    }

    /// Target of a RIP-relative operand inside a signature.
    pub fn resolve_rip(&self, r: &RipRef) -> Option<usize> {
        let key = crate::registry::rip_key(r);
        if let Some(v) = RIP_CACHE.lock().unwrap_or_else(|e| e.into_inner()).as_ref().and_then(|m| m.get(&key).copied()) {
            return v;
        }
        let v = self.scan_rip(r);
        RIP_CACHE.lock().unwrap_or_else(|e| e.into_inner()).get_or_insert_with(HashMap::new).insert(key, v);
        v
    }

    fn scan_rip(&self, r: &RipRef) -> Option<usize> {
        let m = self.resolve(r.sig)? + r.sig.offset as usize;
        let disp = unsafe { std::ptr::read_unaligned((m + r.disp_off as usize) as *const i32) };
        let addr = (m as isize + r.next_ip_off as isize + disp as isize) as usize;
        let rva = addr.wrapping_sub(self.base) as u32;
        if rva == r.rva {
            info!("ref {:<28} -> RVA 0x{rva:X}", r.name);
        } else {
            warn!("ref {:<28} -> RVA 0x{rva:X} (expected 0x{:X})", r.name, r.rva);
        }
        Some(addr)
    }
}

/// Lua C API functions of the game's Lua 5.2.
#[derive(Clone, Copy)]
pub struct LuaApi {
    pub gettop: unsafe extern "C" fn(*mut u8) -> i32,
    pub tonumberx: unsafe extern "C" fn(*mut u8, i32, *mut i32) -> f64,
    pub tolstring: unsafe extern "C" fn(*mut u8, i32, *mut usize) -> *const u8,
    pub type_: unsafe extern "C" fn(*mut u8, i32) -> i32,
    pub pushnumber: unsafe extern "C" fn(*mut u8, f64),
    pub pushboolean: unsafe extern "C" fn(*mut u8, i32),
    pub pushstring: unsafe extern "C" fn(*mut u8, *const u8) -> *const u8,
}

impl LuaApi {
    pub fn resolve(t: &Text) -> Option<LuaApi> {
        unsafe {
            Some(LuaApi {
                gettop: std::mem::transmute::<usize, _>(t.resolve(&sigs::LUA_GETTOP)?),
                tonumberx: std::mem::transmute::<usize, _>(t.resolve(&sigs::LUA_TONUMBERX)?),
                tolstring: std::mem::transmute::<usize, _>(t.resolve(&sigs::LUA_TOLSTRING)?),
                type_: std::mem::transmute::<usize, _>(t.resolve(&sigs::LUA_TYPE)?),
                pushnumber: std::mem::transmute::<usize, _>(t.resolve(&sigs::LUA_PUSHNUMBER)?),
                pushboolean: std::mem::transmute::<usize, _>(t.resolve(&sigs::LUA_PUSHBOOLEAN)?),
                pushstring: std::mem::transmute::<usize, _>(t.resolve(&sigs::LUA_PUSHSTRING)?),
            })
        }
    }
}

extern "C" {
    fn evt_seh_call5(f: usize, a: u64, b: u64, c: u64, d: u64, e: u64, out: *mut u64) -> i32;
    fn evt_seh_call6(f: usize, a: u64, b: u64, c: u64, d: u64, e: u64, g: u64, out: *mut u64) -> i32;
    fn evt_seh_read(src: usize, dst: *mut u8, n: usize) -> i32;
    fn evt_seh_write(dst: usize, src: *const u8, n: usize) -> i32;
}

/// Call engine code `f(a..f)` with 6 integer args (5th and 6th on the stack) under `__try/__except`.
pub fn seh_call6(func: usize, a: u64, b: u64, c: u64, d: u64, e: u64, f: u64) -> Result<u64, u32> {
    let mut out = 0u64;
    let r = unsafe { evt_seh_call6(func, a, b, c, d, e, f, &mut out) };
    if r == 0 {
        Ok(out)
    } else {
        Err(r as u32)
    }
}

/// Call engine code `f(a..e)` (integer args, Microsoft x64) under `__try/__except`.
/// Err = exception code.
pub fn seh_call(f: usize, a: u64, b: u64, c: u64, d: u64, e: u64) -> Result<u64, u32> {
    let mut out = 0u64;
    let r = unsafe { evt_seh_call5(f, a, b, c, d, e, &mut out) };
    if r == 0 {
        Ok(out)
    } else {
        Err(r as u32)
    }
}

/// Guarded read of game memory.
pub fn read<T: Copy>(addr: usize) -> Option<T> {
    if addr < 0x10000 {
        return None;
    }
    let mut v = std::mem::MaybeUninit::<T>::uninit();
    let r = unsafe { evt_seh_read(addr, v.as_mut_ptr() as *mut u8, std::mem::size_of::<T>()) };
    if r == 0 {
        Some(unsafe { v.assume_init() })
    } else {
        None
    }
}

/// Guarded read of a non-null pointer.
pub fn read_ptr(addr: usize) -> Option<usize> {
    read::<usize>(addr).filter(|&p| p >= 0x10000)
}

/// Guarded write of game memory (data only, never code).
pub fn write<T: Copy>(addr: usize, v: T) -> bool {
    if addr < 0x10000 {
        return false;
    }
    unsafe { evt_seh_write(addr, &v as *const T as *const u8, std::mem::size_of::<T>()) == 0 }
}
