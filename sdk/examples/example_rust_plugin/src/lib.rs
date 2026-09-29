//! Example ModLoader plugin (Rust): everything a first plugin usually needs, and nothing that changes the game.
//!
//! * `early`  - runs on the game's main thread before any game code (just logs here);
//! * `init`   - reads its config, lists the active mods, registers a Lua command and hooks
//!              `CMenuController::OpenMenu` with a chained hook that only counts and logs (it always calls on).
//!
//! Build: see README.md. Log lines appear in `<game>\evt_loader\loader.log` prefixed `example_rust_plugin: `.

use evt_plugin_sdk::{declare_plugin, evt_debug, evt_info, evt_warn, try_host, Host, LuaCall};
use std::sync::atomic::{AtomicU32, AtomicUsize, Ordering};
use std::sync::OnceLock;

// ------------------------------------------------------------------------------------------- game function
//
// `bool CMenuController::OpenMenu(this, const u32 *nameHash, const OpenMenuParam *p)` (nie.exe v7.1.2, RVA
// 0x10DEC90; docs/game/engine/hook-map.md). Called for every menu the game opens. The signature is unique in
// `.text`; the first 16 bytes (`mov [rsp+10h],rbx` + seven pushes) are whole instructions without relative operands,
// so they can be stolen by an inline hook (hook_inline needs >= 14 such bytes, checked against `PROLOGUE`).

const SIG: &str = "48 89 5C 24 10 55 56 57 41 54 41 55 41 56 41 57 48 8D AC 24 B0 FE FF FF 48 81 EC 50 02 00 00 \
                   48 8B 05 ?? ?? ?? ?? 48 33 C4 48 89 85 40 01 00 00";
const EXPECTED_RVA: u32 = 0x10DEC90;
const PROLOGUE: [u8; 16] = [0x48, 0x89, 0x5C, 0x24, 0x10, 0x55, 0x56, 0x57, 0x41, 0x54, 0x41, 0x55, 0x41, 0x56, 0x41, 0x57];

type OpenMenuFn = unsafe extern "C" fn(this: usize, name_hash: *const u32, param: *const u8) -> u8;

// ------------------------------------------------------------------------------------------- state

/// What the hook calls to continue: the loader keeps it up to date (next plugin's detour, or the original function).
/// It must be a `static`: the loader writes it whenever the chain changes.
static NEXT: AtomicUsize = AtomicUsize::new(0);
static MENUS_OPENED: AtomicU32 = AtomicU32::new(0);
static CONFIG: OnceLock<Config> = OnceLock::new();

struct Config {
    greeting: String,
    log_first_menus: u32,
}

impl Config {
    /// Defaults live in the code; `config.toml` of the mod and `[mods.example_rust_plugin]` of evt_loader\config.toml
    /// (merged by the loader into one TOML text) only override them.
    fn parse(text: &str) -> Config {
        let mut c = Config { greeting: "hello from Rust".into(), log_first_menus: 5 };
        match text.parse::<toml::Table>() {
            Ok(t) => {
                if let Some(s) = t.get("greeting").and_then(|v| v.as_str()) {
                    c.greeting = s.to_string();
                }
                if let Some(n) = t.get("log_first_menus").and_then(|v| v.as_integer()) {
                    c.log_first_menus = n.clamp(0, 10_000) as u32;
                }
            }
            Err(e) => evt_warn!("config is not valid TOML ({e}): using the defaults"),
        }
        c
    }
}

// ------------------------------------------------------------------------------------------- phases

/// Early phase: game main thread, before any game code. Hooks that must exist before the game starts go here.
/// The game's globals do not exist yet, and the log is buffered in memory until the loader opens the file.
fn early(h: &'static Host) -> Result<(), String> {
    evt_info!("early phase on the game's main thread (loader {})", h.loader_version);
    Ok(())
}

