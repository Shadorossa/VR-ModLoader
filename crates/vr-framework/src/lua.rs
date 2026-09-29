//! Lua commands (`CMND_EVT_*`, module `lua_bridge` of the ModLoader) and values that cross the Lua boundary.
//!
//! * [`register_all`]: register a list of commands, get back the ones the loader refused (each engine logs them in
//!   its own words: the usual cause is `lua_bridge` off or a name already taken).
//! * [`valid_command`]: the naming rule `CMND_EVT_<ENGINE>_<VERB>` (upper case ASCII).
//! * Lua ↔ JSON: [`num_value`], [`str_value`] (with size limits) and [`push_json`].
//! * [`WarnOnce`]: a warning per distinct cause (a script calling a command wrongly every frame logs once).
//! * [`cstr`]: the C strings of an engine's DLL exports for other plugins.

use evt_plugin_sdk::{Host, LuaCall};
use std::collections::HashSet;
use std::ffi::{c_char, CStr};
use std::sync::Mutex;

/// A Lua command handler.
pub type Command = fn(&mut LuaCall);

/// Register `cmds`; returns `(name, loader code)` of every command that was not registered.
pub fn register_all(host: &Host, cmds: &[(&'static str, Command)]) -> Vec<(&'static str, i32)> {
    cmds.iter().filter_map(|(n, f)| host.lua_register(n, *f).err().map(|e| (*n, e))).collect()
}

/// `CMND_EVT_` + upper-case ASCII letters, digits and `_` (the prefix every mod command uses, so it never collides
/// with the game's own `CMND_*`).
pub fn valid_command(name: &str) -> bool {
    name.len() > 9 && name.starts_with("CMND_EVT_") && name.bytes().all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == b'_')
}

/// Longest key (bytes) a command accepts.
pub const MAX_KEY: usize = 128;
/// Longest string value (bytes) a command accepts.
pub const MAX_STR: usize = 64 * 1024;

/// A key: 1..=[`MAX_KEY`] bytes, no control characters.
pub fn valid_key(key: &str) -> bool {
    !key.is_empty() && key.len() <= MAX_KEY && !key.chars().any(char::is_control)
}

/// A Lua number as JSON (integral values as JSON integers). None = NaN / infinity.
pub fn num_value(v: f64) -> Option<serde_json::Value> {
    if !v.is_finite() {
        return None;
    }
    if v.fract() == 0.0 && v.abs() < 9_007_199_254_740_992.0 {
        return Some(serde_json::Value::from(v as i64));
    }
    serde_json::Number::from_f64(v).map(serde_json::Value::Number)
}

/// A Lua string as JSON. None = longer than [`MAX_STR`].
pub fn str_value(s: &str) -> Option<serde_json::Value> {
    (s.len() <= MAX_STR).then(|| serde_json::Value::String(s.to_string()))
}

/// Push a JSON value as one Lua result: bool, number, string; anything else as its JSON text.
pub fn push_json(c: &mut LuaCall, v: &serde_json::Value) {
    use serde_json::Value;
    match v {
        Value::Bool(b) => c.push_bool(*b),
        Value::Number(n) => c.push_num(n.as_f64().unwrap_or(0.0)),
        Value::String(s) => c.push_str(s),
        other => c.push_str(&other.to_string()),
    }
}

/// Distinct warnings, each once (at most `cap` remembered; past it nothing more is logged).
pub struct WarnOnce {
    seen: Mutex<Option<HashSet<String>>>,
    cap: usize,
}

impl WarnOnce {
    pub const fn new(cap: usize) -> WarnOnce {
        WarnOnce { seen: Mutex::new(None), cap }
    }

    /// True the first time `key` is seen (then log the warning).
    pub fn first(&self, key: &str) -> bool {
        let mut g = self.seen.lock().unwrap_or_else(|e| e.into_inner());
        let set = g.get_or_insert_with(HashSet::new);
        set.len() < self.cap && set.insert(key.to_string())
    }
}

/// A NUL-terminated UTF-8 argument of a C export (None = NULL or not UTF-8).
///
/// # Safety
/// `p` is NULL or points to a NUL-terminated string that lives for `'a`.
pub unsafe fn cstr<'a>(p: *const c_char) -> Option<&'a str> {
    (!p.is_null()).then(|| CStr::from_ptr(p).to_str().ok()).flatten()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn keys_values_names() {
        assert!(valid_key("progress.stage 3") && valid_key("ñ"));
        assert!(!valid_key("") && !valid_key("a\nb") && !valid_key(&"k".repeat(129)));
        assert_eq!(num_value(3.0), Some(json!(3)));
        assert_eq!(num_value(-2.5), Some(json!(-2.5)));
        assert_eq!(num_value(f64::NAN), None);
        assert_eq!(num_value(1e300), Some(json!(1e300)));
        assert!(str_value(&"x".repeat(MAX_STR + 1)).is_none());
        assert!(valid_command("CMND_EVT_TEXT_GET") && !valid_command("CMND_EVT_") && !valid_command("CMND_TEXT") && !valid_command("CMND_EVT_text"));
        let w = WarnOnce::new(2);
        assert!(w.first("a") && !w.first("a") && w.first("b") && !w.first("c"));
        assert_eq!(unsafe { cstr(c"hi".as_ptr()) }, Some("hi"));
        assert_eq!(unsafe { cstr(std::ptr::null()) }, None);
    }
}
