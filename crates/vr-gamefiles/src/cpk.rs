//! `cpk_list.cfg.bin` encryption and CPK extraction, on top of `l5-cpk`.

use std::collections::HashMap;
use std::fs::File;
use std::io::BufReader;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

use l5_cpk::cpk::CpkArchive;

use crate::cpk_list::CpkList;
use crate::error::{Error, IoContext, Result};

/// Read and decrypt `cpk_list.cfg.bin` (AES, plain, or legacy XOR).
pub fn read_list(path: &Path) -> Result<CpkList> {
    let raw = std::fs::read(path).at(path)?;
    let (plain, _) = l5_cpk::aes_list::decode(&raw).map_err(|e| Error::Format(format!("{}: {e}", path.display())))?;
    CpkList::parse(&plain)
}

/// Serialize, AES-encrypt and write `cpk_list.cfg.bin` atomically.
pub fn write_list(path: &Path, list: &CpkList) -> Result<()> {
    let bytes = l5_cpk::aes_list::encrypt(&list.to_bytes());
    let tmp = path.with_extension("bin.tmp");
    std::fs::write(&tmp, &bytes).at(&tmp)?;
    std::fs::rename(&tmp, path).at(path)
}

type Archive = CpkArchive<BufReader<File>>;

/// Open CPKs keep their parsed TOC; the game has 936 of them, so cap the cache.
fn cache() -> &'static Mutex<HashMap<PathBuf, Archive>> {
    static CACHE: OnceLock<Mutex<HashMap<PathBuf, Archive>>> = OnceLock::new();
    CACHE.get_or_init(Default::default)
}

const MAX_OPEN_CPKS: usize = 32;

/// Extract `dir + name` from the CPK at `cpk` (decrypting and decompressing).
pub fn extract_from_cpk(cpk: &Path, dir: &str, name: &str) -> Result<Vec<u8>> {
    let mut cache = cache().lock().unwrap_or_else(|e| e.into_inner());
    if !cache.contains_key(cpk) {
        if cache.len() >= MAX_OPEN_CPKS {
            cache.clear();
        }
        let a = CpkArchive::open(cpk).map_err(|e| Error::Format(format!("{}: {e}", cpk.display())))?;
        cache.insert(cpk.to_path_buf(), a);
    }
    let a = cache.get_mut(cpk).expect("inserted above");
    let path = format!("{dir}{name}");
    a.extract_path(&path)
        .or_else(|_| a.extract_path(name))
        .map_err(|e| Error::Format(format!("{path} en {}: {e}", cpk.display())))
}
