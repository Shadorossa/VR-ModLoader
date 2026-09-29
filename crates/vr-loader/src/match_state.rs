//! One answer to «is a real match running?» for every module and plugin (plugin API `game_state`).
//!
//! A real match = the soccer-mode flag `[g_sceneSoccer]+0x222D` (the byte `CMND_IS_SOCCER_MODE` reads, set for the
//! whole match including its pause menus), or a match build seen less than [`BUILD_WINDOW_MS`] ago (match loading,
//! before kick-off, reported through [`note_match_build`]). NOT «the soccer scene and the actor array are allocated»:
//! both stay allocated in the menus once a save is loaded. The build window is closed when the flag
//! goes from set to clear (back from a match), so a late build never leaks into the menus.

/// How long after a match build a request still counts as "in a match" while the soccer-mode flag is not set yet.
pub const BUILD_WINDOW_MS: u64 = 30_000;

/// The pure rule: flag set, or a match build less than [`BUILD_WINDOW_MS`] ago.
pub fn real_match(soccer_mode: Option<bool>, ms_since_build: Option<u64>) -> bool {
    soccer_mode == Some(true) || ms_since_build.is_some_and(|d| d < BUILD_WINDOW_MS)
}

/// Scene byte `+0x222D`: soccer mode.
pub const SCENE_SOCCER_MODE: usize = 0x222D;

#[cfg(all(windows, target_arch = "x86_64"))]
mod rt {
    use super::*;
    use crate::game::{read, read_ptr};
    use crate::info;
    use std::sync::atomic::{AtomicU64, AtomicU8, AtomicUsize, Ordering};

    static EPOCH: std::sync::OnceLock<std::time::Instant> = std::sync::OnceLock::new();
    /// Milliseconds since the first call (+1, so 0 means "never").
    fn now_ms() -> u64 {
        EPOCH.get_or_init(std::time::Instant::now).elapsed().as_millis() as u64 + 1
    }

    /// Address of the global `g_sceneSoccer` pointer (`crate::sigs::G_SCENE_SOCCER`, resolved by the runtime at init).
    static G_SCENE: AtomicUsize = AtomicUsize::new(0);
    static LAST_BUILD_MS: AtomicU64 = AtomicU64::new(0);
    /// 0 menus, 1 match, 2 unknown (logged on change)
    static STATE: AtomicU8 = AtomicU8::new(2);
    /// last flag value seen (0 clear, 1 set, 2 unknown): closes the build window on set -> clear
    static FLAG: AtomicU8 = AtomicU8::new(2);

    /// Give the address of the `g_sceneSoccer` global (`crate::sigs::G_SCENE_SOCCER`, RIP-resolved). Idempotent.
    pub fn set_scene_global(addr: Option<usize>) {
        if let Some(a) = addr.filter(|&a| a != 0) {
            let _ = G_SCENE.compare_exchange(0, a, Ordering::AcqRel, Ordering::Acquire);
        }
    }

    /// A match build was seen (a module or plugin that hooks the match build, before kick-off).
    pub fn note_match_build() {
        LAST_BUILD_MS.store(now_ms(), Ordering::Release);
    }

    /// The soccer-mode flag (None = no scene / unreadable).
    pub fn soccer_mode() -> Option<bool> {
        let g = G_SCENE.load(Ordering::Acquire);
        if g == 0 {
            return None;
        }
        read::<u8>(read_ptr(g)? + SCENE_SOCCER_MODE).map(|b| b != 0)
    }

    /// Is a real match running ([`real_match`])? Logs every change.
    pub fn in_match() -> bool {
        let flag = soccer_mode();
        let f = match flag {
            Some(true) => 1,
            Some(false) => 0,
            None => 2,
        };
        if FLAG.swap(f, Ordering::AcqRel) == 1 && f != 1 {
            LAST_BUILD_MS.store(0, Ordering::Release); // back from a match
        }
        let b = LAST_BUILD_MS.load(Ordering::Acquire);
        let since = (b != 0).then(|| now_ms().saturating_sub(b));
        let on = real_match(flag, since);
        if STATE.swap(on as u8, Ordering::AcqRel) != on as u8 {
            info!(
                "match_state: {} (soccer mode flag {flag:?}, last match build {})",
                if on { "IN A MATCH" } else { "menus" },
                since.map_or("never".to_string(), |d| format!("{} ms ago", d))
            );
        }
        on
    }
}

#[cfg(all(windows, target_arch = "x86_64"))]
pub use rt::{in_match, note_match_build, set_scene_global, soccer_mode};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rule() {
        // menus: flag clear, no build seen (or long ago), whatever the scene pointers
        assert!(!real_match(Some(false), None));
        assert!(!real_match(Some(false), Some(BUILD_WINDOW_MS + 1)));
        assert!(!real_match(None, None), "unreadable flag, no build: menus");
        assert!(real_match(Some(true), None), "soccer mode (match, pause menu)");
        assert!(real_match(Some(false), Some(2_000)), "match loading right after a build");
        assert!(real_match(None, Some(0)));
    }
}
