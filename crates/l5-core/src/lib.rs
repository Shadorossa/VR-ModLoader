//! Level-5 core formats for Inazuma Eleven: Victory Road (PC).
//!
//! * [`hash`] — standard CRC-32 name hashes (docs/OVERVIEW.md §2).
//! * [`t2b`] — T2B cfg.bin read/write + nested tree view (docs/formats/cfgbin.md).
//! * [`rdbn`] — RDBN typed databases read/write (docs/formats/rdbn.md).
//! * [`mod@detect`] — variant detection and the unified [`CfgBin`] document.
//! * [`text`] — lossless Shift-JIS / UTF-8 codecs used by both.
//!
//! Every writer reproduces the game's files byte for byte when the document is unchanged:
//! 70 798 / 70 798 T2B and 302 / 302 RDBN files of the v7.1.2 dump.
//!
//! ```
//! use l5_core::{CfgBin, t2b::{T2b, Value}};
//!
//! let mut doc = T2b::default();
//! let e = doc.new_entry("TEXT_INFO", vec![Value::Int(1), Value::String(Some("Hi".into()))])?;
//! doc.entries.push(e);
//! let bytes = doc.to_bytes()?;
//! assert_eq!(CfgBin::parse(&bytes)?.to_bytes()?, bytes);
//! # Ok::<(), l5_core::Error>(())
//! ```

mod bytes;
pub mod detect;
pub mod error;
pub mod hash;
pub mod rdbn;
pub mod t2b;
pub mod text;

pub use detect::{CfgBin, FileKind, detect};
pub use error::{Error, Result};
pub use rdbn::Rdbn;
pub use t2b::T2b;
pub use text::TextEncoding;
