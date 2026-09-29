//! CRI ADX2 audio formats, read-only (docs/formats/audio-acb-awb-hca.md).
//!
//! * [`utf`] — `@UTF` tables (the container of ACB / ACF files and of every table nested in them).
//! * [`acb`] — cue sheets: cue names → waveforms, loop points, memory / stream AWB ids.
//! * [`awb`] — `AFS2` archives (`.awb` and the memory AWB inside an ACB): id → byte range.
//! * [`hca`] — HCA decoder (v1.3 – v3.0, no cipher or type-0 cipher) to interleaved 16-bit PCM.
//! * [`wav`] — RIFF/WAVE writer for the decoded PCM.
//!
//! Nothing here writes game files; the game's banks are used as-is (no key, no obfuscation).

pub mod acb;
pub mod bank;
pub mod crypt;
pub mod awb;
pub mod hca;
pub mod hca_enc;
pub mod utf;
pub mod utf_own;
pub mod wav;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("@UTF: {0}")]
    Utf(String),
    #[error("ACB: {0}")]
    Acb(String),
    #[error("AFS2: {0}")]
    Afs2(String),
    #[error("HCA: {0}")]
    Hca(String),
}

pub type Result<T> = std::result::Result<T, Error>;

/// The v7.1.2 dump's `sound_asset` folder (tests only; skipped when absent).
#[cfg(test)]
pub(crate) const DUMP_SOUND_ASSET: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../assets/v7.1.2/data/common/sound_asset");
