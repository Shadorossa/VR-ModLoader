//! Log rate limiting for LUAERR / LUAPRINT / WATCH lines: at most `max` lines per window, identical consecutive
//! messages folded into a repeat count.

#[derive(Debug)]
pub struct Limiter {
    window_ms: u64,
    max: u32,
    start: u64,
    count: u32,
    suppressed: u32,
    last_key: u64,
    last_ms: u64,
    repeats: u32,
}

/// What to do with one message.
#[derive(Debug, PartialEq, Eq)]
pub struct Verdict {
    pub log: bool,
    /// Lines to write before the message (summaries of what was dropped).
    pub notes: Vec<String>,
}

/// Identical messages closer than this are folded.
const FOLD_MS: u64 = 5_000;

impl Limiter {
    pub const fn new(window_ms: u64, max: u32) -> Limiter {
        Limiter { window_ms, max, start: 0, count: 0, suppressed: 0, last_key: 0, last_ms: 0, repeats: 0 }
    }

    pub fn check(&mut self, now_ms: u64, key: u64) -> Verdict {
        let mut notes = Vec::new();
        if key == self.last_key && now_ms.saturating_sub(self.last_ms) < FOLD_MS && self.last_ms != 0 {
            self.repeats += 1;
            self.last_ms = now_ms;
            return Verdict { log: false, notes };
        }
        if self.repeats > 0 {
            notes.push(format!("(previous message repeated {} more time(s))", self.repeats));
            self.repeats = 0;
        }
        self.last_key = key;
        self.last_ms = now_ms.max(1);
        if now_ms.saturating_sub(self.start) >= self.window_ms {
            if self.suppressed > 0 {
                notes.push(format!("({} message(s) suppressed by the rate limit)", self.suppressed));
            }
            self.start = now_ms;
            self.count = 0;
            self.suppressed = 0;
        }
        if self.count >= self.max {
            self.suppressed += 1;
            return Verdict { log: false, notes };
        }
        self.count += 1;
        Verdict { log: true, notes }
    }
}

/// FNV-1a of a message (folding key).
pub fn key(s: &str) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in s.bytes() {
        h ^= b as u64;
        h = h.wrapping_mul(0x100_0000_01b3);
    }
    h
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn folds_repeats_and_limits() {
        let mut l = Limiter::new(10_000, 3);
        assert!(l.check(1, key("a")).log);
        assert!(!l.check(2, key("a")).log);
        assert!(!l.check(3, key("a")).log);
        let v = l.check(4, key("b"));
        assert!(v.log);
        assert_eq!(v.notes, vec!["(previous message repeated 2 more time(s))"]);
        assert!(l.check(5, key("c")).log);
        assert!(!l.check(6, key("d")).log); // 4th distinct in the window
        assert!(!l.check(7, key("e")).log);
        let v = l.check(20_000, key("f"));
        assert!(v.log);
        assert_eq!(v.notes, vec!["(2 message(s) suppressed by the rate limit)"]);
        // same message after the fold interval is logged again
        assert!(l.check(30_000, key("f")).log);
    }
}
