//! Bounds-checked little-endian primitives used by the parsers.

use crate::error::{Error, Result};

/// Round `x` up to a multiple of the power-of-two `a`.
#[inline]
pub(crate) fn align(x: usize, a: usize) -> usize {
    debug_assert!(a.is_power_of_two());
    (x + a - 1) & !(a - 1)
}

/// Checked variant of [`align`] for values coming from untrusted headers.
#[inline]
pub(crate) fn align_checked(x: usize, a: usize) -> Result<usize> {
    x.checked_add(a - 1)
        .map(|v| v & !(a - 1))
        .ok_or_else(|| Error::overflow("aligned offset", x))
}

/// `d[off..off + len]`, or an [`Error::OutOfBounds`].
#[inline]
pub(crate) fn get(d: &[u8], off: usize, len: usize) -> Result<&[u8]> {
    off.checked_add(len)
        .and_then(|end| d.get(off..end))
        .ok_or(Error::OutOfBounds {
            offset: off,
            len,
            size: d.len(),
        })
}

#[inline]
pub(crate) fn arr<const N: usize>(d: &[u8], off: usize) -> Result<[u8; N]> {
    let s = get(d, off, N)?;
    let mut a = [0u8; N];
    a.copy_from_slice(s);
    Ok(a)
}

#[inline]
pub(crate) fn u8_at(d: &[u8], off: usize) -> Result<u8> {
    d.get(off).copied().ok_or(Error::OutOfBounds {
        offset: off,
        len: 1,
        size: d.len(),
    })
}
#[inline]
pub(crate) fn u16_at(d: &[u8], off: usize) -> Result<u16> {
    arr(d, off).map(u16::from_le_bytes)
}
#[inline]
pub(crate) fn i16_at(d: &[u8], off: usize) -> Result<i16> {
    arr(d, off).map(i16::from_le_bytes)
}
#[inline]
pub(crate) fn u32_at(d: &[u8], off: usize) -> Result<u32> {
    arr(d, off).map(u32::from_le_bytes)
}
#[inline]
pub(crate) fn i32_at(d: &[u8], off: usize) -> Result<i32> {
    arr(d, off).map(i32::from_le_bytes)
}
#[inline]
pub(crate) fn f32_at(d: &[u8], off: usize) -> Result<f32> {
    arr(d, off).map(f32::from_le_bytes)
}

/// Bytes of the NUL-terminated string starting at `off` (terminator excluded).
pub(crate) fn cstr(d: &[u8], off: usize) -> Result<&[u8]> {
    let tail = d.get(off..).ok_or(Error::OutOfBounds {
        offset: off,
        len: 1,
        size: d.len(),
    })?;
    let n = tail
        .iter()
        .position(|&b| b == 0)
        .ok_or(Error::UnterminatedString { offset: off })?;
    Ok(&tail[..n])
}

/// Pad `buf` with `byte` up to a multiple of `a`.
#[inline]
pub(crate) fn pad_to(buf: &mut Vec<u8>, a: usize, byte: u8) {
    let target = align(buf.len(), a);
    buf.resize(target, byte);
}

/// NUL-terminated string pool shared by the T2B and RDBN writers.
///
/// With `dedup = true` identical byte strings share one offset (first occurrence wins);
/// with `dedup = false` every `add` writes a new copy. Tails of other strings are never
/// shared: Level-5 never does that (cfgbin.md §1.4, rdbn.md §8).
#[derive(Debug, Default)]
pub(crate) struct StringPool {
    pub buf: Vec<u8>,
    pub count: usize,
    dedup: bool,
    map: std::collections::HashMap<Vec<u8>, usize>,
}

impl StringPool {
    pub fn new(dedup: bool) -> Self {
        StringPool {
            dedup,
            ..Default::default()
        }
    }

    /// Add a string (encoded bytes, no terminator) and return its offset.
    pub fn add(&mut self, s: &[u8]) -> usize {
        if self.dedup
            && let Some(&o) = self.map.get(s)
        {
            return o;
        }
        let off = self.buf.len();
        self.buf.extend_from_slice(s);
        self.buf.push(0);
        self.count += 1;
        self.map.entry(s.to_vec()).or_insert(off);
        off
    }

    /// Offset of the first copy of `s`, if it was added.
    pub fn offset_of(&self, s: &[u8]) -> Option<usize> {
        self.map.get(s).copied()
    }
}
