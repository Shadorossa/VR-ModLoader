//! Game file access for Inazuma Eleven Victory Road (PC, v7.1.2).
//!
//! - [`cpk_list`]: plaintext model of `data/cpk_list.cfg.bin`.
//! - [`cpk`]: read / write the encrypted `cpk_list`, extract a file from its CPK (decrypted + decompressed).
//! - [`install`]: install a folder of `data/**` files into the game as loose files and patch `cpk_list`; undo it exactly.
//! - [`walk`]: the `data/**` files of a folder as logical game paths.
//! - [`pattern`]: game path patterns (`{ver}` placeholders, `*` wildcards, file name versions).
//! - [`timing`]: opt-in phase timing.

pub mod cpk;
pub mod cpk_list;
pub mod error;
pub mod install;
pub mod pattern;
pub mod timing;
pub mod walk;

pub use error::{Error, Result};
