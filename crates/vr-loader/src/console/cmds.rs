//! Console commands: tokenizer, the command registry (built-ins + commands registered by plugins / other modules)
//! and the pure built-ins `help`, `log`, `config` (docs/app/modloader-consola.md §3). The Windows-only ones
//! (`status`, `lua`, `quit`, `close`, `clear`) are registered by [`super::rt`].
//!
//! **API hook point** (ModLoader plugins): [`register_command`] (Rust side). A handler gets the arguments after the
//! command name and returns the text to print (`Ok`) or an error (`Err`); it runs on the console input thread, never
//! on the game thread, so it must not call game functions that need the game thread. [`unregister_owner`] drops
//! every command of a plugin whose init failed.
//!
//! Names are English. [`register_alias`] adds a hidden alias (accepted by the dispatcher and `help <alias>`, never
//! listed): the Spanish names of the first versions (`cerrar`) stay usable that way.

use super::{level_name, parse_level, CategorySet};
use crate::log::Level;
use std::panic::AssertUnwindSafe;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};

pub type CmdResult = Result<String, String>;
pub type Handler = Arc<dyn Fn(&[String]) -> CmdResult + Send + Sync>;

#[derive(Clone)]
pub struct CmdEntry {
    pub name: String,
    /// `log <category> on|off` style usage line.
    pub usage: String,
    pub help: String,
    /// `loader` for the built-ins, the mod id for plugins.
    pub owner: String,
    handler: Handler,
}

/// Names no plugin can take (answered by the registry itself).
/// (`ayuda` = the old Spanish name of `help`.)
const RESERVED: &[&str] = &["help", "ayuda", "?"];

pub fn valid_name(n: &str) -> bool {
    !n.is_empty() && n.len() <= 32 && n.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_' || b == b'-' || b == b'.')
}

#[derive(Default)]
pub struct Registry {
    cmds: Vec<CmdEntry>,
    /// Hidden aliases: (alias, command name).
    aliases: Vec<(String, String)>,
}

impl Registry {
    pub const fn new() -> Self {
        Registry { cmds: Vec::new(), aliases: Vec::new() }
    }

    pub fn register(&mut self, name: &str, usage: &str, help: &str, owner: &str, handler: Handler) -> Result<(), String> {
        let n = name.trim().to_ascii_lowercase();
        if !valid_name(&n) {
            return Err(format!("invalid command name: `{name}` (a-z, 0-9, _ - .; up to 32)"));
        }
        if RESERVED.contains(&n.as_str()) {
            return Err(format!("command `{n}` is reserved"));
        }
        if let Some(e) = self.cmds.iter().find(|e| e.name == n) {
            return Err(format!("command `{n}` already exists (from {})", e.owner));
        }
        if self.aliases.iter().any(|(a, _)| *a == n) {
            return Err(format!("`{n}` is an alias of another command"));
        }
        let usage = if usage.trim().is_empty() { n.clone() } else { usage.trim().to_string() };
        self.cmds.push(CmdEntry { name: n, usage, help: help.trim().to_string(), owner: owner.to_string(), handler });
        self.cmds.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(())
    }

    /// A hidden alias for an existing command (not listed by `help`, not offered as a suggestion).
    pub fn alias(&mut self, alias: &str, target: &str) -> Result<(), String> {
        let a = alias.trim().to_ascii_lowercase();
        let t = target.trim().to_ascii_lowercase();
        if !valid_name(&a) {
            return Err(format!("invalid alias: `{alias}`"));
        }
        if RESERVED.contains(&a.as_str()) || self.cmds.iter().any(|e| e.name == a) || self.aliases.iter().any(|(x, _)| *x == a) {
            return Err(format!("alias `{a}` is already taken"));
        }
        if !self.cmds.iter().any(|e| e.name == t) {
            return Err(format!("alias `{a}`: no command `{t}`"));
        }
        self.aliases.push((a, t));
        Ok(())
    }

    /// Remove every command of `owner` (and the aliases that pointed to them); returns how many commands.
    pub fn unregister_owner(&mut self, owner: &str) -> usize {
        let n = self.cmds.len();
        self.cmds.retain(|e| e.owner != owner);
        let cmds = &self.cmds;
        self.aliases.retain(|(_, t)| cmds.iter().any(|e| e.name == *t));
        n - self.cmds.len()
    }

