//! Bounds-checked little-endian readers and small writing helpers.

use crate::error::{Error, Result};

/// Round `v` up to a multiple of `a` (`a` must be a power of two).
#[inline]
pub const fn align(v: usize, a: usize) -> usize {
    (v + a - 1) & !(a - 1)
}

/// A read-only view over a byte buffer whose accessors report [`Error::Truncated`]
/// instead of panicking.
#[derive(Clone, Copy)]
pub struct Reader<'a> {
    data: &'a [u8],
    what: &'static str,
}

impl<'a> Reader<'a> {
    pub fn new(data: &'a [u8], what: &'static str) -> Self {
        Self { data, what }
    }

    pub fn bytes(&self, offset: usize, len: usize) -> Result<&'a [u8]> {
        offset
            .checked_add(len)
            .and_then(|end| self.data.get(offset..end))
            .ok_or(Error::Truncated {
                what: self.what,
                offset,
                len,
                size: self.data.len(),
            })
    }

    pub fn u8(&self, offset: usize) -> Result<u8> {
        Ok(self.bytes(offset, 1)?[0])
    }

    pub fn u16(&self, offset: usize) -> Result<u16> {
        let b = self.bytes(offset, 2)?;
        Ok(u16::from_le_bytes([b[0], b[1]]))
    }

    pub fn i16(&self, offset: usize) -> Result<i16> {
        Ok(self.u16(offset)? as i16)
    }

    pub fn u32(&self, offset: usize) -> Result<u32> {
        let b = self.bytes(offset, 4)?;
        Ok(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    }

    /// NUL-terminated byte string starting at `offset` (terminator excluded).
    pub fn cstr(&self, offset: usize) -> Result<&'a [u8]> {
        let tail = self.data.get(offset..).ok_or(Error::Truncated {
            what: self.what,
            offset,
            len: 1,
            size: self.data.len(),
        })?;
        let end = tail.iter().position(|&c| c == 0).ok_or(Error::Truncated {
            what: self.what,
            offset,
            len: tail.len() + 1,
            size: self.data.len(),
        })?;
        Ok(&tail[..end])
    }
}

/// Write helpers over a pre-sized output buffer. Callers size the buffer from the
/// computed layout, so these are only used on in-range offsets.
pub fn put_u16(out: &mut [u8], offset: usize, v: u16) {
    out[offset..offset + 2].copy_from_slice(&v.to_le_bytes());
}

pub fn put_u32(out: &mut [u8], offset: usize, v: u32) {
    out[offset..offset + 4].copy_from_slice(&v.to_le_bytes());
}

pub fn put_i16(out: &mut [u8], offset: usize, v: i16) {
    put_u16(out, offset, v as u16);
}

/// Validate an entry name for a G4 string table: non-empty ASCII without NUL.
pub fn check_name(what: &'static str, name: &str) -> Result<()> {
    if name.is_empty() || !name.is_ascii() || name.contains('\0') {
        return Err(crate::error::invalid(
            what,
            format!("name {name:?} must be non-empty ASCII without NUL"),
        ));
    }
    Ok(())
}

/// Decode a name read from a string table (ASCII expected; any UTF-8 accepted).
pub fn name_from_bytes(what: &'static str, b: &[u8]) -> Result<String> {
    String::from_utf8(b.to_vec())
        .map_err(|_| crate::error::invalid(what, format!("name bytes {b:02X?} are not UTF-8")))
}

/// Convert a value to a narrower integer, returning [`Error::Limit`] on overflow.
pub fn fit<T: TryFrom<usize>>(v: usize, what: &str) -> Result<T> {
    T::try_from(v).map_err(|_| Error::Limit(format!("{what} = {v} does not fit the field")))
}
