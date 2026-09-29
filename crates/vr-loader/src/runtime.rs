//! DllMain + init thread.
//!
//! DllMain (loader lock held) only does lock-safe work: host/version quick check (PE header), read `config.toml`
//! and the mod plan, resolve every signature on the clean `.text` (`crate::registry`), then the inline hooks and
//! byte patches that must be in place before the game runs any code (Lua dispatcher, Lua error capture, the file
//! overlay of `mods`, the Lua patch runner, the console taps, `chara_legal`), and the plugins' early phase; then it
//! spawns the init thread. The init thread (runs once the loader lock is released) opens the log, verifies the exe
//! SHA-1, loads the native plugins, activates the built-in modules and the Lua commands. Until then every hook is a
//! pure pass-through.

use crate::config::{Config, DEFAULT_TOML};
use crate::log::Level;
use crate::lua::{self, Call};
use crate::{error, gate, info, warn};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Condvar, Mutex, OnceLock};
use std::time::Duration;
use windows_sys::Win32::System::LibraryLoader::{DisableThreadLibraryCalls, GetModuleFileNameW};

pub struct Ctx {
    pub exe: PathBuf,
    pub game_dir: PathBuf,
    pub data_dir: PathBuf,
    pub cfg: Config,
    pub cfg_error: Option<String>,
}

static CTX: OnceLock<Ctx> = OnceLock::new();
static ENABLED: AtomicBool = AtomicBool::new(false);
static DONE: Mutex<bool> = Mutex::new(false);
static DONE_CV: Condvar = Condvar::new();
/// Active built-in modules (reported by `CMND_EVT_LOADER_VERSION` and the plugin API `loader.modules_mask`). Bits not
/// listed here are reserved (they were used by modules that are now mods).
pub static MODULES: AtomicU32 = AtomicU32::new(0);
/// Module `lua_patch` (bit 9): plain-text Lua patch files run after each script's main chunk.
pub const MOD_LUA_PATCH: u32 = 1 << 9;
static LUA_PATCH_HOOKED: AtomicBool = AtomicBool::new(false);
/// Module `debug` (bit 11); its Lua error hooks were installed in DllMain.
pub const MOD_DEBUG: u32 = 1 << 11;
static DEBUG_HOOKED: AtomicBool = AtomicBool::new(false);
/// Module `chara_legal` (bit 20): every character is legal; applied in DllMain.
pub const MOD_CHARA_LEGAL: u32 = 1 << 20;
static CHARA_LEGAL_ON: AtomicBool = AtomicBool::new(false);
/// Module `mods` (bit 31): mod folders `<game>/mods/<id>/`; plan read and the file-redirect hook installed in
/// DllMain, Lua roots handed to the `lua_patch` runner by the init thread.
pub const MOD_MODS: u32 = 1 << 31;
struct ModsState {
    plan: evt_modfmt::LoadPlan,
    overlay: crate::mods::Overlay,
    bad: Vec<(String, PathBuf)>,
    hook: Result<usize, String>,
    voice: crate::mods::voice::Setup,
}
static MODS_STATE: std::sync::OnceLock<ModsState> = std::sync::OnceLock::new();

/// Init thread: log the mod plan (read in DllMain). Returns the Lua roots and whether the module is active.
fn init_mods(cfg: &Config) -> (bool, Vec<(String, PathBuf)>) {
    let Some(st) = MODS_STATE.get() else {
        warn!("mods: module not active (nie.exe not v7.1.2 at DllMain)");
        return (false, Vec::new());
    };
    let modules = toml::Value::try_from(&cfg.modules).ok().and_then(|v| v.as_table().cloned()).unwrap_or_default();
    let missing = crate::mods::missing_loader_modules(&st.plan, &modules);
    for (lvl, line) in crate::mods::report(&st.plan, &st.overlay, &st.bad, &missing) {
        match lvl {
            crate::mods::Lvl::Info => info!("{line}"),
            crate::mods::Lvl::Warn => warn!("{line}"),
            crate::mods::Lvl::Error => error!("{line}"),
        }
    }
    match &st.hook {
        Ok(0) => {}
        Ok(n) => info!("mods: {} hooked: {n} file(s) redirected", crate::mods::sigs::MODS_RESOLVE_OVERLAY.name),
        Err(e) => crate::mods::hooks::log_install_error(e),
    }
    for line in crate::mods::voice::report(&st.voice) {
        match line {
            (true, l) => warn!("{l}"),
            (false, l) => info!("{l}"),
        }
    }
    let lua = st.plan.lua_roots();
    if !lua.is_empty() && !LUA_PATCH_HOOKED.load(Ordering::Acquire) {
        error!("mods: lua_patch hooks missing: the mods' Lua patches do NOT run");
    }
    (true, lua)
}

pub fn ctx() -> Option<&'static Ctx> {
    CTX.get()
}