    /// The command called `name` (or aliased to it).
    pub fn get(&self, name: &str) -> Option<&CmdEntry> {
        let mut n = name.to_ascii_lowercase();
        if let Some((_, t)) = self.aliases.iter().find(|(a, _)| *a == n) {
            n = t.clone();
        }
        self.cmds.iter().find(|e| e.name == n)
    }

    pub fn names(&self) -> Vec<String> {
        self.cmds.iter().map(|e| e.name.clone()).collect()
    }

    /// `help` / `help <command>`.
    pub fn help_text(&self, topic: Option<&str>) -> CmdResult {
        if let Some(t) = topic {
            let e = self.get(t).ok_or_else(|| format!("no command `{t}` (type help)"))?;
            let owner = if e.owner == "loader" { String::new() } else { format!("  [{}]", e.owner) };
            return Ok(format!("{}{owner}\n  {}", e.usage, e.help.replace('\n', "\n  ")));
        }
        let w = self.cmds.iter().map(|e| e.usage.chars().count()).max().unwrap_or(0).max(10);
        let mut out = String::from("Commands (help <command> for details):\n");
        out.push_str(&format!("  {:<w$}  this help\n", "help [command]"));
        for e in &self.cmds {
            let first = e.help.lines().next().unwrap_or("");
            let owner = if e.owner == "loader" { String::new() } else { format!(" [{}]", e.owner) };
            out.push_str(&format!("  {:<w$}  {first}{owner}\n", e.usage));
        }
        Ok(out.trim_end().to_string())
    }
}

/// Split a command line: spaces separate, `"..."` groups (with `\"` inside).
pub fn tokenize(line: &str) -> Result<Vec<String>, String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut in_q = false;
    let mut have = false;
    let mut it = line.chars().peekable();
    while let Some(c) = it.next() {
        match c {
            '"' => {
                in_q = !in_q;
                have = true;
            }
            '\\' if in_q && it.peek() == Some(&'"') => {
                cur.push('"');
                it.next();
            }
            c if c.is_whitespace() && !in_q => {
                if have {
                    out.push(std::mem::take(&mut cur));
                    have = false;
                }
            }
            c => {
                cur.push(c);
                have = true;
            }
        }
    }
    if in_q {
        return Err("unclosed quote".into());
    }
    if have {
        out.push(cur);
    }
    Ok(out)
}

/// Run `line` with `reg`. The registry lock is released before the handler runs (a handler may call `help` or
/// register commands); a panicking handler is reported as an error.
pub fn execute_in(reg: &Mutex<Registry>, line: &str) -> CmdResult {
    let toks = tokenize(line)?;
    let Some(first) = toks.first() else { return Ok(String::new()) };
    let name = first.to_ascii_lowercase();
    if RESERVED.contains(&name.as_str()) {
        return reg.lock().unwrap_or_else(|e| e.into_inner()).help_text(toks.get(1).map(String::as_str));
    }
    let h = {
        let r = reg.lock().unwrap_or_else(|e| e.into_inner());
        match r.get(&name) {
            Some(e) => e.handler.clone(),
            None => {
                let near: Vec<String> = r.names().into_iter().filter(|n| n.starts_with(&name[..name.len().min(2)])).collect();
                return Err(if near.is_empty() {
                    format!("unknown command `{name}` (type help)")
                } else {
                    format!("unknown command `{name}`; maybe {}? (type help)", near.join(", "))
                });
            }
        }
    };
    let args = &toks[1..];
    match std::panic::catch_unwind(AssertUnwindSafe(|| h(args))) {
        Ok(r) => r,
        Err(_) => Err(format!("command `{name}` failed (panic)")),
    }
}

/// The console's command registry.
pub static REGISTRY: Mutex<Registry> = Mutex::new(Registry::new());

/// **API**: register a console command (built-in modules, ModLoader plugins through the plugin API).
pub fn register_command(
    name: &str,
    usage: &str,
    help: &str,
    owner: &str,
    f: impl Fn(&[String]) -> CmdResult + Send + Sync + 'static,
) -> Result<(), String> {
    REGISTRY.lock().unwrap_or_else(|e| e.into_inner()).register(name, usage, help, owner, Arc::new(f))
}

