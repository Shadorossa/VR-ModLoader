//! Run time of plugin `match_engine`, all through the ModLoader API (docs/app/modloader-plugins.md). The loader prefixes
//! every log line with `match_engine: `, so the lines read as with the old built-in module.
//!
//! Init (loader init thread): configuration, rulesets + assignment tables of every mod that uses the engine, the
//! globals, the hooks (`TeamBuild` read-only, `TeamRecordBuild` setup knobs + probe, `NextHalf`, the InPlay update
//! slot) and the Lua commands (`CMND_EVT_MATCH_ENGINE_*`, `CMND_EVT_MATCH_RULES_*`).

mod end;

mod rules;
mod setup;

use crate::assign::{self, MatchKeys, Table};
use crate::ruleset::{self, Ruleset};
use crate::sigs::{self as ms, Rip, Sig};
use crate::MatchEngineCfg;
use evt_plugin_sdk::{declare_plugin, host, Host, Level};
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Mutex, OnceLock};

pub(crate) fn log(level: Level, msg: &str) {
    host().log(level, msg);
}
macro_rules! info { ($($a:tt)*) => { $crate::rt::log(evt_plugin_sdk::Level::Info, &format!($($a)*)) }; }
macro_rules! warning { ($($a:tt)*) => { $crate::rt::log(evt_plugin_sdk::Level::Warn, &format!($($a)*)) }; }
macro_rules! error { ($($a:tt)*) => { $crate::rt::log(evt_plugin_sdk::Level::Error, &format!($($a)*)) }; }
macro_rules! debug { ($($a:tt)*) => { $crate::rt::log(evt_plugin_sdk::Level::Debug, &format!($($a)*)) }; }
pub(crate) use {debug, error, info, warning};

pub(crate) fn read<T: Copy>(a: usize) -> Option<T> {
    host().read::<T>(a)
}
pub(crate) fn read_ptr(a: usize) -> Option<usize> {
    host().read_ptr(a)
}
pub(crate) fn write<T: Copy>(a: usize, v: T) -> bool {
    host().write::<T>(a, v)
}

/// Addresses of the globals (0 = unresolved).
pub(crate) static G_ROOT: AtomicUsize = AtomicUsize::new(0);
pub(crate) static NET_VAR: AtomicUsize = AtomicUsize::new(0);

pub(crate) static CFG: OnceLock<MatchEngineCfg> = OnceLock::new();
pub(crate) static RULESETS: OnceLock<Vec<Ruleset>> = OnceLock::new();
pub(crate) static TABLE: OnceLock<Table> = OnceLock::new();

/// What a mode asked for the next match (`CMND_EVT_MATCH_ENGINE_SELECT`).
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Pending {
    Ruleset(String),
    /// `"retail"`: no ruleset, the row's rules.
    Retail,
}
pub(crate) static PENDING: Mutex<Option<Pending>> = Mutex::new(None);

pub(crate) fn cfg() -> &'static MatchEngineCfg {
    CFG.get_or_init(MatchEngineCfg::default)
}

pub(crate) fn rulesets() -> &'static [Ruleset] {
    RULESETS.get().map_or(&[], |v| v.as_slice())
}

pub(crate) fn find_ruleset(id: &str) -> Option<Ruleset> {
    rulesets().iter().find(|r| r.id == id).cloned()
}

/// `[[g_gameRoot]+off]`.
pub(crate) fn root_ptr(off: usize) -> Option<usize> {
    let root = read_ptr(G_ROOT.load(Ordering::Acquire))?;
    read_ptr(root + off)
}

/// The live match info (`[[g_gameRoot]+0x6A58]`).
pub(crate) fn mi() -> Option<usize> {
    root_ptr(ms::OFF_MATCH)
}

/// Offline and not an observer (unknown = not offline) for the match info at `mi` (live or being built).
pub(crate) fn offline(mi: usize) -> bool {
    let net = read_ptr(NET_VAR.load(Ordering::Acquire)).and_then(|g| ms::network_active(g, read::<u8>, read::<u32>));
    net == Some(false) && read::<u8>(mi + ms::MI_OBSERVER) == Some(0)
}

/// Play mode byte (None = unreadable).
pub(crate) fn play_mode() -> Option<u8> {
    root_ptr(ms::ROOT_PLAYDATA).and_then(|p| read::<u8>(p + ms::PLAYDATA_MODE))
}

/// The ruleset a match gets and why. `mi` = the match info (being built or live), `rival_team` from `TeamBuild`.
pub(crate) struct Decision {
    pub keys: MatchKeys,
    pub rules: Option<Ruleset>,
    pub source: String,
}

