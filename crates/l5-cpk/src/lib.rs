//! CRI CPK archives as used by *Inazuma Eleven: Victory Road* (PC), plus the Level-5 encryption layers around
//! them.
//!
//! | Module | What | Spec |
//! |---|---|---|
//! | [`xor`] | filename-keyed XOR stream cipher (CPKs, loose USM) | `docs/formats/encryption.md` §1 |
//! | [`aes_list`] | AES-256-CBC layer of `data/cpk_list.cfg.bin` | `docs/formats/encryption.md` §2, `docs/formats/cpk_list.md` |
//! | [`utf`] | CRI `@UTF` table reader/writer | `docs/formats/cpk.md` §2.1 |
//! | [`crilayla`] | CRILAYLA decompressor and compressor | `docs/formats/cpk.md` §3 |
//! | [`cpk`] | streaming CPK reader (header/TOC/ITOC/ETOC, extraction) | `docs/formats/cpk.md` §2, §5 |
//! | [`writer`] | Viola-compatible CPK builder | `docs/formats/cpk.md` §4 |
//!
//! ```no_run
//! use l5_cpk::CpkArchive;
//! let mut cpk = CpkArchive::open(r"data\packs\e832856918ebb97cb4430f715e6bd525.cpk")?;
//! let entry = cpk.entries()[0].clone();
//! let bytes = cpk.extract(&entry)?; // decrypted + CRILAYLA-decompressed
//! # Ok::<(), l5_cpk::Error>(())
//! ```

#![warn(missing_docs)]

pub mod aes_list;
pub mod cpk;
pub mod crilayla;
mod error;
pub mod utf;
pub mod writer;
pub mod xor;

pub use cpk::{CpkArchive, CpkEntry, PacketHeader};
pub use error::{Error, Result};
pub use utf::{Column, ColumnType, Storage, UtfTable, Value};
pub use writer::{CpkBuilder, CpkSource};
pub use xor::{Encryption, XorReader, XorWriter, key_for_name};
