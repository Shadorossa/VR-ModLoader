//! Encryption layer of `data/cpk_list.cfg.bin`.
//!
//! Spec: `docs/formats/encryption.md` §2 and `docs/formats/cpk_list.md` §2. IEVR (PC) stores the list as
//! AES-256-CBC with PKCS#7 padding. The key and IV are kept obfuscated (as in Viola's `CCpkListUtils` and, by the
//! evidence of the immediates in `nie.exe`, in the game itself) and de-obfuscated with the filename XOR at
//! offset 0 keyed by `crc32("key")` / `crc32("iv")`.
//!
//! The plaintext is a T2B cfg.bin; parsing it is another crate's job. This module only returns / accepts bytes.
//! Older Level-5 PC games use the legacy scheme: the filename XOR keyed by `crc32("cpk_list.cfg.bin")`.

use aes::Aes256;
use cbc::cipher::{BlockDecryptMut, BlockEncryptMut, KeyIvInit, block_padding::Pkcs7};

use crate::error::{Error, Result};
use crate::xor::{crc32, crypt_in_place};

type Aes256CbcDec = cbc::Decryptor<Aes256>;
type Aes256CbcEnc = cbc::Encryptor<Aes256>;

/// Obfuscated AES key bytes as stored by Viola (`CCpkListUtils`).
const OBFUSCATED_KEY: [u8; 32] = [
    0x21, 0xCB, 0xC9, 0x72, 0xF9, 0xF2, 0x8B, 0x17, 0x9D, 0xE2, 0x50, 0x64, 0xD1, 0x8C, 0xA9, 0x4D, 0x53, 0x8D, 0x90,
    0x1E, 0x96, 0xF6, 0x0D, 0x75, 0x7A, 0xA8, 0xD9, 0x43, 0x42, 0xE2, 0x4F, 0x58,
];

/// Obfuscated AES IV bytes as stored by Viola (`CCpkListUtils`).
const OBFUSCATED_IV: [u8; 16] =
    [0x6D, 0x4B, 0x8E, 0x3F, 0x2F, 0x49, 0xC9, 0xF0, 0x9D, 0xE6, 0x44, 0x38, 0xE3, 0x1E, 0xCB, 0xB0];

/// T2B footer magic `01 74 32 62` found 0x10 bytes before the end of a cfg.bin.
pub const T2B_FOOTER_MAGIC: [u8; 4] = [0x01, 0x74, 0x32, 0x62];

/// XOR key of the legacy (pre-AES) encoding: `crc32("cpk_list.cfg.bin")` = `0x1717E18E`.
pub const LEGACY_XOR_KEY: u32 = 0x1717_E18E;

/// The de-obfuscated AES-256 key.
pub fn aes_key() -> [u8; 32] {
    let mut k = OBFUSCATED_KEY;
    crypt_in_place(crc32(b"key"), 0, &mut k);
    k
}

/// The de-obfuscated AES IV.
pub fn aes_iv() -> [u8; 16] {
    let mut iv = OBFUSCATED_IV;
    crypt_in_place(crc32(b"iv"), 0, &mut iv);
    iv
}

/// AES-256-CBC/PKCS#7-decrypt an encrypted `cpk_list.cfg.bin`, returning the T2B plaintext.
///
/// Uses a fresh CBC context per call (see the .NET gotcha in `encryption.md` §2.2).
pub fn decrypt(data: &[u8]) -> Result<Vec<u8>> {
    if data.is_empty() || !data.len().is_multiple_of(16) {
        return Err(Error::Aes(format!("length {} is not a non-zero multiple of 16", data.len())));
    }
    let key = aes_key();
    let iv = aes_iv();
    Aes256CbcDec::new(&key.into(), &iv.into())
        .decrypt_padded_vec_mut::<Pkcs7>(data)
        .map_err(|_| Error::Aes("invalid PKCS#7 padding (wrong key or not an AES cpk_list)".into()))
}