/// Hidden alias for a built-in command (see [`Registry::alias`]).
pub fn register_alias(alias: &str, target: &str) -> Result<(), String> {
    REGISTRY.lock().unwrap_or_else(|e| e.into_inner()).alias(alias, target)
}

/// **API**: drop every command of `owner` (a plugin whose init failed).
pub fn unregister_owner(owner: &str) -> usize {
    REGISTRY.lock().unwrap_or_else(|e| e.into_inner()).unregister_owner(owner)
}

/// Run a command line with the global registry.
pub fn execute(line: &str) -> CmdResult {
    execute_in(&REGISTRY, line)
}

// ---------------------------------------------------------------- built-ins: log / config

/// What the built-ins change: the category set and the run-time switches (globals in the game, locals in tests).
pub struct RtState<'a> {
    pub cats: &'a CategorySet,
    pub colors: &'a AtomicBool,
    pub file_hits: &'a AtomicBool,
    pub rate_limit: &'a AtomicU32,
    pub get_level: &'a (dyn Fn() -> Level + Sync),
    pub set_level: &'a (dyn Fn(Level) + Sync),
}

pub fn parse_bool(s: &str) -> Option<bool> {
    match s.trim().to_ascii_lowercase().as_str() {
        "on" | "yes" | "true" | "1" | "si" | "sí" => Some(true),
        "off" | "no" | "false" | "0" => Some(false),
        _ => None,
    }
}

/// `log`, `log <category|all> on|off`, `log level [<level>]` (`todas` / `nivel`: the old Spanish names).
pub fn cmd_log(args: &[String], st: &RtState) -> CmdResult {
    let a: Vec<String> = args.iter().map(|s| s.to_ascii_lowercase()).collect();
    match a.as_slice() {
        [] => {
            let mut out = format!("Log level: {}\nCategories:\n", level_name((st.get_level)()));
            for (_, n, d, on) in st.cats.list() {
                out.push_str(&format!("  {:<10} {:<3}  {d}\n", n, if on { "on" } else { "off" }));
            }
            out.push_str("(log <category|all> on|off, log level <error|warn|info|debug|trace>)");
            Ok(out)
        }
        [l] if l == "level" || l == "nivel" => Ok(format!("log level: {}", level_name((st.get_level)()))),
        [l, v] if l == "level" || l == "nivel" => {
            let lv = parse_level(v).ok_or_else(|| format!("unknown level `{v}` (error, warn, info, debug, trace)"))?;
            (st.set_level)(lv);
            Ok(format!("log level: {} (also in loader.log; categories are written as info)", level_name(lv)))
        }
        [c, v] => {
            let on = parse_bool(v).ok_or_else(|| format!("`{v}`: use on or off"))?;
            if c == "todas" || c == "all" {
                for (id, ..) in st.cats.list() {
                    st.cats.set(id, on);
                }
                return Ok(format!("all categories: {}", if on { "on" } else { "off" }));
            }
            let id = st.cats.id(c).ok_or_else(|| {
                let names: Vec<String> = st.cats.list().into_iter().map(|x| x.1).collect();
                format!("unknown category `{c}` (there are: {})", names.join(", "))
            })?;
            st.cats.set(id, on);
            Ok(format!("{c}: {}", if on { "on" } else { "off" }))
        }
        _ => Err("usage: log | log <category|all> on|off | log level <level>".into()),
    }
}

/// Keys `config set` can change while the game runs: (key, description).
pub const RUNTIME_KEYS: &[(&str, &str)] = &[
    ("loader.log_level", "log level (error, warn, info, debug, trace)"),
    ("console.categories", "active categories, e.g. states,menus (replaces the list)"),
    ("console.colors", "console colours (on/off)"),
    ("console.file_hits", "files: also the ones that open fine (on/off)"),
    ("console.rate_limit", "lines per second and category (0 = no limit)"),
];

/// Value of `a.b.c` in a TOML value.
pub fn get_path<'v>(v: &'v toml::Value, path: &str) -> Option<&'v toml::Value> {
    let mut cur = v;
    for part in path.split('.') {
        if part.is_empty() {
            return None;
        }
        cur = cur.as_table()?.get(part)?;
    }
    Some(cur)
}

