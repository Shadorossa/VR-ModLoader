//! **Generic** helpers (candidates for the shared `vr-framework` crate, docs/app/modloader-roadmap.md «VR-Framework»):
//! nothing here knows about saves or slots.
//!
//! * [`write_atomic`]: tmp + flush + rename;
//! * mod ids / keys usable as file names and map keys ([`valid_mod_id`], [`valid_key`]);
//! * Lua values ↔ JSON ([`num_value`], [`str_value`]) with size limits;
//! * reading a loader event ring through `game_state` ([`ring_pending`]).

use std::io::Write;
use std::path::{Path, PathBuf};

/// Write `bytes` to `path` atomically (parent folders created): `<path>.tmp`, flushed to disk, renamed over the old
/// file (Windows: `MoveFileExW(REPLACE_EXISTING)`). A crash leaves either the old or the new file, never half of one.
pub fn write_atomic(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    if let Some(d) = path.parent() {
        std::fs::create_dir_all(d)?;
    }
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(".tmp");
    let tmp = PathBuf::from(tmp);
    {
        let mut f = std::fs::File::create(&tmp)?;
        f.write_all(bytes)?;
        f.sync_all()?;
    }
    std::fs::rename(&tmp, path).inspect_err(|_| {
        let _ = std::fs::remove_file(&tmp);
    })
}

pub fn unix_now() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

/// Longest key (bytes).
pub const MAX_KEY: usize = 128;
/// Longest string value (bytes).
pub const MAX_STR: usize = 64 * 1024;
/// Most keys per mod and scope.
pub const MAX_KEYS: usize = 4096;

/// A mod id usable as a file name: `[a-z0-9_][a-z0-9_.-]{0,63}` (the mod.toml rule, plus a leading `_` for
/// `_local`).
pub fn valid_mod_id(id: &str) -> bool {
    let b = id.as_bytes();
    !b.is_empty()
        && b.len() <= 64
        && (b[0].is_ascii_lowercase() || b[0].is_ascii_digit() || b[0] == b'_')
        && b.iter().all(|&c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, b'_' | b'.' | b'-'))
}

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

/// A reader of a numbered event ring (`<prefix>_seq` + `<prefix>.<n>`) stands at `seen`: events `range` are new;
/// `lost` = some left the ring (size `ring`) before they were read.
pub fn ring_pending(seen: u64, seq: u64, ring: u64) -> (std::ops::RangeInclusive<u64>, bool) {
    if seq <= seen {
        return (1..=0, false);
    }
    let oldest = seq.saturating_sub(ring - 1).max(1);
    let first = (seen + 1).max(oldest);
    (first..=seq, first > seen + 1)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn atomic_write_replaces_and_leaves_no_tmp() {
        let d = std::env::temp_dir().join(format!("vr-generic-atomic-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        let p = d.join("a").join("x.json");
        write_atomic(&p, b"one").unwrap();
        write_atomic(&p, b"two").unwrap();
        assert_eq!(std::fs::read(&p).unwrap(), b"two");
        assert!(!d.join("a").join("x.json.tmp").exists());
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn ids_keys_values() {
        for ok in ["my_mod", "a", "_local", "mod-2.1", "0x"] {
            assert!(valid_mod_id(ok), "{ok}");
        }
        for bad in ["", "My", "a/b", "..", ".x", "a b", "a\\b", &"x".repeat(65)] {
            assert!(!valid_mod_id(bad), "{bad}");
        }
        assert!(valid_key("progress.stage 3") && valid_key("ñ"));
        assert!(!valid_key("") && !valid_key("a\nb") && !valid_key(&"k".repeat(129)));
        assert_eq!(num_value(3.0), Some(json!(3)));
        assert_eq!(num_value(-2.5), Some(json!(-2.5)));
        assert_eq!(num_value(f64::NAN), None);
        assert_eq!(num_value(1e300), Some(json!(1e300)));
        assert!(str_value(&"x".repeat(MAX_STR + 1)).is_none());
    }

    #[test]
    fn ring_reader() {
        assert_eq!(ring_pending(0, 0, 64), (1..=0, false));
        assert_eq!(ring_pending(3, 5, 64), (4..=5, false));
        assert_eq!(ring_pending(0, 100, 64), (37..=100, true));
        assert_eq!(ring_pending(36, 100, 64), (37..=100, false));
        assert!(ring_pending(5, 5, 64).0.is_empty());
    }
}
