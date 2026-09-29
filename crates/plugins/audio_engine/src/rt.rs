//! Run time of plugin `audio_engine`, all through the ModLoader API (evt-plugin-sdk):
//!
//! * **early phase** (exe entry point, before any game code): the `sound_queue_sheet` merge ([`crate::boot_merge`])
//!   is written into the fixed-size file the mods overlay serves.
//! * **init**: `armed_voice` — chained inline hooks on `PlayCharaVoice 0x16FC7E0` (the shout) and
//!   `ExecAuraCommand 0x14C4D90` (its own record of the armed commands the engine executes, so it does not depend on
//!   any other module), plus the optional state key `keshin_armed.hiper_armour.<actor>` (the Hiper armour being
//!   fired; only loaders / mods with a Hiper-armour feature answer it, otherwise it reads 0).
//!
//! The loader prefixes every line with `audio_engine: `.

use crate::armed::{self, Cand, Outcome};
use crate::aura::Auras;
use crate::Cfg;
use evt_plugin_sdk::{declare_plugin, host, Host, Level};
use std::ffi::CString;
use std::sync::atomic::{AtomicU32, AtomicU64, AtomicUsize, Ordering};
use std::sync::OnceLock;
use std::time::Instant;

fn log(level: Level, msg: &str) {
    host().log(level, msg);
}
macro_rules! info { ($($a:tt)*) => { log(Level::Info, &format!($($a)*)) }; }
macro_rules! warn { ($($a:tt)*) => { log(Level::Warn, &format!($($a)*)) }; }
macro_rules! error { ($($a:tt)*) => { log(Level::Error, &format!($($a)*)) }; }

// ---------------------------------------------------------------- signatures (nie.exe v7.1.2)

/// `u32* PlayCharaVoice(soundMgr, u32* handle, const char* bank, const char* suffix, const Params*)` 0x16FC7E0
/// (crates/vr-loader/src/armed_voice/sigs.rs). Prologue: `push rbp/rbx/rsi/rdi; lea rbp,[rsp-168h]; sub rsp,268h`.
const PLAY_CHARA_VOICE: (&str, &str, u32) = (
    "audio_engine.PlayCharaVoice",
    "40 55 53 56 57 48 8D AC 24 98 FE FF FF 48 81 EC 68 02 00 00 48 8B 05 ?? ?? ?? ?? 48 33 C4 \
     48 89 85 50 01 00 00 48 8B DA 48 8B F1 48 8B BD B0 01 00 00 48 8B 05 ?? ?? ?? ?? 48 8B 88 50 6A 00 00",
    0x16FC7E0,
);
const PLAY_STEAL: usize = 20;
/// The retail `aura_skill_config` (v7.1.2), read from the player's own game files.
const RETAIL_AURA: &str = "data/common/gamedata/skill/aura_skill_config_1.04.09.00.cfg.bin";

/// `void ExecAuraCommand(cmd)` 0x14C4D90 (keshin_armed/sigs.rs): `mov rax,rsp; push rbp/rbx/rsi/r15;
/// lea rbp,[rax-5Fh]; sub rsp,0E8h` = 19 bytes.
const EXEC_AURA: (&str, &str, u32) = ("audio_engine.ExecAuraCommand", "48 8B C4 55 53 56 41 57 48 8D 68 A1 48 81 EC E8 00 00 00", 0x14C4D90);
const EXEC_STEAL: usize = 19;
/// `CalcUnitMoveSpeed` 0x154DAA0: its `mov r9,[rip+x]` at +0x0C is `g_soccerActors` (stamina/sigs.rs).
const CALC_MOVE: (&str, &str, u32) = ("audio_engine.CalcUnitMoveSpeed", "4C 8B DC 53 55 57 41 55 48 83 EC 48 4C 8B 0D ?? ?? ?? ??", 0x154DAA0);
const G_ACTORS_DISP: (u32, u32) = (0x0F, 0x13);
/// `SoccerChara* ActorUnit(SoccerActor*)` 0x15B25E0 (skill_rank/sigs.rs): the match unit of an actor.
const ACTOR_UNIT: (&str, &str, u32) =
    ("audio_engine.ActorUnit", "48 89 74 24 10 57 48 83 EC 20 48 8B 35 ?? ?? ?? ?? 48 8B F9 48 85 F6 75 0D 33 C0", 0x15B25E0);