/// Active mods `id@version` in load order (ModLoader console `status`); empty when module `mods` is off.
pub fn active_mods() -> Vec<String> {
    MODS_STATE
        .get()
        .map(|s| s.plan.mods.iter().map(|m| format!("{}@{}", m.manifest.id, m.manifest.version)).collect())
        .unwrap_or_default()
}

fn exe_path() -> PathBuf {
    let mut buf = vec![0u16; 1024];
    let n = unsafe { GetModuleFileNameW(std::ptr::null_mut(), buf.as_mut_ptr(), buf.len() as u32) } as usize;
    PathBuf::from(String::from_utf16_lossy(&buf[..n]))
}

fn load_config(data_dir: &std::path::Path) -> (Config, Option<String>) {
    let path = data_dir.join("config.toml");
    match std::fs::read_to_string(&path) {
        Ok(t) => match Config::parse(&t) {
            Ok(c) => (c, None),
            Err(e) => (Config::default(), Some(format!("config.toml invalid ({e}); using defaults"))),
        },
        Err(_) => {
            let _ = std::fs::write(&path, DEFAULT_TOML);
            (Config::default(), None)
        }
    }
}

pub fn on_process_attach(hinst: *mut core::ffi::c_void) {
    unsafe { DisableThreadLibraryCalls(hinst as _) };
    let exe = exe_path();
    let is_game = exe.file_name().and_then(|n| n.to_str()).is_some_and(|n| n.eq_ignore_ascii_case(gate::EXE_NAME));
    if !is_game {
        return; // another process in the game folder (launcher, bootstrapper): proxy only
    }
    let quick_ok = unsafe { crate::pe::live::headers(crate::pe::live::exe_base()) }
        .is_some_and(|h| gate::header_matches(h.time_date_stamp, h.size_of_image));
    let game_dir = exe.parent().map(PathBuf::from).unwrap_or_default();
    let data_dir = game_dir.join("evt_loader");
    let _ = std::fs::create_dir_all(&data_dir);
    let (mut cfg, cfg_error) = load_config(&data_dir);
    if cfg.modules.mods {
        // mod plan first: a mod's `loader_modules` turn built-in modules on before any switch below is read
        let plan = crate::mods::plan_root_for(&game_dir.join(crate::mods::MODS_DIR), Some(crate::MODLOADER_VERSION));
        for (lvl, line) in crate::mods::apply_loader_modules(&plan, &mut cfg.modules) {
            match lvl {
                crate::mods::Lvl::Info => info!("{line}"),
                crate::mods::Lvl::Warn => warn!("{line}"),
                crate::mods::Lvl::Error => error!("{line}"),
            }
        }
        crate::mods::set_early_plan(plan);
    }
    // hang watchdog (module debug): the thread loading winmm.dll is the game's main (Lua / window) thread
    crate::debug::watchdog::set_main_thread();
    for l in cfg.module_gate_report() {
        info!("{l}");
    }
    let _ = CTX.set(Ctx { exe, game_dir, data_dir, cfg, cfg_error });
    if !quick_ok {
        let _ = std::thread::Builder::new().name("vr-loader-init".into()).spawn(move || init_thread(quick_ok));
        return;
    }
    if let (Some(text), Some(ctx)) = (crate::game::Text::current(), CTX.get()) {
        // resolve every module's signatures on the clean .text before the first patch (crate::registry)
        text.prime_all();
        if ctx.cfg.modules.lua_bridge {
            crate::lua::install_hook(&text);
        }
        if ctx.cfg.modules.debug {
            DEBUG_HOOKED.store(crate::debug::install_hooks(&text, &ctx.cfg.debug), Ordering::Release);
        }
        if ctx.cfg.modules.chara_legal {
            // in DllMain: before another patcher's thread (UVR's D3DCOMPILER_47.dll) can run
            CHARA_LEGAL_ON.store(crate::chara_legal::apply(&text), Ordering::Release);
        }
        if ctx.cfg.modules.mods {
            // read the plan now: boot-time file opens must already be redirected
            let plan = crate::mods::take_early_plan().unwrap_or_else(|| {
                crate::mods::plan_root_for(&ctx.game_dir.join(crate::mods::MODS_DIR), Some(crate::MODLOADER_VERSION))
            });
            let (overlay, bad) = crate::mods::Overlay::from_plan(&plan);
            crate::mods::set_served_previews(&overlay);
            // voice packs: «voice language» (evt_loader/voice.json) before the boot-time voice sheets load
            let voice = crate::mods::voice::setup(&plan, &ctx.data_dir);
            // (also hooked with no mod files when a plugin may serve generated files: plugin API file_serve)
            let plugins = plan.mods.iter().any(|m| !m.manifest.plugin.is_empty());
            let hook = crate::mods::hooks::install_hook_with(&text, overlay.clone(), plugins);
            let _ = MODS_STATE.set(ModsState { plan, overlay, bad, hook, voice });
            // native plugins: list them and arm their early phase (CRT entry point)
            if let Some(st) = MODS_STATE.get() {
                crate::plugins::host::arm_early(&st.plan, &ctx.data_dir);
            }
        }
        let mods_lua = MODS_STATE.get().is_some_and(|s| !s.plan.lua_roots().is_empty());
        if ctx.cfg.modules.lua_patch || mods_lua {
            // file open + script chunk loader hooks: before the first script (title menu) is loaded
            LUA_PATCH_HOOKED.store(crate::lua_patch::hooks::install_hooks(&text), Ordering::Release);
        }
        if ctx.cfg.modules.console {
            // ModLoader console taps: chain links on the hooks above + OpenMenu + PlayCharaVoice + Lua filters
            crate::console::rt::install(&text, &ctx.cfg.console);
        }
    }
    let _ = std::thread::Builder::new().name("vr-loader-init".into()).spawn(move || init_thread(quick_ok));
}