fn show_value(v: &toml::Value) -> String {
    match v {
        toml::Value::Table(t) => toml::to_string(t).unwrap_or_default().trim_end().to_string(),
        other => other.to_string(),
    }
}

/// Category names from `a,b`, `a b` or `["a", "b"]`.
pub fn parse_list(parts: &[String]) -> Vec<String> {
    parts
        .join(" ")
        .split(|c: char| c == ',' || c.is_whitespace() || c == '[' || c == ']' || c == '"')
        .filter(|s| !s.is_empty())
        .map(|s| s.to_ascii_lowercase())
        .collect()
}

fn runtime_value(key: &str, st: &RtState) -> Option<String> {
    Some(match key {
        "loader.log_level" => format!("\"{}\"", level_name((st.get_level)())),
        "console.categories" => format!("{:?}", st.cats.on_names()),
        "console.colors" => st.colors.load(Ordering::Relaxed).to_string(),
        "console.file_hits" => st.file_hits.load(Ordering::Relaxed).to_string(),
        "console.rate_limit" => st.rate_limit.load(Ordering::Relaxed).to_string(),
        _ => return None,
    })
}

/// `config`, `config keys`, `config get <section.key>`, `config set <section.key> <value>` (`claves`: the old Spanish name of `keys`). `startup` = the
/// configuration read at start (config.toml + defaults).
pub fn cmd_config(args: &[String], startup: Option<&toml::Value>, st: &RtState) -> CmdResult {
    let sub = args.first().map(|s| s.to_ascii_lowercase());
    match sub.as_deref() {
        None | Some("keys") | Some("claves") => {
            let mut out = String::from("Keys that can be changed while the game runs (config set):\n");
            for (k, d) in RUNTIME_KEYS {
                out.push_str(&format!("  {k:<20} {d}\n"));
            }
            out.push_str("The rest is read with config get and changed in evt_loader\\config.toml (restart the game).\n");
            out.push_str("config set changes are not saved to config.toml.");
            Ok(out)
        }
        Some("get") => {
            let key = args.get(1).ok_or("usage: config get <section.key>")?.to_ascii_lowercase();
            if let Some(v) = runtime_value(&key, st) {
                return Ok(format!("{key} = {v}"));
            }
            let root = startup.ok_or("configuration not available")?;
            let v = get_path(root, &key).ok_or_else(|| format!("no key `{key}`"))?;
            if v.is_table() {
                Ok(format!("[{key}] (startup values)\n{}", show_value(v)))
            } else {
                Ok(format!("{key} = {} (startup value)", show_value(v)))
            }
        }
        Some("set") => {
            let key = args.get(1).ok_or("usage: config set <section.key> <value>")?.to_ascii_lowercase();
            let rest = &args[2.min(args.len())..];
            if rest.is_empty() {
                return Err("missing value".into());
            }
            let one = rest.join(" ");
            match key.as_str() {
                "loader.log_level" => {
                    let lv = parse_level(one.trim_matches('"')).ok_or_else(|| format!("unknown level `{one}`"))?;
                    (st.set_level)(lv);
                }
                "console.categories" => {
                    let names = parse_list(rest);
                    let unknown = st.cats.apply(&names);
                    if !unknown.is_empty() {
                        return Ok(format!(
                            "{key} = {:?} (unknown, ignored: {})",
                            st.cats.on_names(),
                            unknown.join(", ")
                        ));
                    }
                }
                "console.colors" => st.colors.store(parse_bool(&one).ok_or("use on or off")?, Ordering::Relaxed),
                "console.file_hits" => st.file_hits.store(parse_bool(&one).ok_or("use on or off")?, Ordering::Relaxed),
                "console.rate_limit" => {
                    st.rate_limit.store(one.trim().parse::<u32>().map_err(|_| format!("`{one}` is not a number"))?, Ordering::Relaxed)
                }
                _ => {
                    return Err(if startup.and_then(|r| get_path(r, &key)).is_some() {
                        format!("`{key}` cannot be changed while running: edit evt_loader\\config.toml and restart (config keys)")
                    } else {
                        format!("no key `{key}` (config keys)")
                    })
                }
            }
            Ok(format!("{key} = {}", runtime_value(&key, st).unwrap_or_default()))
        }
        Some(o) => Err(format!("`config {o}`: use config keys | get <key> | set <key> <value>")),
    }
}