// ---------------------------------------------------------------- data layout (keshin-armed.md §2)

const ACTOR_MAX: usize = 0x3A;
const ACTOR_STRIDE: usize = 0x570;
const ACTOR_HDL: usize = 0x94;
const SUMMON_SKILL: usize = 0x358;
const SUMMON_STATE: usize = 0x374;
const META_SKILL: usize = 0x380;
const META_MAX: usize = 0x388;
const META_LEFT: usize = 0x39C;
const META_STATE: usize = 0x3A0;
const UNIT_SKILLS: usize = 0x30;
const UNIT_SKILL_STRIDE: usize = 0x1C;
/// Aura command: type `+0x16C` (0x16 exec), actor handle `+0x60`, skill `+0x40`, commit pass `+0x171`.
const CMD_TYPE: usize = 0x16C;
const CMD_HDL: usize = 0x60;
const CMD_SKILL: usize = 0x40;
const CMD_COMMIT: usize = 0x171;

#[derive(Debug, Clone, Copy)]
struct Addrs {
    /// `g_soccerActors` (pointer variable; `[it] + idx * 0x570`, 0 outside matches).
    g_actors: usize,
    actor_unit: Option<usize>,
}

static ADDRS: OnceLock<Addrs> = OnceLock::new();
static AURAS: OnceLock<Auras> = OnceLock::new();
static PLAY_NEXT: AtomicUsize = AtomicUsize::new(0);
static EXEC_NEXT: AtomicUsize = AtomicUsize::new(0);
static EPOCH: OnceLock<Instant> = OnceLock::new();
static EXEC_SKILL: [AtomicU32; ACTOR_MAX] = [const { AtomicU32::new(0) }; ACTOR_MAX];
static EXEC_MS: [AtomicU64; ACTOR_MAX] = [const { AtomicU64::new(0) }; ACTOR_MAX];

type PlayFn = unsafe extern "C" fn(u64, *mut u32, *const u8, *const u8, u64) -> *mut u32;
type ExecFn = unsafe extern "C" fn(u64) -> u64;

fn now_ms() -> u64 {
    EPOCH.get_or_init(Instant::now).elapsed().as_millis() as u64 + 1
}

fn read<T: Copy>(a: usize) -> Option<T> {
    host().read::<T>(a)
}

/// Guarded read of a C string of at most `max` bytes (None when longer or unreadable).
fn read_cstr(addr: usize, max: usize) -> Option<String> {
    if addr < 0x10000 {
        return None;
    }
    let mut v = Vec::with_capacity(16);
    for i in 0..=max {
        let b = read::<u8>(addr + i)?;
        if b == 0 {
            return String::from_utf8(v).ok();
        }
        v.push(b);
    }
    None
}

// ---------------------------------------------------------------- ExecAuraCommand: armed commands executed

/// An aura exec command reached the executor: first pass time per (actor, skill), like the ModLoader's `aura_data`.
fn note_exec(hdl: u32, skill: u32, commit: bool) {
    let i = (hdl & 0xFF) as usize;
    if i >= ACTOR_MAX {
        return;
    }
    if EXEC_SKILL[i].swap(skill, Ordering::Relaxed) != skill || !commit {
        EXEC_MS[i].store(now_ms(), Ordering::Relaxed);
    }
}

unsafe extern "C" fn exec_detour(cmd: u64) -> u64 {
    let next: ExecFn = std::mem::transmute::<usize, ExecFn>(EXEC_NEXT.load(Ordering::Acquire));
    if cmd >= 0x10000 {
        let c = cmd as usize;
        if read::<u8>(c + CMD_TYPE) == Some(0x16) {
            let hdl = read::<u32>(c + CMD_HDL).unwrap_or(0);
            let skill = read::<u32>(c + CMD_SKILL).unwrap_or(0);
            let commit = read::<u8>(c + CMD_COMMIT).unwrap_or(0) != 0;
            note_exec(hdl, skill, commit);
        }
    }
    next(cmd)
}

// ---------------------------------------------------------------- PlayCharaVoice: the shout

