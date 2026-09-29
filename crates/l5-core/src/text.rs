//! Lossless text codecs for cfg.bin strings (docs/formats/cfgbin.md §4).
//!
//! T2B strings are Shift-JIS (CP932) or UTF-8 depending on the footer flag; RDBN strings
//! are always UTF-8. To guarantee byte-exact round trips even for bytes that are not
//! valid in the declared encoding, undecodable bytes are mapped to the private-use code
//! points `U+10FF00 + byte` (the same idea as Python's `surrogateescape`, which the
//! reference `tools/py/cfgbin.py` uses). Such code points never occur in game text; if a
//! decoded file genuinely contained one, its bytes are escaped too, so
//! `encode(decode(b)) == b` holds for every input.

use encoding_rs::SHIFT_JIS;
use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};

/// Text encoding of a cfg.bin string pool.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum TextEncoding {
    /// Shift-JIS / CP932 (T2B footer encoding `0`).
    ShiftJis,
    /// UTF-8 (T2B footer encoding `1`, also `0x100`/`0x101` in other games; always for RDBN).
    Utf8,
}

const ESCAPE_BASE: u32 = 0x10_FF00;

#[inline]
fn escape_char(b: u8) -> char {
    // U+10FF00..=U+10FFFF are valid scalar values (plane 16 private use).
    char::from_u32(ESCAPE_BASE + b as u32).unwrap_or(char::REPLACEMENT_CHARACTER)
}

#[inline]
fn unescape_char(c: char) -> Option<u8> {
    let v = c as u32;
    (v >= ESCAPE_BASE).then(|| (v - ESCAPE_BASE) as u8)
}

#[inline]
fn is_escape(c: char) -> bool {
    c as u32 >= ESCAPE_BASE
}

impl TextEncoding {
    /// Decode bytes (without terminator) losslessly.
    pub fn decode(self, bytes: &[u8]) -> String {
        match self {
            TextEncoding::Utf8 => decode_utf8(bytes),
            TextEncoding::ShiftJis => decode_sjis(bytes),
        }
    }

    /// Encode a string produced by [`decode`](Self::decode) (or any user text) back to bytes.
    ///
    /// Fails with [`Error::Unencodable`] when a character has no Shift-JIS mapping.
    pub fn encode(self, s: &str) -> Result<Vec<u8>> {
        match self {
            TextEncoding::Utf8 => Ok(encode_utf8(s)),
            TextEncoding::ShiftJis => encode_sjis(s),
        }
    }
}

fn decode_utf8(bytes: &[u8]) -> String {
    if let Ok(s) = std::str::from_utf8(bytes)
        && !s.chars().any(is_escape)
    {
        return s.to_owned();
    }
    let mut out = String::with_capacity(bytes.len());
    for chunk in bytes.utf8_chunks() {
        for c in chunk.valid().chars() {
            if is_escape(c) {
                let mut buf = [0u8; 4];
                for &b in c.encode_utf8(&mut buf).as_bytes() {
                    out.push(escape_char(b));
                }
            } else {
                out.push(c);
            }
        }
        for &b in chunk.invalid() {
            out.push(escape_char(b));
        }
    }
    out
}

fn encode_utf8(s: &str) -> Vec<u8> {
    if !s.chars().any(is_escape) {
        return s.as_bytes().to_vec();
    }
    let mut out = Vec::with_capacity(s.len());
    let mut buf = [0u8; 4];
    for c in s.chars() {
        match unescape_char(c) {
            Some(b) => out.push(b),
            None => out.extend_from_slice(c.encode_utf8(&mut buf).as_bytes()),
        }
    }
    out
}

/// Decode `unit` strictly and accept it only if it re-encodes to the same bytes.
fn sjis_unit(unit: &[u8]) -> Option<String> {
    let s = SHIFT_JIS.decode_without_bom_handling_and_without_replacement(unit)?;
    if s.chars().any(is_escape) {
        return None;
    }
    let (enc, _, had_errors) = SHIFT_JIS.encode(&s);
    (!had_errors && enc.as_ref() == unit).then(|| s.into_owned())
}

fn decode_sjis(bytes: &[u8]) -> String {
    if bytes.is_ascii() {
        // ASCII maps to itself in WHATWG Shift_JIS (0x5C/0x7E included).
        return decode_utf8(bytes);
    }
    if let Some(s) = sjis_unit(bytes) {
        return s;
    }
    // Slow path: character by character, escaping anything that does not round-trip.
    let mut out = String::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        let b = bytes[i];
        let is_lead = matches!(b, 0x81..=0x9F | 0xE0..=0xFC);
        if is_lead
            && i + 1 < bytes.len()
            && let Some(s) = sjis_unit(&bytes[i..i + 2])
        {
            out.push_str(&s);
            i += 2;
            continue;
        }
        match sjis_unit(&bytes[i..i + 1]) {
            Some(s) => out.push_str(&s),
            None => out.push(escape_char(b)),
        }
        i += 1;
    }
    out
}

fn encode_sjis(s: &str) -> Result<Vec<u8>> {
    let mut out = Vec::with_capacity(s.len() * 2);
    let mut run_start = 0;
    let flush = |out: &mut Vec<u8>, run: &str| -> Result<()> {
        if run.is_empty() {
            return Ok(());
        }
        let (enc, _, had_errors) = SHIFT_JIS.encode(run);
        if had_errors {
            return Err(Error::Unencodable {
                text: run.to_owned(),
                encoding: TextEncoding::ShiftJis,
            });
        }
        out.extend_from_slice(&enc);
        Ok(())
    };
    for (i, c) in s.char_indices() {
        if let Some(b) = unescape_char(c) {
            flush(&mut out, &s[run_start..i])?;
            out.push(b);
            run_start = i + c.len_utf8();
        }
    }
    flush(&mut out, &s[run_start..])?;
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rt(enc: TextEncoding, b: &[u8]) {
        let s = enc.decode(b);
        assert_eq!(enc.encode(&s).unwrap(), b, "{enc:?} {b:02X?} -> {s:?}");
    }

    #[test]
    fn utf8_lossless() {
        rt(TextEncoding::Utf8, "Hello! 円堂守".as_bytes());
        rt(TextEncoding::Utf8, b"bad \xFF\xFE bytes \xE3\x81");
        rt(TextEncoding::Utf8, "\u{10FF41}".as_bytes());
        assert_eq!(TextEncoding::Utf8.decode(b"abc"), "abc");
    }

    #[test]
    fn sjis_lossless() {
        let (b, _, _) = SHIFT_JIS.encode("円堂守 ｶﾀｶﾅ \\~");
        rt(TextEncoding::ShiftJis, &b);
        assert_eq!(TextEncoding::ShiftJis.decode(&b), "円堂守 ｶﾀｶﾅ \\~");
        // NEC-selected IBM extension duplicate (0xED40) and invalid trail bytes
        rt(TextEncoding::ShiftJis, b"\xED\x40\x81\x20\xFF\x80\xA0\x82");
        assert!(TextEncoding::ShiftJis.encode("😀").is_err());
    }
}
