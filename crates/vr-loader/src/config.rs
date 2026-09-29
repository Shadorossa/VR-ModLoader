//! `evt_loader\config.toml` (created with defaults on first run).
//!
//! Sections of other (older / private) builds are ignored, so an existing `config.toml` keeps working.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Config {
    pub loader: LoaderCfg,
    pub modules: ModulesCfg,
    pub debug: crate::debug::DebugCfg,
    pub lua_patch: crate::lua_patch::LuaPatchCfg,
    pub quit_fix: crate::quit_fix::QuitFixCfg,
    pub console: crate::console::ConsoleCfg,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct LoaderCfg {
    /// error | warn | info | debug | trace
    pub log_level: String,
    /// Also send log lines to OutputDebugString (DebugView).
    pub debug_output: bool,
    /// Skip the SHA-1 check of nie.exe (the PE header check still applies). Only for testing.
    pub skip_hash_check: bool,
}

/// `[modules]`: the built-in modules. A mod can switch one on with `loader_modules` in its `mod.toml`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct ModulesCfg {
    /// Lua command bridge `CMND_EVT_*` (needed by every Lua command, the plugins' `lua_register` included).
    pub lua_bridge: bool,
    /// Lua error capture (`LUAERR` lines in loader.log), script prints, local debug server, hang watchdog (`[debug]`).
    pub debug: bool,
    /// Plain-text Lua patch files `evt_loader\lua_patches\<script>\*.lua`, run in the script's own VM right after
    /// its main chunk. Mods' `lua\` folders use the same runner whether this is on or not.
    pub lua_patch: bool,
    /// Mod folders `<game>\mods\<id>\` (mod.toml; lua/, files/data/, data/ deltas, plugins). On by default.
    pub mods: bool,
    /// Every character counts as legal (no yellow placeholder face for characters added by mods). On by default.
    #[serde(alias = "uvr_compat")]
    pub chara_legal: bool,
    /// Closing the window (X / Alt+F4) always ends the process, also in a match: watcher of the retail quit gate +
    /// `ExitProcess` -> `TerminateProcess` (`[quit_fix]`). On by default.
    pub quit_fix: bool,
    /// ModLoader console: a separate console window mirroring loader.log live with colours and a command line, plus
    /// the switchable categories of game logging (`[console]`). Off by default.
    pub console: bool,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            loader: LoaderCfg::default(),
            modules: ModulesCfg::default(),
            debug: crate::debug::DebugCfg::default(),
            lua_patch: crate::lua_patch::LuaPatchCfg::default(),
            quit_fix: crate::quit_fix::QuitFixCfg::default(),
            console: crate::console::ConsoleCfg::default(),
        }
    }
}
impl Default for LoaderCfg {
    fn default() -> Self {
        LoaderCfg { log_level: "info".into(), debug_output: false, skip_hash_check: false }
    }
}
impl Default for ModulesCfg {
    fn default() -> Self {
        ModulesCfg { lua_bridge: true, debug: false, lua_patch: false, mods: true, chara_legal: true, quit_fix: true, console: false }
    }
}

pub const DEFAULT_TOML: &str = r#"# VR-ModLoader configuration (winmm.dll proxy).
# Changes take effect on the next game start.

[loader]
log_level = "info"        # error | warn | info | debug | trace
debug_output = false      # also OutputDebugString (DebugView)
skip_hash_check = false   # testing only: skip the SHA-1 check of nie.exe (v7.1.2 header check still applies)

[modules]
lua_bridge = true         # CMND_EVT_* Lua commands (needed by mods' Lua and by plugins that register commands)
debug = false             # LUAERR lines for every Lua error, script prints, local debug server, hang watchdog ([debug])
lua_patch = false         # plain-text Lua patches evt_loader\lua_patches\<script>\*.lua (mods' lua\ folders run anyway)
mods = true               # mod folders <game>\mods\<id>\ (mod.toml, lua\, files\data\, data\ deltas, plugins; enabled.toml)
chara_legal = true        # every character is legal: characters added by mods get no yellow placeholder face
quit_fix = true           # closing the window (X / Alt+F4) always ends nie.exe, also in a match; never during a save job ([quit_fix])
console = false           # ModLoader console: separate window with loader.log live (colours) + command line (help) ([console])

[debug]
port = 47017              # local TCP port of the debug server (protocol: crates/vr-loader/src/debug/proto.rs)
bind = "127.0.0.1"        # loopback only: any other address is refused
server = true             # false = Lua error capture only
lua_errors = true         # LUAERR lines in loader.log for every Lua load / run error (the engine hides them)
traceback = true          # stack traceback for errors of the engine's calls into Lua
capture_print = true      # print / PRINT / WARNING / ASSERT of scripts -> LUAPRINT lines
chunk_names = true        # name chunks after their loose file (CRC-32 match in index_dirs) in messages
index_dirs = ["data"]     # folders (relative to the game folder, or absolute) scanned for *.lua.bin / *.lua
errors_per_10s = 20       # LUAERR rate limit (identical consecutive errors are folded)
prints_per_10s = 100      # LUAPRINT rate limit
allow_write = false       # server command `write` (patch game memory)
allow_watch = false       # server command `watch` (temporary logging hooks)
lua_timeout_ms = 5000     # how long `lua` waits for the game thread