/// The unit's 6 skill rows (the armour lent by hiper_slot or the morphed keshin move sits in one of them).
fn unit_rows(actor: usize) -> [u32; 6] {
    let mut rows = [0u32; 6];
    let Some(f) = ADDRS.get().and_then(|a| a.actor_unit) else { return rows };
    let Ok(u) = host().call(f, &[actor as u64]) else { return rows };
    let unit = u as usize;
    if unit < 0x10000 {
        return rows;
    }
    for (s, r) in rows.iter_mut().enumerate() {
        *r = read::<u32>(unit + UNIT_SKILLS + s * UNIT_SKILL_STRIDE).unwrap_or(0);
    }
    rows
}

/// What `actor` may be putting on before the exec: (Hiper armour being fired, keshin summoned, the unit's rows while
/// that keshin is out). Same data as the ModLoader's `keshin_armed::hooks::arming_state`.
fn arming_state(actor: usize, idx: usize) -> (u32, u32, [u32; 6]) {
    let hiper = host().state(&format!("keshin_armed.hiper_armour.{idx}")).map(|v| v as u32).unwrap_or(0);
    let keshin = read::<u32>(actor + SUMMON_SKILL).unwrap_or(0);
    if keshin == 0 || read::<u8>(actor + SUMMON_STATE).unwrap_or(0) == 0 {
        return (hiper, 0, [0; 6]);
    }
    (hiper, keshin, unit_rows(actor))
}

fn candidates(bank_crc: u32, auras: &Auras) -> Vec<(Cand, u32)> {
    let mut v = Vec::new();
    let Some(a) = ADDRS.get() else { return v };
    let Some(base) = host().read_ptr(a.g_actors) else { return v };
    let now = now_ms();
    let keys = |id| auras.armed_keys(id);
    for i in 0..ACTOR_MAX {
        let actor = base + i * ACTOR_STRIDE;
        let Some(hdl) = read::<u32>(actor + ACTOR_HDL).filter(|&h| h != 0 && (h & 0xFF) as usize == i) else { continue };
        let meta = read::<u32>(actor + META_SKILL).unwrap_or(0);
        let state = read::<u8>(actor + META_STATE).unwrap_or(0);
        let ex_skill = EXEC_SKILL[i].load(Ordering::Relaxed);
        let first = EXEC_MS[i].load(Ordering::Relaxed);
        let ex_age = (ex_skill != 0 && first != 0).then(|| now.saturating_sub(first));
        let ex_recent = ex_age.is_some_and(|t| t <= armed::EXEC_RECENT_MS) && auras.armed_keys(ex_skill).is_some();
        let mut pending = 0;
        let skill = if state != 0 && meta != 0 && auras.armed_keys(meta).is_some() {
            meta
        } else if ex_recent {
            ex_skill
        } else {
            let (hiper, keshin, rows) = arming_state(actor, i);
            let Some((s, lv)) = armed::pending_armour(hiper, keshin, &rows, keys) else { continue };
            pending = lv;
            s
        };
        v.push((
            Cand {
                idx: i,
                skill,
                state,
                left: read::<f32>(actor + META_LEFT).unwrap_or(0.0),
                max: read::<f32>(actor + META_MAX).unwrap_or(0.0),
                exec_age_ms: ex_age.filter(|_| ex_skill == skill),
                owner_match: auras.is_owner(skill, bank_crc),
                pending,
            },
            hdl,
        ));
    }
    v
}

fn plan(bank_ptr: usize) -> Option<armed::Plan> {
    let bank = read_cstr(bank_ptr, armed::CUE_BUF)?;
    if bank.is_empty() {
        return None;
    }
    let auras = AURAS.get()?;
    let cands = candidates(crc32fast::hash(bank.as_bytes()), auras);
    Some(armed::plan(&bank, &cands, auras))
}

fn is_armed_suffix(p: *const u8) -> bool {
    read_cstr(p as usize, 8).is_some_and(|s| s == armed::ARMED_SUFFIX)
}

