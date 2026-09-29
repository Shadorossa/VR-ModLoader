//! cfg.bin variant detection and the unified [`CfgBin`] document (docs/formats/cfgbin.md, intro).

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};
use crate::rdbn::{self, Rdbn};
use crate::t2b::{self, T2b};

/// Container variant of a `.cfg.bin`-family file.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum FileKind {
    /// Footer `01 74 32 62` in the last 16 bytes.
    T2b,
    /// Magic `RDBN` at offset 0.
    Rdbn,
}

/// Identify a file by content. Order as in CfgBinEditor: T2B footer first, then RDBN
/// magic (the two cannot collide). Returns `None` for anything else, including the
/// AES-encrypted `cpk_list.cfg.bin`.
pub fn detect(data: &[u8]) -> Option<FileKind> {
    if t2b::is_t2b(data) {
        Some(FileKind::T2b)
    } else if rdbn::is_rdbn(data) {
        Some(FileKind::Rdbn)
    } else {
        None
    }
}

/// A parsed cfg.bin of either variant.
///
/// JSON: the variant's own object plus a `"format": "t2b" | "rdbn"` tag.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "format", rename_all = "camelCase")]
pub enum CfgBin {
    T2b(T2b),
    Rdbn(Rdbn),
}

impl CfgBin {
    /// Detect the variant and parse.
    pub fn parse(data: &[u8]) -> Result<CfgBin> {
        match detect(data) {
            Some(FileKind::T2b) => T2b::parse(data).map(CfgBin::T2b),
            Some(FileKind::Rdbn) => Rdbn::parse(data).map(CfgBin::Rdbn),
            None => Err(Error::UnknownFormat(data.iter().take(4).copied().collect())),
        }
    }

    /// Serialise (byte-exact for unmodified game files).
    pub fn to_bytes(&self) -> Result<Vec<u8>> {
        match self {
            CfgBin::T2b(d) => d.to_bytes(),
            CfgBin::Rdbn(d) => d.to_bytes(),
        }
    }

    /// Variant of this document.
    pub fn kind(&self) -> FileKind {
        match self {
            CfgBin::T2b(_) => FileKind::T2b,
            CfgBin::Rdbn(_) => FileKind::Rdbn,
        }
    }

    /// The T2B document, if this is one.
    pub fn as_t2b(&self) -> Option<&T2b> {
        match self {
            CfgBin::T2b(d) => Some(d),
            CfgBin::Rdbn(_) => None,
        }
    }

    /// The RDBN document, if this is one.
    pub fn as_rdbn(&self) -> Option<&Rdbn> {
        match self {
            CfgBin::Rdbn(d) => Some(d),
            CfgBin::T2b(_) => None,
        }
    }
}

impl From<T2b> for CfgBin {
    fn from(d: T2b) -> Self {
        CfgBin::T2b(d)
    }
}

impl From<Rdbn> for CfgBin {
    fn from(d: Rdbn) -> Self {
        CfgBin::Rdbn(d)
    }
}