/// AES-256-CBC/PKCS#7-encrypt a T2B plaintext into the on-disk `cpk_list.cfg.bin` form.
pub fn encrypt(plain: &[u8]) -> Vec<u8> {
    let key = aes_key();
    let iv = aes_iv();
    Aes256CbcEnc::new(&key.into(), &iv.into()).encrypt_padded_vec_mut::<Pkcs7>(plain)
}

/// True if `plain` ends like a T2B cfg.bin (`01 74 32 62` at `len - 0x10`).
pub fn has_t2b_footer(plain: &[u8]) -> bool {
    plain.len() >= 0x10 && plain[plain.len() - 0x10..plain.len() - 0x0C] == T2B_FOOTER_MAGIC
}

/// How a cpk_list file was encoded on disk.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ListEncoding {
    /// Already plaintext T2B (Switch, or a decrypted copy).
    Plain,
    /// AES-256-CBC ("modern", IEVR PC).
    Aes,
    /// Filename XOR with [`LEGACY_XOR_KEY`] (older Level-5 PC games).
    LegacyXor,
}

/// Decode a cpk_list of unknown encoding, like Viola's `GetReadableCandidates`: plaintext, AES, legacy XOR.
///
/// A candidate is accepted when the result ends with the T2B footer.
pub fn decode(data: &[u8]) -> Result<(Vec<u8>, ListEncoding)> {
    if has_t2b_footer(data) {
        return Ok((data.to_vec(), ListEncoding::Plain));
    }
    if data.len().is_multiple_of(16)
        && let Ok(p) = decrypt(data)
        && has_t2b_footer(&p)
    {
        return Ok((p, ListEncoding::Aes));
    }
    let mut x = data.to_vec();
    crypt_in_place(LEGACY_XOR_KEY, 0, &mut x);
    if has_t2b_footer(&x) {
        return Ok((x, ListEncoding::LegacyXor));
    }
    Err(Error::Aes("cpk_list is neither plain T2B, AES nor legacy-XOR encoded".into()))
}

/// Re-encode a plaintext list with the given encoding.
pub fn encode(plain: &[u8], encoding: ListEncoding) -> Vec<u8> {
    match encoding {
        ListEncoding::Plain => plain.to_vec(),
        ListEncoding::Aes => encrypt(plain),
        ListEncoding::LegacyXor => {
            let mut x = plain.to_vec();
            crypt_in_place(LEGACY_XOR_KEY, 0, &mut x);
            x
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hex(b: &[u8]) -> String {
        b.iter().map(|x| format!("{x:02x}")).collect()
    }

    #[test]
    fn key_and_iv_match_docs() {
        assert_eq!(hex(&aes_key()), "99ad1240bac70843e461279d535c86d21f331ebd211bdbb0f7f3fb2b34ea3556");
        assert_eq!(hex(&aes_iv()), "c1a1e44478f0fbedf0e9828875425566");
    }

    #[test]
    fn roundtrip_and_detection() {
        let mut plain = vec![0u8; 64];
        plain.extend_from_slice(&[0x01, 0x74, 0x32, 0x62, 0xFE, 0x01, 0x01, 0x00, 0x01, 0x00, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF]);
        let enc = encrypt(&plain);
        assert_eq!(enc.len(), plain.len() + 16);
        assert_eq!(decrypt(&enc).unwrap(), plain);
        assert_eq!(decode(&enc).unwrap(), (plain.clone(), ListEncoding::Aes));
        assert_eq!(decode(&plain).unwrap().1, ListEncoding::Plain);
        let legacy = encode(&plain, ListEncoding::LegacyXor);
        assert_eq!(decode(&legacy).unwrap(), (plain, ListEncoding::LegacyXor));
    }

    #[test]
    fn garbage_is_an_error_not_a_panic() {
        assert!(decrypt(&[]).is_err());
        assert!(decrypt(&[1, 2, 3]).is_err());
        assert!(decode(&[0u8; 48]).is_err());
    }
}
