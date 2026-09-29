//! Version gate: the loader only acts inside `nie.exe` PC v7.1.2.
//!
//! * quick check (safe in DllMain): host file name `nie.exe` + PE `TimeDateStamp` / `SizeOfImage` of the mapped
//!   image;
//! * full check (init thread): SHA-1 of the exe file on disk.

pub const EXE_NAME: &str = "nie.exe";
pub const V712_TIME_DATE_STAMP: u32 = 0x6A63_2640;
pub const V712_SIZE_OF_IMAGE: u32 = 0x027C_A000;
pub const V712_SHA1: &str = "d27e76217730783fec8df4a3b0541cb15fe8f100";

pub fn header_matches(time_date_stamp: u32, size_of_image: u32) -> bool {
    time_date_stamp == V712_TIME_DATE_STAMP && size_of_image == V712_SIZE_OF_IMAGE
}

pub fn sha1_file(path: &std::path::Path) -> std::io::Result<String> {
    use sha1::{Digest, Sha1};
    use std::io::Read;
    let mut f = std::fs::File::open(path)?;
    let mut h = Sha1::new();
    let mut buf = vec![0u8; 1 << 20];
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
    }
    Ok(h.finalize().iter().map(|b| format!("{b:02x}")).collect())
}