unsafe extern "C" fn play_detour(mgr: u64, out: *mut u32, bank: *const u8, suffix: *const u8, params: u64) -> *mut u32 {
    let next: PlayFn = std::mem::transmute::<usize, PlayFn>(PLAY_NEXT.load(Ordering::Acquire));
    if out.is_null() || bank.is_null() || !is_armed_suffix(suffix) {
        return next(mgr, out, bank, suffix, params);
    }
    let Some(p) = std::panic::catch_unwind(|| plan(bank as usize)).ok().flatten() else {
        return next(mgr, out, bank, suffix, params);
    };
    for s in &p.suffixes {
        let Ok(c) = CString::new(s.as_str()) else { continue };
        let r = next(mgr, out, bank, c.as_ptr() as *const u8, params);
        if read::<u32>(out as usize).unwrap_or(0) != 0 {
            info!("{}", armed::log_line(&p.bank, p.hdl, &p.keshin, &p.armour, &p.suffixes, &Outcome::Found(s.clone())));
            return r;
        }
    }
    let r = next(mgr, out, bank, suffix, params);
    let why = if read::<u32>(out as usize).unwrap_or(0) == 0 { "nothing played (voices off or bank not loaded)" } else { p.why };
    info!("{}", armed::log_line(&p.bank, p.hdl, &p.keshin, &p.armour, &p.suffixes, &Outcome::Fallback(why)));
    r
}

/// The fixed first bytes of an IDA pattern (the stolen prologue).
fn prologue(pattern: &str, n: usize) -> Option<Vec<u8>> {
    let v: Vec<u8> = pattern.split_whitespace().take(n).map(|t| u8::from_str_radix(t, 16)).collect::<Result<_, _>>().ok()?;
    (v.len() == n).then_some(v)
}

fn install_armed_voice(host: &'static Host) -> Result<(), String> {
    let calc = host.sig(CALC_MOVE.0, CALC_MOVE.1, CALC_MOVE.2).ok_or("CalcUnitMoveSpeed (g_soccerActors) not found")?;
    let g_actors = host.rip(calc, G_ACTORS_DISP.0, G_ACTORS_DISP.1).ok_or("g_soccerActors not resolved")?;
    let actor_unit = host.sig(ACTOR_UNIT.0, ACTOR_UNIT.1, ACTOR_UNIT.2);
    if actor_unit.is_none() {
        warn!("armed_voice: ActorUnit not found: an armour in the rows with the keshin out is not seen before its exec");
    }
    let _ = ADDRS.set(Addrs { g_actors, actor_unit });
    let game = host.path("game_dir").unwrap_or_default();
    AURAS.get_or_init(|| {
        let loose = Auras::load(&game);
        // the player's own retail rows (no mods overlay), after the loose ones
        let retail = host
            .game_file_path(RETAIL_AURA)
            .and_then(|p| std::fs::read(p).ok())
            .and_then(|b| crate::aura::parse(&b).ok())
            .unwrap_or_default();
        let n_loose = loose.len();
        let n_retail = retail.len();
        let loose_rows: Vec<crate::aura::AuraRow> = (0..n_loose).filter_map(|i| loose.row_at(i).cloned()).collect();
        let t = Auras::with_fallback(loose_rows, retail);
        info!("armed_voice: aura_skill_config rows: {n_loose} loose, {n_retail} retail ({} in all)", t.len());
        t
    });
    // own record of the armed commands the engine executes (optional: without it, the Hiper / rows / owner rules)
    match host.sig(EXEC_AURA.0, EXEC_AURA.1, EXEC_AURA.2) {
        Some(f) => {
            let pro = prologue(EXEC_AURA.1, EXEC_STEAL).ok_or("ExecAuraCommand prologue")?;
            if let Err(e) = unsafe { host.hook_inline(f, &pro, exec_detour as *const (), &EXEC_NEXT, 0) } {
                warn!("armed_voice: ExecAuraCommand hook failed ({e}): recent armour commands not tracked");
            }
        }
        None => warn!("armed_voice: ExecAuraCommand not found: recent armour commands not tracked"),
    }
    let f = host.sig(PLAY_CHARA_VOICE.0, PLAY_CHARA_VOICE.1, PLAY_CHARA_VOICE.2).ok_or("PlayCharaVoice signature not found")?;
    let pro = prologue(PLAY_CHARA_VOICE.1, PLAY_STEAL).ok_or("PlayCharaVoice prologue")?;
    unsafe { host.hook_inline(f, &pro, play_detour as *const (), &PLAY_NEXT, 0) }.map_err(|e| format!("PlayCharaVoice hook failed ({e})"))?;
    info!(
        "armed_voice: PlayCharaVoice hooked at RVA 0x{:X}: <bank>_armed -> <bank>_<armour id> when the bank has that cue",
        f - host.exe_base()
    );
    Ok(())
}

// ---------------------------------------------------------------- early phase: audio build, sound_queue_sheet, bgm_config