/// The global state the built-ins act on.
fn global_state<'a>(get: &'a (dyn Fn() -> Level + Sync), set: &'a (dyn Fn(Level) + Sync)) -> RtState<'a> {
    RtState {
        cats: &super::CATS,
        colors: &super::COLORS,
        file_hits: &super::FILE_HITS,
        rate_limit: &super::RATE_LIMIT,
        get_level: get,
        set_level: set,
    }
}

fn get_level() -> Level {
    crate::log::level()
}
fn set_level(l: Level) {
    crate::log::set_level(l)
}

/// Register `log` and `config` (`startup` = the configuration read at start as TOML).
pub fn register_builtins(startup: fn() -> Option<toml::Value>) {
    let _ = register_command(
        "log",
        "log [<category|all> on|off | level <level>]",
        "log categories (no arguments: list and state)\nlog states on / log files off / log all off\nlog level debug: level of loader.log",
        "loader",
        |a| cmd_log(a, &global_state(&get_level, &set_level)),
    );
    let _ = register_command(
        "config",
        "config [keys | get <key> | set <key> <value>]",
        "reads the configuration (get section.key) and changes the config keys listed by `config keys` while running\nconfig get quit_fix.grace_seconds / config set console.categories states,menus",
        "loader",
        move |a| {
            let v = startup();
            cmd_config(a, v.as_ref(), &global_state(&get_level, &set_level))
        },
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicU8;

    #[test]
    fn tokenizer() {
        assert_eq!(tokenize("  log  states   on ").unwrap(), vec!["log", "states", "on"]);
        assert_eq!(tokenize(r#"say "hello world" x"#).unwrap(), vec!["say", "hello world", "x"]);
        assert_eq!(tokenize(r#"a "" b"#).unwrap(), vec!["a", "", "b"]);
        assert_eq!(tokenize(r#"a "say \"hi\"""#).unwrap(), vec!["a", "say \"hi\""]);
        assert!(tokenize(r#"a "unclosed"#).is_err());
        assert!(tokenize("   ").unwrap().is_empty());
    }

    #[test]
    fn registry_dispatch() {
        let reg = Mutex::new(Registry::new());
        {
            let mut r = reg.lock().unwrap();
            r.register("echo", "echo <text>", "repeats", "loader", Arc::new(|a: &[String]| Ok(a.join(" ")))).unwrap();
            r.register("Mod.Hello", "", "greets\nsecond line", "mod_a", Arc::new(|_: &[String]| Ok("hello".into()))).unwrap();
            r.register("broken", "", "", "mod_a", Arc::new(|_: &[String]| panic!("x"))).unwrap();
            assert!(r.register("echo", "", "", "mod_b", Arc::new(|_: &[String]| Ok(String::new()))).is_err(), "duplicate");
            assert!(r.register("help", "", "", "mod_b", Arc::new(|_: &[String]| Ok(String::new()))).is_err(), "reserved");
            assert!(r.register("bad name", "", "", "mod_b", Arc::new(|_: &[String]| Ok(String::new()))).is_err());
        }
        assert_eq!(execute_in(&reg, "echo a  b").unwrap(), "a b");
        assert_eq!(execute_in(&reg, "MOD.HELLO").unwrap(), "hello");
        assert!(execute_in(&reg, "broken").unwrap_err().contains("panic"));
        assert!(execute_in(&reg, "ec").unwrap_err().contains("maybe echo?"));
        assert!(execute_in(&reg, "zzz").unwrap_err().contains("unknown command"));
        assert_eq!(execute_in(&reg, "").unwrap(), "");
        let h = execute_in(&reg, "help").unwrap();
        assert!(h.contains("echo <text>") && h.contains("greets [mod_a]") && !h.contains("second"), "{h}");
        let h1 = execute_in(&reg, "help mod.hello").unwrap();
        assert!(h1.contains("second line") && h1.contains("[mod_a]"), "{h1}");
        assert!(execute_in(&reg, "help nothing").is_err());
        // a handler may use the registry itself (the lock is not held while it runs)
        static REG2: Mutex<Registry> = Mutex::new(Registry::new());
        REG2.lock()
            .unwrap()
            .register("helpme", "", "", "loader", Arc::new(|_: &[String]| execute_in(&REG2, "help")))
            .unwrap();
        assert!(execute_in(&REG2, "helpme").unwrap().contains("helpme"));
        // hidden aliases: dispatched, never listed or suggested
        {
            let mut r = reg.lock().unwrap();
            r.alias("repite", "echo").unwrap();
            r.alias("Saluda", "mod.hello").unwrap();
            assert!(r.alias("echo", "echo").is_err(), "taken by a command");
            assert!(r.alias("repite", "echo").is_err(), "taken by an alias");
            assert!(r.alias("help", "echo").is_err(), "reserved");
            assert!(r.alias("nuevo", "nada").is_err(), "unknown target");
            assert!(r.register("saluda", "", "", "mod_b", Arc::new(|_: &[String]| Ok(String::new()))).is_err(), "a command cannot take an alias name");
        }
        assert_eq!(execute_in(&reg, "REPITE x y").unwrap(), "x y");
        assert_eq!(execute_in(&reg, "saluda").unwrap(), "hello");
        let h = execute_in(&reg, "help").unwrap();
        assert!(!h.contains("repite") && !h.contains("saluda"), "{h}");
        assert!(execute_in(&reg, "help repite").unwrap().contains("echo <text>"));
        assert!(!execute_in(&reg, "zz").unwrap_err().contains("repite"));
        assert!(execute_in(&reg, "re").unwrap_err().contains("unknown command"), "aliases are not suggestions");
        // a failed plugin leaves (with its aliases)
        assert_eq!(reg.lock().unwrap().unregister_owner("mod_a"), 2);
        assert!(execute_in(&reg, "mod.hello").is_err());
        assert!(execute_in(&reg, "saluda").is_err());
        assert_eq!(execute_in(&reg, "repite z").unwrap(), "z");
    }

    static LVL: AtomicU8 = AtomicU8::new(2);
    fn lvl_get() -> Level {
        match LVL.load(Ordering::Relaxed) {
            0 => Level::Error,
            1 => Level::Warn,
            2 => Level::Info,
            3 => Level::Debug,
            _ => Level::Trace,
        }
    }
    fn lvl_set(l: Level) {
        LVL.store(l as u8, Ordering::Relaxed)
    }

    fn s(v: &[&str]) -> Vec<String> {
        v.iter().map(|x| x.to_string()).collect()
    }

    #[test]
    fn log_command() {
        let cats = CategorySet::new();
        let (c, f, r) = (AtomicBool::new(true), AtomicBool::new(true), AtomicU32::new(200));
        let st = RtState { cats: &cats, colors: &c, file_hits: &f, rate_limit: &r, get_level: &lvl_get, set_level: &lvl_set };
        let list = cmd_log(&[], &st).unwrap();
        assert!(list.contains("states") && list.contains("loader     on") && !list.contains("estados"), "{list}");
        assert_eq!(cmd_log(&s(&["states", "on"]), &st).unwrap(), "states: on");
        assert!(cats.enabled(super::super::CAT_STATES));
        cmd_log(&s(&["Files", "YES"]), &st).unwrap();
        assert!(cats.enabled(super::super::CAT_FILES));
        // the old Spanish names still work
        assert_eq!(cmd_log(&s(&["ficheros", "off"]), &st).unwrap(), "ficheros: off");
        assert!(!cats.enabled(super::super::CAT_FILES));
        cmd_log(&s(&["sonido", "sí"]), &st).unwrap();
        assert!(cats.enabled(super::super::CAT_SOUND));
        cmd_log(&s(&["all", "on"]), &st).unwrap();
        assert_eq!(cats.mask() & 0b111111, 0b111111);
        assert!(cmd_log(&s(&["nothing", "on"]), &st).unwrap_err().contains("there are: loader"));
        assert!(cmd_log(&s(&["menus", "maybe"]), &st).is_err());
        cmd_log(&s(&["todas", "off"]), &st).unwrap();
        assert_eq!(cats.mask(), 0);
        cmd_log(&s(&["level", "debug"]), &st).unwrap();
        assert_eq!(lvl_get(), Level::Debug);
        assert!(cmd_log(&s(&["level", "loads"]), &st).is_err());
        cmd_log(&s(&["nivel", "info"]), &st).unwrap();
        assert_eq!(lvl_get(), Level::Info);
        cmd_log(&s(&["level", "debug"]), &st).unwrap();
        assert!(cmd_log(&s(&["level"]), &st).unwrap().contains("debug"));
        lvl_set(Level::Info);
    }

    #[test]
    fn config_command() {
        let cats = CategorySet::new();
        let (c, f, r) = (AtomicBool::new(true), AtomicBool::new(true), AtomicU32::new(200));
        let st = RtState { cats: &cats, colors: &c, file_hits: &f, rate_limit: &r, get_level: &lvl_get, set_level: &lvl_set };
        let startup = toml::Value::try_from(crate::config::Config::default()).unwrap();
        let root = Some(&startup);
        assert!(cmd_config(&[], root, &st).unwrap().contains("console.categories"));
        assert_eq!(
            cmd_config(&s(&["get", "quit_fix.grace_seconds"]), root, &st).unwrap(),
            "quit_fix.grace_seconds = 5 (startup value)"
        );
        assert!(cmd_config(&s(&["get", "quit_fix"]), root, &st).unwrap().contains("retail_quit = true"));
        assert!(cmd_config(&s(&["get", "no.existe"]), root, &st).is_err());
        assert!(cmd_config(&s(&["set", "quit_fix.grace_seconds", "9"]), root, &st).unwrap_err().contains("cannot be changed while running"));
        assert!(cmd_config(&s(&["set", "zz.yy", "1"]), root, &st).unwrap_err().contains("no key"));
        // run-time keys
        assert_eq!(
            cmd_config(&s(&["set", "console.categories", "states,", "menus"]), root, &st).unwrap(),
            "console.categories = [\"states\", \"menus\"]"
        );
        assert!(cats.enabled(super::super::CAT_MENUS) && !cats.enabled(super::super::CAT_LOADER));
        assert!(cmd_config(&s(&["set", "console.categories", "[\"loader\",\"zzz\"]"]), root, &st).unwrap().contains("ignored: zzz"));
        cmd_config(&s(&["set", "console.colors", "off"]), root, &st).unwrap();
        assert!(!c.load(Ordering::Relaxed));
        cmd_config(&s(&["set", "console.rate_limit", "50"]), root, &st).unwrap();
        assert_eq!(r.load(Ordering::Relaxed), 50);
        assert!(cmd_config(&s(&["set", "console.rate_limit", "x"]), root, &st).is_err());
        assert_eq!(cmd_config(&s(&["get", "console.file_hits"]), root, &st).unwrap(), "console.file_hits = true");
        cmd_config(&s(&["set", "loader.log_level", "\"warn\""]), root, &st).unwrap();
        assert_eq!(lvl_get(), Level::Warn);
        assert_eq!(cmd_config(&s(&["get", "loader.log_level"]), root, &st).unwrap(), "loader.log_level = \"warn\"");
        lvl_set(Level::Info);
        assert!(cmd_config(&s(&["set", "console.colors"]), root, &st).is_err());
        assert!(cmd_config(&s(&["delete"]), root, &st).is_err());
        assert!(cmd_config(&s(&["claves"]), root, &st).unwrap().contains("console.categories"), "old alias of keys");
    }

    #[test]
    fn helpers() {
        assert_eq!(parse_list(&s(&["[\"a\",", "\"B\"]"])), vec!["a", "b"]);
        assert_eq!(parse_bool("Yes"), Some(true));
        assert_eq!(parse_bool("OFF"), Some(false));
        assert_eq!(parse_bool("Sí"), Some(true), "old Spanish spelling");
        assert_eq!(parse_bool("x"), None);
        let v: toml::Value = toml::from_str("[a]\nb = 1\n").unwrap();
        assert_eq!(get_path(&v, "a.b").and_then(|x| x.as_integer()), Some(1));
        assert!(get_path(&v, "a..b").is_none());
    }
}