/// The whole choice (docs/game/modes/match-engine.md §12.3): Lua SELECT > test_ruleset > the assignment tables.
pub(crate) fn decide(mi: usize, rival_team: Option<u32>) -> Decision {
    let ty = read::<u8>(mi + ms::MI_TYPE).unwrap_or(0);
    let bits = read::<u32>(mi + ms::MI_RULES).unwrap_or(0);
    let keys = MatchKeys {
        game: read::<u32>(mi + ms::MI_GAME).unwrap_or(0),
        orig_game: read::<u32>(mi + ms::MI_ORIG_GAME).unwrap_or(0),
        rival_team,
        mode: assign::classify(ty, play_mode()),
    };
    let pending = PENDING.lock().unwrap_or_else(|e| e.into_inner()).take();
    let d = |rules: Option<Ruleset>, source: String| Decision { keys: keys.clone(), rules, source };
    if !offline(mi) {
        if let Some(p) = pending {
            warning!("selection {p:?} dropped: the match is online or observed (its rules come from the server)");
        }
        return d(None, "online / observer: untouched".into());
    }
    match pending {
        Some(Pending::Retail) => return d(None, "Lua SELECT(\"retail\"): the row's rules".into()),
        Some(Pending::Ruleset(id)) => match find_ruleset(&id) {
            Some(r) => return d(Some(r), "Lua CMND_EVT_MATCH_ENGINE_SELECT".into()),
            None => warning!("selected ruleset \"{id}\" no longer loaded: the assignment tables decide"),
        },
        None => {}
    }
    if let Some(id) = cfg().test_ruleset() {
        if crate::test_ruleset_eligible(ty, bits) {
            match find_ruleset(id) {
                Some(r) => return d(Some(r), "config test_ruleset".into()),
                None => warning!("test_ruleset \"{id}\" not loaded: the assignment tables decide"),
            }
        } else {
            info!("test_ruleset not applied (match type {ty}, story V-goal {})", bits & crate::BIT_EXTENDED_VGOAL != 0);
        }
    }
    let Some(t) = TABLE.get() else { return d(None, "no assignment table: the row's rules".into()) };
    for (e, lvl) in t.resolve_all(&keys) {
        if e.ruleset.eq_ignore_ascii_case("retail") {
            return d(None, format!("{} {} (mod {}): \"retail\" = the row's rules", lvl.as_str(), what(&keys, lvl), e.mod_id));
        }
        match find_ruleset(&e.ruleset) {
            Some(r) => return d(Some(r), format!("{} {} (mod {})", lvl.as_str(), what(&keys, lvl), e.mod_id)),
            None => warning!(
                "{} {} of mod {} names ruleset \"{}\", not loaded: next level",
                lvl.as_str(),
                what(&keys, lvl),
                e.mod_id,
                e.ruleset
            ),
        }
    }
    d(None, format!("no assignment for mode {}: the row's rules", keys.mode))
}

fn what(k: &MatchKeys, lvl: assign::Level) -> String {
    match lvl {
        assign::Level::Game | assign::Level::ModDefault => format!("0x{:08X}", k.game),
        assign::Level::Team => format!("0x{:08X}", k.rival_team.unwrap_or(0)),
        assign::Level::Mode | assign::Level::BaseMode => k.mode.clone(),
    }
}

// ---------------------------------------------------------------- init

fn resolve(host: &Host, s: &Sig) -> Option<usize> {
    let r = host.sig(s.name, s.pattern, s.rva);
    if r.is_none() {
        error!("{} not found", s.name);
    }
    r
}

pub(crate) fn resolve_rip(host: &Host, r: &Rip) -> Option<usize> {
    let insn = host.sig(r.sig.name, r.sig.pattern, r.sig.rva)?;
    let v = host.rip(insn, r.disp_off, r.next_ip_off);
    if v.is_none() {
        error!("{} not resolved", r.name);
    }
    v
}

/// Inline hook through the loader (the stolen bytes = the pattern's fixed prefix).
pub(crate) fn hook(host: &Host, s: &Sig, steal: usize, detour: *const (), next: &'static AtomicUsize) -> Option<usize> {
    let target = resolve(host, s)?;
    let Some(pro) = crate::fixed_prefix(s.pattern, steal) else {
        error!("{}: stolen bytes have wildcards", s.name);
        return None;
    };
    match unsafe { host.hook_inline(target, &pro, detour, next, 0) } {
        Ok(()) => Some(target),
        Err(e) => {
            error!("{} hook refused (code {e})", s.name);
            None
        }
    }
}