/// Name of the calling thread (`GetThreadDescription`); the ModLoader's init thread is `vr-loader-init`.
fn thread_name() -> Option<String> {
    use windows_sys::Win32::Foundation::LocalFree;
    use windows_sys::Win32::System::Threading::{GetCurrentThread, GetThreadDescription};
    unsafe {
        let mut p: *mut u16 = std::ptr::null_mut();
        if GetThreadDescription(GetCurrentThread(), &mut p) < 0 || p.is_null() {
            return None;
        }
        let mut n = 0;
        while *p.add(n) != 0 && n < 256 {
            n += 1;
        }
        let s = String::from_utf16_lossy(std::slice::from_raw_parts(p, n));
        LocalFree(p as _);
        Some(s)
    }
}

/// The placeholder path (a ModLoader without `file_serve`): rewrite the fixed-size served sheet in place.
fn queue_sheet_placeholder(host: &'static Host, late: bool) {
    let mods = active_mods(host);
    let game = host.path("game_dir").unwrap_or_default();
    let r = match crate::boot_merge(&game, &host.mod_id, &host.mod_dir, &mods) {
        Ok(r) => r,
        Err(e) => {
            error!("sound_queue_sheet: {e}: the banks mods declare are NOT registered");
            return;
        }
    };
    log_sheet(&r.base, &r.report, &r.warnings);
    let Some(bytes) = r.write else {
        info!("sound_queue_sheet: served placeholder up to date ({})", r.target.display());
        return;
    };
    if late {
        warn!("sound_queue_sheet: the early phase ran late (the game is already running): the placeholder is NOT rewritten now; the new banks load from the next start");
        return;
    }
    let tmp = r.target.with_extension("bin.tmp");
    match std::fs::write(&tmp, &bytes).and_then(|_| std::fs::rename(&tmp, &r.target)) {
        Ok(()) => info!("sound_queue_sheet: placeholder rewritten ({} B, same size as at DllMain): {}", bytes.len(), r.target.display()),
        Err(e) => error!("sound_queue_sheet: cannot write {} ({e}): the banks mods declare are NOT registered", r.target.display()),
    }
}

fn log_sheet(base: &str, rep: &crate::sheet::MergeReport, warnings: &[String]) {
    for w in warnings {
        warn!("sound_queue_sheet: {w}");
    }
    for (src, e) in &rep.errors {
        warn!("sound_queue_sheet: mod {src}: {e}");
    }
    let added: Vec<String> = rep.added.iter().map(|(src, b, g, _)| format!("{b} [{g}] ({src})")).collect();
    let present: Vec<String> = rep.present.iter().map(|(src, b)| format!("{b} ({src})")).collect();
    info!(
        "sound_queue_sheet: base = {base}; {} bank(s) added: {}; {} already listed{}",
        added.len(),
        if added.is_empty() { "-".into() } else { added.join(", ") },
        present.len(),
        if present.is_empty() { String::new() } else { format!(": {}", present.join(", ")) }
    );
}

use vr_framework::host::{active_mods, HostGame};

/// Write `bytes` to the cache at game path `key` (plain, e.g. a cfg.bin) and serve it.
fn serve_generated(host: &'static Host, cache: &crate::framework::Cache, key: &str, bytes: &[u8], what: &str) {
    let p = cache.path(key);
    let res = std::fs::create_dir_all(p.parent().unwrap()).and_then(|_| std::fs::write(&p, bytes));
    match res.map_err(|e| e.to_string()).and_then(|_| host.file_serve(key, &p).map_err(|c| format!("file_serve error {c}"))) {
        Ok(()) => info!("{what}: served from {} ({} B)", p.display(), bytes.len()),
        Err(e) => error!("{what}: NOT served ({e})"),
    }
}

fn log_notes(prefix: &str, notes: &[(bool, String)]) {
    for (w, n) in notes {
        if *w {
            warn!("{prefix}{n}");
        } else {
            info!("{prefix}{n}");
        }
    }
}