/// Init phase: loader init thread, after the nie.exe v7.1.2 SHA-1 check. Returning Err disables this plugin only
/// (its hooks are removed, the game goes on).
fn init(h: &'static Host) -> Result<(), String> {
    let cfg = CONFIG.get_or_init(|| Config::parse(&h.config_text()));
    evt_info!(
        "init: mod {} {} in {} (load index {}); greeting {:?}, log_first_menus {}",
        h.mod_id,
        h.mod_version,
        h.mod_dir.display(),
        h.load_index,
        cfg.greeting,
        cfg.log_first_menus
    );
    let names: Vec<String> = h.mods().into_iter().map(|m| format!("{}@{}", m.id, m.version)).collect();
    evt_info!("{} active mod(s) in load order: {}", names.len(), names.join(", "));

    // Lua command, callable from any Lua script with funcLuaCommand(crc32("CMND_EVT_EXAMPLE_MENU_COUNT")).
    // Registration is only possible during init and needs [modules] lua_bridge = true (mod.toml asks for it).
    h.lua_register("CMND_EVT_EXAMPLE_MENU_COUNT", menu_count_cmd)
        .map_err(|e| format!("lua_register failed ({e}); is [modules] lua_bridge on?"))?;

    // The chained hook. A failure here is not fatal for this example: log it and keep the Lua command.
    match install_hook(h) {
        Ok(()) => evt_info!("OpenMenu hooked (chained, priority 0)"),
        Err(e) => evt_warn!("OpenMenu hook not installed: {e}"),
    }
    Ok(())
}

fn install_hook(h: &'static Host) -> Result<(), String> {
    let target = h.sig("OpenMenu", SIG, EXPECTED_RVA).ok_or("signature not found (other game version?)")?;
    // SAFETY: `detour` has the exact signature and calling convention of OpenMenu and always continues through
    // NEXT; NEXT is a static; PROLOGUE is the target's real first bytes (part of the unique signature).
    unsafe { h.hook_inline(target, &PROLOGUE, open_menu_detour as *const (), &NEXT, 0) }
        .map_err(|code| format!("hook_inline returned {code} (details in loader.log)"))
}

// ------------------------------------------------------------------------------------------- the hook

/// Runs on whatever thread the game opens menus from. Must not panic (a panic across `extern "C"` aborts the
/// process): everything here is infallible, and `try_host` avoids the `host()` panic.
unsafe extern "C" fn open_menu_detour(this: usize, name_hash: *const u32, param: *const u8) -> u8 {
    let n = MENUS_OPENED.fetch_add(1, Ordering::Relaxed) + 1;
    if let (Some(h), Some(cfg)) = (try_host(), CONFIG.get()) {
        if n <= cfg.log_first_menus {
            // guarded read: never dereference game pointers directly from a plugin
            match h.read::<u32>(name_hash as usize) {
                Some(hash) => evt_info!("menu #{n} opened (crc32 of its name 0x{hash:08X})"),
                None => evt_debug!("menu #{n} opened (name hash unreadable)"),
            }
        }
    }
    // Continue the chain: the next plugin's detour or the original function. 0 = chain not set up (cannot happen
    // once hook_inline returned OK); answer "false" rather than jump to null.
    let next = NEXT.load(Ordering::Acquire);
    if next == 0 {
        return 0;
    }
    let f: OpenMenuFn = std::mem::transmute::<usize, OpenMenuFn>(next);
    f(this, name_hash, param)
}

// ------------------------------------------------------------------------------------------- the Lua command

/// `CMND_EVT_EXAMPLE_MENU_COUNT([reset])` -> `count, greeting`. Runs on the game's Lua thread: keep it short.
/// Arguments start at index 0 (the first one after the command hash); pushed values are the return values.
fn menu_count_cmd(call: &mut LuaCall) {
    let count = if call.nargs() > 0 && call.int(0) == Some(1) {
        MENUS_OPENED.swap(0, Ordering::Relaxed)
    } else {
        MENUS_OPENED.load(Ordering::Relaxed)
    };
    call.push_int(i64::from(count));
    call.push_str(CONFIG.get().map_or("", |c| c.greeting.as_str()));
}

// Exports evt_plugin_api_version, evt_plugin_init and (because of `early = ...`) evt_plugin_early.
declare_plugin!(init = init, early = early);