[lua_patch]               # with [modules] lua_patch = true or mods with lua\ (sdk/README.md, Lua patches)
match = "fingerprint"     # "fingerprint": the script's folder is chosen by the globals its chunk defined (_fingerprints.json); "name": by the last .lua.bin path opened (pre-2026-09-27)
prefilter = true          # fingerprint: one cheap probe first (OnOpenLayer / OnSetupLayer / OnInit / Step / OnFunction); false = probe every stem in every VM

[quit_fix]                # with [modules] quit_fix = true
grace_seconds = 5         # seconds the retail quit gets after the close request (and the retail shutdown after g_quit)
retail_quit = true        # gate still closed after the grace: set g_quit (the game's own shutdown runs); false = TerminateProcess directly
save_wait_seconds = 60    # a save job still running after the grace is waited for up to this long (never cut while it moves)
exit_terminate = true     # after a close request, nie.exe's ExitProcess becomes TerminateProcess (no DLL detach can hang)

[console]                 # with [modules] console = true
categories = ["loader"]   # on at start: loader, states, files, menus, sound, match (+ plugin categories); console: log <cat> on|off
colors = true             # colours per level / category (ANSI)
input = true              # command line in the console window (help, log, config, status, lua reload, quit, close)
file_hits = true          # files: also files opened fine (false = only missing / failed)
rate_limit = 200          # lines per second and category written by the taps (the rest is counted); 0 = no limit
block_close = true        # the console's X / Alt+F4 are disabled (command close closes it safely); false = X detaches the console, the game goes on
"#;

impl Config {
    pub fn parse(text: &str) -> Result<Config, String> {
        toml::from_str(text).map_err(|e| e.to_string())
    }

    /// One log line per `[modules]` flag: "module X: enabled" or "module X: disabled (no hooks)".
    pub fn module_gate_report(&self) -> Vec<String> {
        let v = serde_json::to_value(&self.modules).unwrap_or_default();
        let Some(obj) = v.as_object() else { return vec![] };
        obj.iter()
            .map(|(k, b)| if b.as_bool().unwrap_or(false) { format!("module {k}: enabled") } else { format!("module {k}: disabled (no hooks)") })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn default_text_matches_default_struct() {
        assert_eq!(Config::parse(DEFAULT_TOML).unwrap(), Config::default());
    }

    #[test]
    fn gate_report_names_every_module() {
        let r = Config::default().module_gate_report();
        assert!(r.iter().any(|l| l == "module mods: enabled"), "{r:?}");
        assert!(r.iter().any(|l| l == "module console: disabled (no hooks)"), "{r:?}");
        assert_eq!(r.len(), serde_json::to_value(&Config::default().modules).unwrap().as_object().unwrap().len());
    }

    #[test]
    fn unknown_sections_and_old_names_are_accepted() {
        let c = Config::parse("[modules]\nuvr_compat = false\nboard = true\n[stats]\nhero_mult = 2.0\n").unwrap();
        assert!(!c.modules.chara_legal && c.modules.mods && c.modules.lua_bridge);
    }

    #[test]
    fn lua_patch_section() {
        use crate::lua_patch::MatchMode;
        let d = Config::default();
        assert!(!d.modules.lua_patch && d.lua_patch.prefilter && d.lua_patch.mode() == MatchMode::Fingerprint);
        let c = Config::parse("[modules]\nlua_patch = true\n[lua_patch]\nmatch = \"name\"\nprefilter = false\n").unwrap();
        assert!(c.modules.lua_patch && !c.lua_patch.prefilter && c.lua_patch.mode() == MatchMode::Name);
        assert_eq!(c.lua_patch.match_, "name");
        assert_eq!(Config::parse("[modules]\nlua_patch = true\n").unwrap().lua_patch, crate::lua_patch::LuaPatchCfg::default());
    }

    #[test]
    fn debug_section() {
        let d = Config::default();
        assert!(!d.modules.debug);
        assert!(d.debug.lua_errors && !d.debug.allow_write && !d.debug.allow_watch);
        let c = Config::parse("[modules]\ndebug = true\n[debug]\nport = 5000\nserver = false\n").unwrap();
        assert!(c.modules.debug);
        assert_eq!((c.debug.port, c.debug.server, c.debug.lua_errors), (5000, false, true));
    }
}
