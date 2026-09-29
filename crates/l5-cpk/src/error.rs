//! Error type shared by every module of the crate.

use thiserror::Error;

/// Everything that can go wrong while reading or writing CPK-related data.
///
/// Malformed input never panics; it is reported as one of these variants.
#[derive(Debug, Error)]
pub enum Error {
    /// Underlying I/O failure.
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    /// A magic number did not match.
    #[error("bad magic for {what}: expected {expected:?}, found {found:?}")]
    BadMagic {
        /// What was being parsed.
        what: &'static str,
        /// The magic that was expected.
        expected: &'static str,
        /// The first bytes actually found (lossy).
        found: String,
    },

    /// The data ends before a structure is complete, or an offset points outside it.
    #[error("truncated or out-of-bounds data in {what} (offset {offset:#x}, need {needed} bytes, have {available})")]
    Truncated {
        /// What was being parsed.
        what: &'static str,
        /// Offset of the failing access.
        offset: u64,
        /// Bytes needed.
        needed: u64,
        /// Bytes available.
        available: u64,
    },

    /// A @UTF table is structurally invalid.
    #[error("malformed @UTF table: {0}")]
    Utf(String),

    /// A CPK container is structurally invalid or uses an unsupported feature.
    #[error("malformed CPK: {0}")]
    Cpk(String),

    /// The CRILAYLA stream is invalid.
    #[error("malformed CRILAYLA data: {0}")]
    Crilayla(String),

    /// AES decryption failed (wrong key, wrong length or bad PKCS#7 padding).
    #[error("AES decryption failed: {0}")]
    Aes(String),

    /// The CPK could not be decrypted with any candidate key.
    #[error("could not decrypt {0:?}: not a CPK with the plain or filename-XOR encoding")]
    UnknownEncryption(String),

    /// A requested file is not in the archive.
    #[error("file not found in CPK: {0}")]
    NotFound(String),

    /// A value does not fit the target column type or field.
    #[error("value out of range: {0}")]
    OutOfRange(String),
}

/// Crate-wide result alias.
pub type Result<T, E = Error> = std::result::Result<T, E>;

/// Check that `offset..offset+needed` lies within `available` bytes.
pub(crate) fn check_bounds(what: &'static str, offset: u64, needed: u64, available: u64) -> Result<()> {
    match offset.checked_add(needed) {
        Some(end) if end <= available => Ok(()),
        _ => Err(Error::Truncated { what, offset, needed, available }),
    }
}

/// Lossy printable rendering of a magic for error messages.
pub(crate) fn show_magic(bytes: &[u8]) -> String {
    bytes
        .iter()
        .take(8)
        .map(|&b| if b.is_ascii_graphic() || b == b' ' { b as char } else { '.' })
        .collect()
}
