//! Module `debug`: in-game debugger (docs/game/engine/debugger.md). Off by default (`[modules] debug = false`).
//!
//! * **Lua error capture** ([`luaerr`], works without the server): hooks `luaL_loadbufferx` and `lua_pcallk`, the two
//!   places every chunk load and protected call goes through. The engine pops and ignores every error; the hooks
//!   write `LUAERR ...` lines (message, chunk/file, traceback) to loader.log, rate-limited. Also `LUAPRINT` lines for
//!   the scripts' `print`/`PRINT`/`WARNING`/`ASSERT` (empty stubs in the retail build).
//! * **Debug server** (planned, not in this build): local TCP (127.0.0.1 only), one request per line (text or JSON), JSON answers:
//!   `ping`, `info`, `read`, `write`, `lua` (run on the game thread from the Lua dispatcher hook), `vms`, `log`,
//!   `luaerr`, `watch`/`unwatch`/`watches` (temporary logpoints), `units`, `chara`, `help`.
//! * Pure helpers (unit-tested on any host): [`x64len`], [`addr`], [`proto`], [`ratelimit`], [`stubgen`], [`sigs`].
//!
//! Client: `tools/py/evtdbg.py`; app: "Depurador" card of the Proyecto page.

pub mod addr;
pub mod proto;
pub mod ratelimit;
pub mod sigs;
pub mod stubgen;
pub mod x64len;

#[cfg(all(windows, target_arch = "x86_64"))]
pub mod luaerr;
/// Hang watchdog (`[debug] watchdog`): HANG lines with the main thread's stack when it stops responding.
#[cfg(all(windows, target_arch = "x86_64"))]
pub mod watchdog;

use serde::{Deserialize, Serialize};

pub const DEFAULT_PORT: u16 = 47017;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct DebugCfg {
    /// TCP port of the debug server.
    pub port: u16,
    /// Listen address; only loopback addresses are accepted (anything else falls back to 127.0.0.1).
    pub bind: String,
    /// Start the TCP server (false = Lua error capture only).
    pub server: bool,
    /// LUAERR lines for every Lua load / run error.
    pub lua_errors: bool,
    /// Traceback message handler for the engine's protected calls.
    pub traceback: bool,
    /// Scripts' `print` / `PRINT` / `WARNING` / `ASSERT` -> LUAPRINT lines.
    pub capture_print: bool,
    /// Give nameless chunks the name of their file (matched by CRC-32 in `index_dirs`), so messages say where.
    pub chunk_names: bool,
    /// Folders (relative to the game folder, or absolute) scanned for `*.lua.bin` / `*.lua` to name chunks.
    pub index_dirs: Vec<String>,
    /// LUAERR lines per 10 s (identical consecutive errors are folded).
    pub errors_per_10s: u32,
    /// LUAPRINT lines per 10 s.
    pub prints_per_10s: u32,
    /// Server command `write`.
    pub allow_write: bool,
    /// Server command `watch`.
    pub allow_watch: bool,
    /// How long `lua` / game-thread helpers wait for the game thread.
    pub lua_timeout_ms: u64,
    /// Hang watchdog: HANG lines (main thread stack, last hooks, Lua frame) when the main thread stalls.
    pub watchdog: bool,
    /// Heartbeat age (ms) after which the main thread counts as stalled (its window must not answer either).
    pub watchdog_ms: u64,
}

impl Default for DebugCfg {
    fn default() -> Self {
        DebugCfg {
            port: DEFAULT_PORT,
            bind: "127.0.0.1".into(),
            server: true,
            lua_errors: true,
            traceback: true,
            capture_print: true,
            chunk_names: true,
            index_dirs: vec!["data".into()],
            errors_per_10s: 20,
            prints_per_10s: 100,
            allow_write: false,
            allow_watch: false,
            lua_timeout_ms: 5000,
            watchdog: true,
            watchdog_ms: 3000,
        }
    }
}

// ---------------------------------------------------------------- run time glue (called by runtime.rs)

#[cfg(all(windows, target_arch = "x86_64"))]
mod rt {
    use crate::game::Text;
    use crate::{info, warn};
    use std::sync::atomic::{AtomicBool, Ordering};

    static HOOKED: AtomicBool = AtomicBool::new(false);

    /// DllMain: install the Lua error hooks (pass-through until [`activate`]). No game thread runs yet.
    pub fn install_hooks(text: &Text, cfg: &super::DebugCfg) -> bool {
        if !(cfg.lua_errors || cfg.capture_print) {
            return false;
        }
        let ok = super::luaerr::install_hooks(text);
        HOOKED.store(ok, Ordering::Release);
        ok
    }

    /// Init thread (after the v7.1.2 gate). Returns true when the module is active.
    pub fn activate(text: &Text, game_dir: &std::path::Path, cfg: &super::DebugCfg) -> bool {
        if cfg.watchdog {
            super::watchdog::start(cfg.watchdog_ms);
        } else {
            info!("debug: hang watchdog off ([debug] watchdog = false)");
        }
        if !(cfg.lua_errors || cfg.capture_print) {
            info!("debug: [debug] lua_errors and capture_print are off: nothing to do");
            return false;
        }
        if !HOOKED.load(Ordering::Acquire) {
            warn!("debug: Lua hooks not installed (see errors above): no LUAERR lines");
            return false;
        }
        let ok = super::luaerr::activate(text, game_dir, cfg);
        if ok {
            info!(
                "debug: active (LUAERR {}, traceback {}, LUAPRINT {}, chunk names {}; server: not in this build)",
                cfg.lua_errors, cfg.traceback, cfg.capture_print, cfg.chunk_names
            );
        }
        ok
    }
}

#[cfg(all(windows, target_arch = "x86_64"))]
pub use rt::{activate, install_hooks};

#[cfg(test)]
mod tests {
    #[test]
    fn default_cfg() {
        let c = super::DebugCfg::default();
        assert_eq!(c.port, 47017);
        assert!(super::proto::loopback_addr(&c.bind).is_some());
        assert_eq!(c.index_dirs, vec!["data".to_string()]);
    }
}