/// Early phase with `file_serve` (entry point, before any game code): audio build + sheet + bgm_config, all served.
fn early_audio(host: &'static Host, cfg: &Cfg) {
    let t0 = Instant::now();
    let mods = active_mods(host);
    let cache = crate::framework::Cache::new(vr_framework::host::cache_dir(host));
    let game = HostGame(host);
    // 1. the mods' [[voice]] / [[sfx]] / [[music]]
    let mut plan = crate::build::Plan::default();
    if cfg.build.enabled {
        let decls: Vec<(crate::ModDir, crate::decl::AudioToml)> = crate::framework::mods_with(&mods, crate::decl::FILE)
            .into_iter()
            .filter_map(|(m, t)| match crate::decl::parse(&t) {
                Ok(a) => Some((m, a)),
                Err(e) => {
                    warn!("mod {}: {} does not parse ({e}): its audio is not built", m.id, crate::decl::FILE);
                    None
                }
            })
            .collect();
        if decls.iter().any(|(_, a)| !a.voice.is_empty() || !a.sfx.is_empty() || !a.music.is_empty()) {
            let ip = host.mod_dir.join("index").join("audio_index.json");
            match crate::index::Index::load(&ip) {
                Ok(idx) => plan = crate::build::plan(&decls, &idx),
                Err(e) => error!("audio build: name index not readable ({e}): the mods' audio is NOT built"),
            }
        }
    }
    log_notes("", &plan.notes);
    let built = crate::build::execute(&plan, &crate::build::Env { game: &game, mods: &mods, cache: &cache }, true);
    log_notes("audio build: ", &built.notes);
    for (key, path) in &built.files {
        if let Err(c) = host.file_serve(key, path) {
            error!("audio build: {key}: file_serve error {c}: not served");
        }
    }
    // 2. sound_queue_sheet: the game's sheet + [[bank]] + the new banks
    if cfg.queue_sheet.merge {
        let key = crate::sheet::GAME_PATH;
        let base = host.game_file_path(key).and_then(|p| std::fs::read(&p).ok().map(|b| (format!("game ({})", p.display()), b)));
        let base = base.or_else(|| std::fs::read(crate::shipped_base(&host.mod_dir)).ok().map(|b| ("retail copy of the mod".to_string(), b)));
        match base {
            Some((label, b)) => match crate::sheet_build(&b, &host.mod_id, &mods, &plan.regs, false) {
                Ok(sb) => {
                    log_sheet(&label, &sb.report, &sb.warnings);
                    match sb.sheet.to_bytes() {
                        Ok(bytes) => serve_generated(host, &cache, key, &bytes, "sound_queue_sheet"),
                        Err(e) => error!("sound_queue_sheet: {e}"),
                    }
                }
                Err(e) => error!("sound_queue_sheet: {e}: the banks mods declare are NOT registered"),
            },
            None => error!("sound_queue_sheet: no base sheet (game file nor the mod's retail copy): the banks mods declare are NOT registered"),
        }
    }
    // 3. bgm_config: new ids + redirects
    if !plan.bgm_redirect.is_empty() || !plan.bgm_rows.is_empty() {
        let key = crate::build::BGM_CONFIG;
        let base = crate::framework::overlay_winner(&mods, key).map(|(_, p)| p).or_else(|| host.game_file_path(key));
        match base.and_then(|p| std::fs::read(p).ok()) {
            Some(b) => match crate::build::bgm_config(&b, &plan) {
                Ok((bytes, notes)) => {
                    log_notes("", &notes);
                    serve_generated(host, &cache, key, &bytes, "bgm_config");
                }
                Err(e) => error!("{e}: the mods' music ids are NOT applied"),
            },
            None => error!("bgm_config: the game's table not found: the mods' music ids are NOT applied"),
        }
    }
    // 4. the «Pack de voces» row of Opciones > «Ajustes del juego»
    if cfg.voice_row.enabled {
        voice_row(host, &mods, &cache);
    } else {
        info!("voice row: off ([voice_row] enabled = false): no «Pack de voces» row in Opciones");
    }
    info!(
        "audio build: {} bank(s) built, {} from the cache, {} file(s) served, in {} ms",
        built.rebuilt,
        built.cached,
        built.files.len(),
        t0.elapsed().as_millis()
    );
}

/// The bytes a game path has for the engine before this plugin: another mod's whole file (`files\`), else the game's.
fn base_of(host: &'static Host, mods: &[crate::ModDir], key: &str) -> Option<(String, Vec<u8>)> {
    if let Some((m, p)) = crate::framework::overlay_winner(mods, key) {
        return std::fs::read(&p).ok().map(|b| (format!("mod {}", m.id), b));
    }
    let p = host.game_file_path(key)?;
    std::fs::read(&p).ok().map(|b| ("game".to_string(), b))
}

