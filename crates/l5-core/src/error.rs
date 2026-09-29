//! Error type shared by every parser and writer in this crate.

use crate::text::TextEncoding;

/// Everything that can go wrong while reading or writing a Level-5 file.
///
/// Parsers never panic on malformed input: every out-of-range offset, bad count or
/// unsupported layout is reported through this type.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// A read went past the end of the buffer.
    #[error(
        "unexpected end of data: need {len} byte(s) at offset {offset:#x}, buffer is {size:#x} bytes"
    )]
    OutOfBounds {
        offset: usize,
        len: usize,
        size: usize,
    },

    /// The data does not end with the T2B footer `01 74 32 62` (or is shorter than 0x30 bytes).
    #[error("not a T2B cfg.bin (missing \\x01t2b footer)")]
    NotT2b,

    /// The data does not start with the `RDBN` magic.
    #[error("not an RDBN cfg.bin (missing RDBN magic)")]
    NotRdbn,

    /// Neither a T2B footer nor an RDBN magic was found.
    #[error("unknown cfg.bin variant (first bytes {0:02X?})")]
    UnknownFormat(Vec<u8>),

    /// The T2B entry records fit neither the 4-byte nor the 8-byte value layout (cfgbin.md §1.3).
    #[error("T2B: entry records do not fit a 4- or 8-byte value layout")]
    T2bLayout,

    /// The T2B file uses 8-byte values (never seen in Victory Road, cfgbin.md §1.3).
    #[error("T2B: 8-byte value width is not supported (Victory Road only uses 4-byte values)")]
    UnsupportedValueWidth,

    /// A NUL-terminated string has no terminator inside its pool.
    #[error("unterminated string at offset {offset:#x}")]
    UnterminatedString { offset: usize },

    /// A string cannot be represented in the file's text encoding.
    #[error("cannot encode {text:?} as {encoding:?}")]
    Unencodable {
        text: String,
        encoding: TextEncoding,
    },

    /// A count, offset or index does not fit the on-disk field that stores it.
    #[error("{what} out of range: {value}")]
    Overflow { what: &'static str, value: i128 },

    /// A value does not match the field it is written to.
    #[error("type mismatch: {0}")]
    TypeMismatch(String),

    /// Any other structural inconsistency.
    #[error("malformed data: {0}")]
    Malformed(String),
}

/// Crate-wide result alias.
pub type Result<T, E = Error> = std::result::Result<T, E>;

impl Error {
    pub(crate) fn overflow(what: &'static str, value: impl TryInto<i128>) -> Self {
        Error::Overflow {
            what,
            value: value.try_into().unwrap_or(i128::MAX),
        }
    }
}
