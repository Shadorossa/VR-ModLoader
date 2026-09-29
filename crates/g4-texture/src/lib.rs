//! Level-5 **G4TX** textures for Inazuma Eleven Victory Road (PC), and the pieces every G4 format shares.
//!
//! * [`g4tx`] — G4TX texture containers: textures (DDS payloads) and named sprites
//!   (sub-texture rectangles with optional nine-slice guides). Byte-exact round trip,
//!   texture replacement from PNG, sprite add/rename/move.
//! * [`dds`] — DDS header parsing, BC1–BC7 / uncompressed decoding to RGBA8, and
//!   BC1/BC3/BC4/BC5/BC7 + raw encoding (feature `encode`).
//! * [`pixels`] — RGBA8 / 8-bit images, PNG load/save.
//! * [`header`], [`hash`] — the shared G4 header / pointer scheme and CRC-32 sorted index
//!   tables used by every G4 format.
//!
//! Parsers for the other G4 formats (packages, models, skeletons, animations) build on the shared pieces
//! ([`header`], [`hash`], `util`). Parsers never panic on malformed input.

// Pixel loops use `chunks_exact(4)`; `as_chunks` would not make them clearer.
#![allow(clippy::chunks_exact_to_as_chunks)]

pub mod dds;
pub mod error;
pub mod g4tx;
pub mod hash;
pub mod header;
pub mod pixels;
/// Byte readers / writers shared by the G4 format parsers.
#[doc(hidden)]
pub mod util;

pub use dds::{DdsFormat, DdsInfo, EncodeOptions};
pub use error::{Error, Result};
pub use g4tx::{EntryRef, G4tx, Sprite, Texture};
pub use hash::{crc32, name_hash};
pub use header::G4Header;
pub use pixels::{GrayImage, RgbaImage};