fn finish(enabled: bool) {
    ENABLED.store(enabled, Ordering::Release);
    let mut d = DONE.lock().unwrap();
    *d = true;
    DONE_CV.notify_all();
}

/// Wait for the init thread; true if the loader is active (gate passed).
pub fn wait_init(timeout: Duration) -> bool {
    let d = DONE.lock().unwrap();
    let (d, _) = DONE_CV.wait_timeout_while(d, timeout, |done| !*done).unwrap();
    *d && ENABLED.load(Ordering::Acquire)
}

pub fn enabled() -> bool {
    ENABLED.load(Ordering::Acquire)
}

fn init_thread(quick_ok: bool) {
    let Some(ctx) = CTX.get() else { return finish(false) };
    let cfg = &ctx.cfg;
    crate::log::open(&ctx.data_dir.join("loader.log"), Level::parse(&cfg.loader.log_level), cfg.loader.debug_output);
    info!(
        "VR-ModLoader {} (vr-loader {}, Lua API {}) in {}",
        crate::MODLOADER_VERSION,
        crate::LOADER_VERSION,
        crate::API_VERSION,
        ctx.exe.display()
    );
    if let Some(e) = &ctx.cfg_error {
        warn!("{e}");
    }
    if cfg.modules.console {
        // ModLoader console window (mirror of this log + command line), before the gate so its errors are seen
        crate::console::rt::start(&cfg.console, &ctx.data_dir);
    }
    if !quick_ok {
        error!("nie.exe PE header is not v7.1.2: loader disabled (winmm forwarding only)");
        return finish(false);
    }
    if cfg.loader.skip_hash_check {
        warn!("SHA-1 check skipped (config)");
    } else {
        match gate::sha1_file(&ctx.exe) {
            Ok(h) if h == gate::V712_SHA1 => info!("nie.exe SHA-1 {h} = v7.1.2"),
            Ok(h) => {
                error!("nie.exe SHA-1 {h} is not v7.1.2 ({}): loader disabled", gate::V712_SHA1);
                return finish(false);
            }
            Err(e) => {
                error!("cannot hash nie.exe ({e}): loader disabled");
                return finish(false);
            }
        }
    }
    let Some(text) = crate::game::Text::current() else {
        error!("cannot read nie.exe .text: loader disabled");
        return finish(false);
    };
    // «is a real match running?» for plugins (game_state) and the console: the core resolves g_sceneSoccer itself
    match text.resolve_rip(&crate::sigs::G_SCENE_SOCCER) {
        Some(g) => crate::match_state::set_scene_global(Some(g)),
        None => warn!("match_state: g_sceneSoccer not resolved: game_state match.* keys unavailable"),
    }
    // native plugins of the active mods: before the built-in modules, so a built-in module a plugin `provides` can
    // yield to it
    if let Some(st) = MODS_STATE.get() {
        crate::plugins::host::load_all(&st.plan, &ctx.data_dir);
    }
    // closing the window always ends the process (data reads + ExitProcess IAT only)
    if cfg.modules.quit_fix {
        if let Some(p) = crate::plugins::host::replacing_builtin("quit_fix") {
            info!("quit_fix: built-in module yields to plugin {p} (it provides quit_fix): only the plugin runs");
        } else if !crate::quit_fix::activate(&text, &cfg.quit_fix) {
            warn!("quit_fix: module not active (see errors above): closing in a match keeps the retail behaviour");
        }
    }
    let debug_on = cfg.modules.debug
        && DEBUG_HOOKED.load(Ordering::Acquire)
        && crate::debug::activate(&text, &ctx.game_dir, &cfg.debug);
    if cfg.modules.debug && !debug_on {
        warn!("debug: module not active (see errors above): Lua errors stay hidden");
    }
    let chara_legal_on = cfg.modules.chara_legal && CHARA_LEGAL_ON.load(Ordering::Acquire);
    if cfg.modules.chara_legal && !chara_legal_on {
        warn!("chara_legal: not active (see warnings above): added characters may show the placeholder face");
    }
    let (mods_on, mods_lua) = if cfg.modules.mods { init_mods(cfg) } else { (false, Vec::new()) };
    let lua_patch_on = (cfg.modules.lua_patch || !mods_lua.is_empty())
        && LUA_PATCH_HOOKED.load(Ordering::Acquire)
        && crate::lua_patch::hooks::activate(&ctx.data_dir, cfg.modules.lua_patch, mods_lua, &cfg.lua_patch);
    if cfg.modules.lua_patch && !lua_patch_on {
        warn!("lua_patch: module not active (see errors above): no Lua patch files run");
    }
    if lua_patch_on {
        // mods' patches overwriting each other / patching a script another mod replaces whole
        let ov = MODS_STATE.get().map(|s| s.plan.file_overrides()).unwrap_or_default();
        crate::lua_patch::hooks::report_conflicts(ov.iter().map(|(k, (m, _))| (k, m)));
    }
    let mods = if lua_patch_on && cfg.modules.lua_patch { MOD_LUA_PATCH } else { 0 }
        | if mods_on { MOD_MODS } else { 0 }
        | if chara_legal_on { MOD_CHARA_LEGAL } else { 0 }
        | if debug_on { MOD_DEBUG } else { 0 };
    MODULES.store(mods, Ordering::Release);
    if !cfg.modules.lua_bridge {
        info!("lua_bridge disabled: no Lua commands");
        return finish(true);
    }
    let Some(api) = crate::game::LuaApi::resolve(&text) else {
        error!("Lua API signatures not resolved: Lua commands disabled");
        return finish(true);
    };
    if mods_on {
        // in-game «Mods» menu: CMND_EVT_MODS_* list / toggle / rescan <game>/mods; `applied` = the plan of DllMain,
        // so the menu can say when a change needs a restart
        let applied: Vec<String> =
            MODS_STATE.get().map(|s| s.plan.mods.iter().map(|m| m.manifest.id.clone()).collect()).unwrap_or_default();
        crate::mods::cmds::init(&ctx.game_dir.join(crate::mods::MODS_DIR), applied);
        // voice language row (voice packs): CMND_EVT_VOICE_GET / _NAME / _SET
        crate::mods::voice::init();
    }
    lua::register("CMND_EVT_LOADER_VERSION", cmd_version);
    lua::register("CMND_EVT_LOG", cmd_log);
    lua::register("CMND_EVT_UI_FLAG_SET", cmd_ui_flag_set);
    lua::register("CMND_EVT_UI_FLAG_GET", cmd_ui_flag_get);
    let n = lua::activate(api);
    if lua::hook_installed() {
        info!("init done: {n} Lua commands active, modules mask {mods}");
    } else {
        error!("Lua dispatcher hook not installed: CMND_EVT_* commands unavailable");
    }
    finish(true);
}