/// Folders `(mod id, <mod>\rules)` of every active mod that uses the engine, in load order, then the legacy folder.
fn rule_sources(host: &Host, cfg: &MatchEngineCfg) -> Vec<(String, PathBuf)> {
    let mut v = Vec::new();
    for m in host.mods() {
        let manifest = std::fs::read_to_string(m.dir.join("mod.toml")).unwrap_or_default();
        if assign::uses_match_engine(&host.mod_id, &m.id, &manifest) {
            v.push((m.id.clone(), m.dir.join(crate::RULES_DIR)));
        }
    }
    if !v.iter().any(|(id, _)| *id == host.mod_id) {
        v.insert(0, (host.mod_id.clone(), host.mod_dir.join(crate::RULES_DIR)));
    }
    if cfg.legacy_folder {
        if let Some(l) = host.path("loader_dir") {
            v.push((String::new(), l.join(crate::LEGACY_DIR)));
        }
    }
    v
}

fn load_rules(host: &Host, cfg: &MatchEngineCfg) {
    let sources = rule_sources(host, cfg);
    let (rs, msgs) = ruleset::load_sources(&sources);
    for m in &msgs {
        warning!("ruleset {m}");
    }
    for r in &rs {
        info!(
            "ruleset \"{}\" ({}, {}): {}",
            r.id,
            r.name,
            if r.source_mod.is_empty() { "evt_loader\\match_engine".to_string() } else { format!("mod {}", r.source_mod) },
            r.summary()
        );
    }
    let mut t = Table::new(&host.mod_id);
    for (m, dir) in sources.iter().filter(|(m, _)| !m.is_empty()) {
        let f = dir.join(crate::MATCHES_FILE);
        if let Ok(text) = std::fs::read_to_string(&f) {
            for p in t.add(m, &text) {
                warning!("{p}");
            }
        }
    }
    for (e, what) in t.referenced() {
        if !e.ruleset.eq_ignore_ascii_case("retail") && !rs.iter().any(|r| r.id == e.ruleset) {
            warning!("matches.toml of mod {}: {what} -> ruleset \"{}\" is not loaded (those matches fall back to the next level)", e.mod_id, e.ruleset);
        }
    }
    let folders: Vec<String> = sources.iter().map(|(m, _)| if m.is_empty() { "legacy".into() } else { m.clone() }).collect();
    info!("{} ruleset(s) loaded from [{}]; assignment table: {}", rs.len(), folders.join(", "), t.summary());
    if let Some(tr) = cfg.test_ruleset() {
        if rs.iter().any(|r| r.id == tr) {
            info!("test_ruleset \"{tr}\" applies to every offline full / small match without a Lua selection (testing)");
        } else {
            warning!("test_ruleset \"{tr}\" is not among the loaded rulesets: off");
        }
    }
    let _ = RULESETS.set(rs);
    let _ = TABLE.set(t);
}

fn init(host: &'static Host) -> Result<(), String> {
    let t = host.config_text();
    let cfg = match toml::from_str::<MatchEngineCfg>(&t) {
        Ok(c) => c,
        Err(e) => {
            warning!("config not valid ({}): defaults", e.to_string().trim());
            MatchEngineCfg::default()
        }
    };
    for p in cfg.problems() {
        warning!("config: {p}");
    }
    let cfg = CFG.get_or_init(|| cfg).clone();
    let root = resolve_rip(host, &ms::G_GAME_ROOT).ok_or("g_gameRoot not resolved: plugin off (the built-in modules run if enabled)")?;
    G_ROOT.store(root, Ordering::Release);
    // network predicate (same check as the loader's prematch; the function has an identical twin, so by RVA)
    let net_fn = host.exe_base() + ms::NET_FN_RVA;
    match host.code_clean(net_fn, 7 + ms::NET_FN_TAIL.len()) {
        Some(b) if b[..3] == ms::NET_FN_HEAD && b[7..] == ms::NET_FN_TAIL => {
            let d = i32::from_le_bytes([b[3], b[4], b[5], b[6]]);
            NET_VAR.store((net_fn as i64 + 7 + d as i64) as usize, Ordering::Release);
        }
        _ => warning!("network predicate not the v7.1.2 bytes: every match counts as online (rulesets never apply)"),
    }
    load_rules(host, &cfg);
    // Opciones half length (was the built-in module match_rules)
    if cfg.match_rules {
        rules::init(host);
    }
    // setup: TeamBuild (rival team id) + TeamRecordBuild (knobs + probe)
    setup::install(host, &cfg);
    // end of match: NextHalf + InPlay slot
    let end_on = end::install(host);
    end::register(host);
    info!(
        "match engine {} on: setup knobs {}, end rules {}, CMND_EVT_MATCH_ENGINE_* registered",
        host.mod_version,
        if cfg.setup && setup::hooked() { "on" } else { "off" },
        if end_on { "on" } else { "off" }
    );
    Ok(())
}

declare_plugin!(init = init);
