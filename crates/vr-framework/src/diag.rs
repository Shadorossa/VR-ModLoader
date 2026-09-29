//! Diagnostics of an engine build: [`Notes`] (the log lines a build returns; the plugin logs them with
//! [`crate::host::log_notes`], an offline tool prints them) and [`Diagnostic`] (a problem in a data file: file, line,
//! key).
//!
//! Notes are serializable so a build cache can keep them and repeat them on a cache hit (the serialized form is the
//! text engine's manifest format: `[["warn", "..."], ...]`).

use serde::{Deserialize, Serialize};
use std::fmt;

/// Level of a build note.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Lvl {
    Error,
    Warn,
    Info,
    Debug,
}

/// Log lines of a build.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Notes(pub Vec<(Lvl, String)>);

impl Notes {
    pub fn push(&mut self, l: Lvl, s: impl Into<String>) {
        self.0.push((l, s.into()));
    }
    pub fn error(&mut self, s: impl Into<String>) {
        self.push(Lvl::Error, s)
    }
    pub fn warn(&mut self, s: impl Into<String>) {
        self.push(Lvl::Warn, s)
    }
    pub fn info(&mut self, s: impl Into<String>) {
        self.push(Lvl::Info, s)
    }
    pub fn debug(&mut self, s: impl Into<String>) {
        self.push(Lvl::Debug, s)
    }
    pub fn extend(&mut self, o: Notes) {
        self.0.extend(o.0)
    }
    /// Lines at `l` or more severe.
    pub fn at(&self, l: Lvl) -> impl Iterator<Item = &str> {
        self.0.iter().filter(move |(x, _)| *x <= l).map(|(_, s)| s.as_str())
    }
    /// A data-file problem as a note (its own level, [`Diagnostic`]'s text).
    pub fn diag(&mut self, d: &Diagnostic) {
        self.push(d.level, d.to_string())
    }
    /// Any error?
    pub fn has_errors(&self) -> bool {
        self.0.iter().any(|(l, _)| *l == Lvl::Error)
    }
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

/// A problem in a data file. Displayed as `<file>:<line>: <message>` (`<file>: <message>` without a line).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Diagnostic {
    pub level: Lvl,
    /// The file as the modder sees it: `mods/<mod>/<rel>` or the path relative to the mod folder.
    pub file: String,
    /// 1-based line, when known.
    pub line: Option<usize>,
    /// The key the problem is about (unknown key, missing key, bad value), when known.
    pub key: Option<String>,
    pub message: String,
}

impl Diagnostic {
    pub fn error(file: impl Into<String>, line: Option<usize>, key: Option<String>, message: impl Into<String>) -> Diagnostic {
        Diagnostic { level: Lvl::Error, file: file.into(), line, key, message: message.into() }
    }
}

impl fmt::Display for Diagnostic {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.line {
            Some(l) => write!(f, "{}:{l}: {}", self.file, self.message),
            None => write!(f, "{}: {}", self.file, self.message),
        }
    }
}

/// 1-based line of byte offset `at` in `text`.
pub fn line_of(text: &str, at: usize) -> usize {
    let at = at.min(text.len());
    1 + text.as_bytes()[..at].iter().filter(|&&b| b == b'\n').count()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn notes_levels_and_serde_form() {
        let mut n = Notes::default();
        n.info("i");
        n.warn("w");
        n.error("e");
        n.debug("d");
        assert_eq!(n.at(Lvl::Warn).collect::<Vec<_>>(), ["w", "e"]);
        assert!(n.has_errors());
        assert_eq!(serde_json::to_string(&n).unwrap(), r#"[["info","i"],["warn","w"],["error","e"],["debug","d"]]"#);
        let d = Diagnostic::error("mods/a/example/x.toml", Some(3), Some("colr".into()), "unknown key `colr`");
        assert_eq!(d.to_string(), "mods/a/example/x.toml:3: unknown key `colr`");
        n.diag(&d);
        assert_eq!(n.0.last().unwrap().1, "mods/a/example/x.toml:3: unknown key `colr`");
        assert_eq!(line_of("a\nb\nc", 2), 2);
        assert_eq!(line_of("a\nb\nc", 0), 1);
    }
}