fn cmd_version(c: &mut Call) {
    c.push_int(crate::API_VERSION);
    c.push_int(MODULES.load(Ordering::Acquire) as i64);
}

/// Small UI flags shared between menus (each menu has its own Lua state): `key` = a CRC32 chosen by the script.
static UI_FLAGS: std::sync::Mutex<Vec<(u32, i64)>> = std::sync::Mutex::new(Vec::new());

/// A loader UI flag (`CMND_EVT_UI_FLAG_SET`) read from Rust; 0 = not set.
pub fn ui_flag(key: u32) -> i64 {
    UI_FLAGS.lock().ok().and_then(|f| f.iter().find(|e| e.0 == key).map(|e| e.1)).unwrap_or(0)
}

/// `CMND_EVT_UI_FLAG_SET(key, value)` → `value`.
fn cmd_ui_flag_set(c: &mut Call) {
    let key = c.u32(0).unwrap_or(0);
    let v = c.int(1).unwrap_or(0);
    let mut f = UI_FLAGS.lock().unwrap_or_else(|e| e.into_inner());
    f.retain(|e| e.0 != key);
    if v != 0 {
        f.push((key, v));
    }
    c.push_int(v);
}

/// `CMND_EVT_UI_FLAG_GET(key)` → the value (0 = not set).
fn cmd_ui_flag_get(c: &mut Call) {
    let key = c.u32(0).unwrap_or(0);
    c.push_int(ui_flag(key));
}

fn cmd_log(c: &mut Call) {
    let parts: Vec<String> = (0..c.nargs.min(8)).map(|i| c.describe(i)).collect();
    info!("lua log: {}", parts.join(", "));
    c.push_bool(true);
}