/// The «Pack de voces» row (crate::voice_row): the settings list row (added once; a row of the same id, e.g. one left
/// by an earlier install, is adopted) and its texts (text_engine when active, else our own `menu_text` of each
/// language), all served.
fn voice_row(host: &'static Host, mods: &[crate::ModDir], cache: &crate::framework::Cache) {
    use crate::voice_row as vr;
    match base_of(host, mods, vr::SETTINGS) {
        Some((from, b)) => match vr::patch_settings(&b) {
            Ok((Some(bytes), ch)) => {
                info!("voice row: {} the settings list of the {from}", if ch == vr::RowChange::Adopted { "row «Pack de voces» already there (legacy id): adopted in" } else { "row «Pack de voces» added to" });
                serve_generated(host, cache, vr::SETTINGS, &bytes, "voice row: setting_list_config");
            }
            Ok((None, _)) => info!("voice row: the settings list of the {from} already has the row: nothing served"),
            Err(e) => error!("voice row: {e}: no «Pack de voces» row"),
        },
        None => error!("voice row: the game's settings list not found: no «Pack de voces» row"),
    }
    let texts = match std::fs::read_to_string(host.mod_dir.join("text.toml")).map_err(|e| format!("text.toml: {e}")).and_then(|s| vr::texts_from_toml(&s)) {
        Ok(t) => t,
        Err(e) => {
            error!("voice row: {e}: the row has no label / help texts");
            return;
        }
    };
    if let Some((m, _)) = host.provider("text_engine") {
        info!("voice row: texts through text_engine (mod {}, from this mod's text.toml)", m.id);
        return;
    }
    let mut served = 0;
    for lang in vr::LANGS {
        let key = vr::menu_text_path(lang);
        let Some((_, b)) = base_of(host, mods, &key) else {
            warn!("voice row: {key} not found: no texts in {lang}");
            continue;
        };
        match vr::patch_menu_text(&b, &vr::rows_for(&texts, lang)) {
            Ok(Some(bytes)) => {
                serve_generated(host, cache, &key, &bytes, "voice row: menu_text");
                served += 1;
            }
            Ok(None) => {}
            Err(e) => error!("voice row: {key}: {e}"),
        }
    }
    info!("voice row: texts added to menu_text in {served} language(s) (no text_engine)");
}

// ---------------------------------------------------------------- entry points

fn config(host: &Host) -> Cfg {
    let (c, e) = Cfg::from_text(&host.config_text());
    if let Some(e) = e {
        warn!("configuration invalid ({e}): defaults used");
    }
    c
}

/// `evt_plugin_early`: the audio build, the sheet and bgm_config, before any game code opens them. Never fails the
/// plugin.
fn early(host: &'static Host) -> Result<(), String> {
    let cfg = config(host);
    match host.phase() {
        evt_plugin_sdk::EVT_PHASE_EARLY => early_audio(host, &cfg),
        evt_plugin_sdk::EVT_PHASE_EARLY_LATE => {
            warn!("early phase run late (the game already runs): nothing can be served now; the mods' audio applies from the next start");
        }
        _ => {
            // a ModLoader without phase / file_serve: only the placeholder sheet (no boot-time audio build)
            if cfg.queue_sheet.merge {
                let late = thread_name().is_some_and(|n| n.starts_with("vr-loader"));
                queue_sheet_placeholder(host, late);
            }
            warn!("this ModLoader has no file_serve: the mods' [[voice]] / [[sfx]] / [[music]] only work when they ship built banks, and there is no «Pack de voces» row");
        }
    }
    Ok(())
}

/// `evt_plugin_init`: armed_voice. Err = plugin off, and then the ModLoader's built-in `armed_voice` keeps working
/// (it only yields to a plugin that loaded).
fn init(host: &'static Host) -> Result<(), String> {
    let cfg = config(host);
    if cfg.armed_voice.enabled {
        install_armed_voice(host).map_err(|e| format!("armed_voice: {e}"))?;
    } else {
        info!("armed_voice: off ([armed_voice] enabled = false): every armour plays the retail «armed» cue");
    }
    Ok(())
}

declare_plugin!(init = init, early = early);
