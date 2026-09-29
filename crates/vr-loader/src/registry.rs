//! Every signature, RIP reference and inline hook of every module in one place (pure, unit-tested).
//!
//! Modules patch function prologues, so a signature scanned **after** another module's patch can stop matching
//! (a module that calls a function another module hooks would fail its late scan). The loader therefore resolves everything listed here once, in DllMain, before the
//! first patch (`crate::game::Text::prime_all`), and every later `resolve` / `resolve_rip` reads that cache.
//! `tests/all_modules.rs` checks, on the dump, that all of them resolve on the clean `.text`, that no function is
//! hooked twice, and that the cached values survive every module's patches.

use crate::scan::Pattern;
use crate::sigs::{RipRef, Sig};
use std::collections::HashMap;

/// Every code signature of the loader and its modules (duplicates removed by pattern).
pub fn all_sigs() -> Vec<&'static Sig> {
    let lists: [&[&'static Sig]; 7] = [
        crate::sigs::ALL,
        crate::lua_patch::sigs::ALL,
        crate::mods::sigs::ALL,
        crate::debug::sigs::ALL,
        crate::chara_legal::sigs::ALL,
        crate::quit_fix::sigs::ALL,
        crate::console::sigs::ALL,
    ];
    let mut out: Vec<&'static Sig> = Vec::new();
    for l in lists {
        for s in l {
            if !out.iter().any(|o| o.pattern == s.pattern && o.offset == s.offset) {
                out.push(*s);
            }
        }
    }
    out
}

/// Every RIP-relative reference.
pub fn all_rips() -> Vec<&'static RipRef> {
    crate::sigs::ALL_RIP.iter().copied().chain(crate::quit_fix::sigs::ALL_RIP.iter().copied()).collect()
}

/// Every inline hook (target signature, stolen bytes) any module may install.
pub fn all_hooks() -> Vec<(&'static Sig, usize)> {
    let mut v: Vec<(&'static Sig, usize)> = Vec::new();
    v.extend_from_slice(crate::sigs::INLINE_HOOKS);
    v.extend_from_slice(crate::debug::sigs::INLINE_HOOKS);
    v.extend_from_slice(crate::lua_patch::sigs::HOOKS);
    v.extend_from_slice(crate::mods::sigs::HOOKS);
    v.extend_from_slice(crate::console::sigs::HOOKS);
    v
}

/// Cache key of a signature / RIP reference (by pattern, not name: modules share patterns).
pub fn sig_key(s: &Sig) -> (&'static str, u32) {
    (s.pattern, s.offset)
}
pub fn rip_key(r: &RipRef) -> (&'static str, u32, u32) {
    (r.sig.pattern, r.sig.offset, r.disp_off)
}

/// Resolution of everything on one `.text` image: signature → text offset of the function, RIP reference → target
/// RVA. Errors are kept (a module whose signature fails stays off, as before).
#[derive(Debug, Default)]
pub struct Resolved {
    pub sigs: HashMap<(&'static str, u32), Result<usize, String>>,
    pub rips: HashMap<(&'static str, u32, u32), Result<u32, String>>,
}

pub fn resolve_all(text: &[u8], text_rva: u32) -> Resolved {
    let mut r = Resolved::default();
    for s in all_sigs() {
        r.sigs.entry(sig_key(s)).or_insert_with(|| {
            let p = Pattern::parse(s.pattern)?;
            p.find_unique(text).map(|off| off - s.offset as usize)
        });
    }
    for rr in all_rips() {
        let off = match r.sigs.get(&sig_key(rr.sig)).cloned().unwrap_or_else(|| {
            Pattern::parse(rr.sig.pattern)?.find_unique(text).map(|o| o - rr.sig.offset as usize)
        }) {
            Ok(o) => o,
            Err(e) => {
                r.rips.insert(rip_key(rr), Err(e));
                continue;
            }
        };
        let m = off + rr.sig.offset as usize;
        let d = m + rr.disp_off as usize;
        let res = text
            .get(d..d + 4)
            .map(|b| i32::from_le_bytes(b.try_into().unwrap()))
            .map(|disp| (text_rva as i64 + m as i64 + rr.next_ip_off as i64 + disp as i64) as u32)
            .ok_or_else(|| "displacement outside .text".to_string());
        r.rips.insert(rip_key(rr), res);
    }
    r
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_is_complete_and_hooks_are_unique() {
        let sigs = all_sigs();
        // every module list is in (spot checks) and duplicates are removed
        for n in [
            "lua.CommandDispatch",
            "chara_legal.CharaIllegalTest",
            "console.OpenMenu",
            "lua_patch.ScriptObject_LoadChunk",
            "lua_patch.CCriFileOperate_Open",
            "mods.ResolveOverlayPath",
        ] {
            assert!(sigs.iter().any(|s| s.name == n), "{n}");
        }
        let mut pats: Vec<_> = sigs.iter().map(|s| sig_key(s)).collect();
        pats.sort();
        pats.dedup();
        assert_eq!(pats.len(), sigs.len());
        // no function is hooked by two modules (each hook would find the other's jump)
        let hooks = all_hooks();
        let mut rvas: Vec<u32> = hooks.iter().map(|(s, _)| s.rva - s.offset).collect();
        rvas.sort();
        let n = rvas.len();
        rvas.dedup();
        assert_eq!(rvas.len(), n, "a function is hooked twice");
        // every hook target is a registered signature
        for (s, steal) in &hooks {
            assert!(sigs.iter().any(|x| sig_key(x) == sig_key(s)), "{} not registered", s.name);
            assert!(*steal >= 14, "{}", s.name);
        }
        assert!(!all_rips().is_empty());
    }
}
