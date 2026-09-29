//! Error type shared by every module of the crate.

use thiserror::Error;

/// Errors returned by the G4 parsers, writers and codecs.
///
/// Parsers never panic on malformed input: every out-of-range read is reported as
/// [`Error::Truncated`] and every broken invariant as [`Error::Invalid`].
#[derive(Debug, Error)]
pub enum Error {
    /// The file does not start with the expected magic.
    #[error("bad magic: expected {expected:?}, found {found:02X?}")]
    BadMagic {
        /// Magic the parser expected (e.g. `"G4TX"`).
        expected: &'static str,
        /// First bytes actually found (up to 4).
        found: Vec<u8>,
    },

    /// A read went past the end of the buffer.
    #[error(
        "{what}: read of {len} bytes at 0x{offset:X} is out of bounds (buffer is {size} bytes)"
    )]
    Truncated {
        /// Which structure was being read.
        what: &'static str,
        /// Absolute offset of the read.
        offset: usize,
        /// Number of bytes requested.
        len: usize,
        /// Size of the buffer.
        size: usize,
    },

    /// A structural invariant does not hold (inconsistent counts, bad pointers, ...).
    #[error("invalid {what}: {detail}")]
    Invalid {
        /// Which structure is invalid.
        what: &'static str,
        /// Human readable explanation.
        detail: String,
    },

    /// A valid but unsupported feature (pixel format, DDS variant, ...).
    #[error("unsupported: {0}")]
    Unsupported(String),

    /// A format limit would be exceeded when writing (u8/u16 counts, u16 pointers, ...).
    #[error("format limit exceeded: {0}")]
    Limit(String),

    /// A named entry / texture / sprite does not exist.
    #[error("not found: {0}")]
    NotFound(String),

    /// PNG encoding/decoding error from the `image` crate.
    #[error("image error: {0}")]
    Image(#[from] image::ImageError),

    /// File-system error.
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
}

/// Convenience alias used across the crate.
pub type Result<T, E = Error> = std::result::Result<T, E>;

/// Shorthand constructor for [`Error::Invalid`].
#[doc(hidden)]
pub fn invalid(what: &'static str, detail: impl Into<String>) -> Error {
    Error::Invalid {
        what,
        detail: detail.into(),
    }
}
