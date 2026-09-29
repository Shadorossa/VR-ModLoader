//! Standard CRC-32 name hashes — the universal key of Victory Road (docs/OVERVIEW.md §2).
//!
//! Every file path, cfg.bin entry name, RDBN field/type/list name, character ID, texture
//! sprite, … is identified by **standard CRC-32** (zlib / IEEE: reflected, poly `0xEDB88320`,
//! init and xorout `0xFFFFFFFF`) over the name's bytes (docs/formats/cfgbin.md, "Hashing").
//!
//! Game tables store these IDs as either signed or unsigned 32-bit integers, so helpers
//! for both views are provided.

const POLY: u32 = 0xEDB8_8320;

const fn make_table() -> [u32; 256] {
    let mut table = [0u32; 256];
    let mut i = 0;
    while i < 256 {
        let mut c = i as u32;
        let mut k = 0;
        while k < 8 {
            c = if c & 1 != 0 { POLY ^ (c >> 1) } else { c >> 1 };
            k += 1;
        }
        table[i] = c;
        i += 1;
    }
    table
}

const TABLE: [u32; 256] = make_table();

/// `const` CRC-32 (usable for compile-time constants). Use [`crc32`] at runtime — it is SIMD accelerated.
pub const fn crc32_const(data: &[u8]) -> u32 {
    let mut c = 0xFFFF_FFFFu32;
    let mut i = 0;
    while i < data.len() {
        c = TABLE[((c ^ data[i] as u32) & 0xFF) as usize] ^ (c >> 8);
        i += 1;
    }
    c ^ 0xFFFF_FFFF
}

/// Standard CRC-32 of raw bytes (`zlib.crc32(data)`).
#[inline]
pub fn crc32(data: &[u8]) -> u32 {
    crc32fast::hash(data)
}

/// CRC-32 of a name encoded as UTF-8 (identical to Shift-JIS for ASCII names).
#[inline]
pub fn crc32_str(name: &str) -> u32 {
    crc32(name.as_bytes())
}

/// CRC-32 of a name, as the signed value stored in most T2B int fields.
#[inline]
pub fn crc32_i32(name: &str) -> i32 {
    to_i32(crc32_str(name))
}

/// JAM-CRC (`!crc32`). Used by some older Level-5 titles, never by Victory Road.
#[inline]
pub fn jamcrc(data: &[u8]) -> u32 {
    !crc32(data)
}

/// Reinterpret an unsigned hash as the signed value stored in the files.
#[inline]
pub const fn to_i32(hash: u32) -> i32 {
    hash as i32
}

/// Reinterpret a signed stored value as the unsigned hash.
#[inline]
pub const fn to_u32(value: i32) -> u32 {
    value as u32
}

/// `crc32("INVALID")` — marks an unused slot (`-992181094` signed).
pub const INVALID: u32 = crc32_const(b"INVALID");
/// [`INVALID`] as stored in signed int fields.
pub const INVALID_I32: i32 = to_i32(INVALID);
/// `crc32("0")` — the "none" ID (`-186917087` signed).
pub const NONE: u32 = crc32_const(b"0");
/// [`NONE`] as stored in signed int fields.
pub const NONE_I32: i32 = to_i32(NONE);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_values() {
        assert_eq!(crc32(b"123456789"), 0xCBF4_3926);
        assert_eq!(crc32_const(b"123456789"), 0xCBF4_3926);
        assert_eq!(INVALID_I32, -992_181_094);
        assert_eq!(NONE_I32, -186_917_087);
        assert_eq!(crc32_str("INVALID"), INVALID);
        assert_eq!(jamcrc(b"x"), !crc32(b"x"));
        assert_eq!(to_u32(to_i32(0xDEAD_BEEF)), 0xDEAD_BEEF);
        // chat_text.cfg.bin key table (cfgbin.md §1.8)
        assert_eq!(crc32_str("TEXT_INFO_BEGIN"), 0x0EFB_9738);
    }
}
