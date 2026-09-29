//! The 16-byte prefix shared by every G4 file and the "u16 × 4" section pointer scheme.
//!
//! ```text
//! +0x00 char[4] magic          G4PK / G4TX / G4MD / G4SK / ...
//! +0x04 u16     headerSize     0x40, 0x60 or 0xA0
//! +0x06 u16     fileType       0x64 G4PK, 0x65 G4TX, 0x68 G4MD family ...
//! +0x08 u16     0
//! +0x0A u16     headerSize / 4 ("header words")
//! +0x0C u32     content size (G4PK/G4MD: fileSize - headerSize; G4TX: table size)
//! ```
//!
//! Every internal table pointer is a `u16` counted in dwords from the end of the header:
//! `absolute = headerSize + value * 4` (see `docs/formats/g4-models.md` §0).

use crate::error::{Error, Result, invalid};
use crate::util::{Reader, put_u16, put_u32};

/// Parsed common G4 prefix.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct G4Header {
    /// Four-character magic, e.g. `*b"G4TX"`.
    pub magic: [u8; 4],
    /// Header size in bytes; also the base of every section pointer.
    pub header_size: u16,
    /// File type tag (0x64, 0x65, 0x66, 0x68 ...).
    pub file_type: u16,
    /// Always 0 in retail files.
    pub reserved: u16,
    /// `header_size / 4` in retail files.
    pub header_words: u16,
    /// Content size (meaning depends on the format).
    pub size_field: u32,
}

impl G4Header {
    /// Byte length of the common prefix.
    pub const LEN: usize = 0x10;

    /// Parse the prefix; `expected` magic is checked when given.
    pub fn parse(data: &[u8], expected: Option<&'static str>) -> Result<Self> {
        let r = Reader::new(data, "G4 header");
        let m = r.bytes(0, 4).map_err(|_| Error::BadMagic {
            expected: expected.unwrap_or("G4??"),
            found: data.iter().take(4).copied().collect(),
        })?;
        let magic = [m[0], m[1], m[2], m[3]];
        if let Some(exp) = expected
            && exp.as_bytes() != magic
        {
            return Err(Error::BadMagic {
                expected: exp,
                found: magic.to_vec(),
            });
        }
        let h = Self {
            magic,
            header_size: r.u16(4)?,
            file_type: r.u16(6)?,
            reserved: r.u16(8)?,
            header_words: r.u16(0x0A)?,
            size_field: r.u32(0x0C)?,
        };
        if (h.header_size as usize) < Self::LEN || !h.header_size.is_multiple_of(4) {
            return Err(invalid(
                "G4 header",
                format!(
                    "header size 0x{:X} is not a multiple of 4 >= 0x10",
                    h.header_size
                ),
            ));
        }
        if h.header_size as usize > data.len() {
            return Err(Error::Truncated {
                what: "G4 header",
                offset: 0,
                len: h.header_size as usize,
                size: data.len(),
            });
        }
        Ok(h)
    }

    /// Write the prefix into the first 16 bytes of `out`.
    pub fn write_into(&self, out: &mut [u8]) {
        out[0..4].copy_from_slice(&self.magic);
        put_u16(out, 4, self.header_size);
        put_u16(out, 6, self.file_type);
        put_u16(out, 8, self.reserved);
        put_u16(out, 0x0A, self.header_words);
        put_u32(out, 0x0C, self.size_field);
    }

    /// Absolute offset of a section pointer value.
    pub fn resolve(&self, ptr: u16) -> usize {
        ptr_to_offset(self.header_size as usize, ptr)
    }
}

/// `header_size + value * 4`.
pub fn ptr_to_offset(header_size: usize, value: u16) -> usize {
    header_size + value as usize * 4
}

/// Inverse of [`ptr_to_offset`]; fails if `abs` is before the header end, not dword
/// aligned relative to it, or does not fit a u16.
pub fn offset_to_ptr(header_size: usize, abs: usize) -> Result<u16> {
    if abs < header_size || !(abs - header_size).is_multiple_of(4) {
        return Err(invalid(
            "section pointer",
            format!("offset 0x{abs:X} is not header_size + 4*k (header 0x{header_size:X})"),
        ));
    }
    u16::try_from((abs - header_size) / 4).map_err(|_| {
        Error::Limit(format!(
            "section at 0x{abs:X} is beyond the u16 dword pointer range (0x{:X})",
            header_size + 0xFFFF * 4
        ))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pointer_round_trip() {
        assert_eq!(ptr_to_offset(0x60, 0x12), 0x60 + 0x48);
        assert_eq!(offset_to_ptr(0x60, 0xA8).unwrap(), 0x12);
        assert!(offset_to_ptr(0x60, 0x62).is_err());
        assert!(offset_to_ptr(0x60, 0x10).is_err());
        assert!(offset_to_ptr(0x40, 0x40 + 0x10000 * 4).is_err());
    }

    #[test]
    fn header_parse_write() {
        let mut b = vec![0u8; 0x40];
        let h = G4Header {
            magic: *b"G4PK",
            header_size: 0x40,
            file_type: 0x64,
            reserved: 0,
            header_words: 0x10,
            size_field: 0,
        };
        h.write_into(&mut b);
        assert_eq!(G4Header::parse(&b, Some("G4PK")).unwrap(), h);
        assert!(matches!(
            G4Header::parse(&b, Some("G4TX")),
            Err(Error::BadMagic { .. })
        ));
        assert!(G4Header::parse(&b[..3], None).is_err());
    }
}
