//! File helpers: atomic writes and game keys (`data/...`, `/`-separated) as paths under a folder.

use std::io::Write;
use std::path::{Path, PathBuf};

/// Write `bytes` to `path` atomically (parent folders created): `<path>.tmp`, flushed to disk, renamed over the old
/// file (Windows: `MoveFileExW(REPLACE_EXISTING)`). A crash leaves either the old or the new file, never half of one.
pub fn write_atomic(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    if let Some(d) = path.parent() {
        std::fs::create_dir_all(d)?;
    }
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(".tmp");
    let tmp = PathBuf::from(tmp);
    {
        let mut f = std::fs::File::create(&tmp)?;
        f.write_all(bytes)?;
        f.sync_all()?;
    }
    std::fs::rename(&tmp, path).inspect_err(|_| {
        let _ = std::fs::remove_file(&tmp);
    })
}

/// Seconds since the Unix epoch (0 when the clock is before it).
pub fn unix_now() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

/// `root` + the `/`-separated parts of `key` (`data/common/x.cfg.bin` → `<root>\data\common\x.cfg.bin`).
pub fn key_path(root: &Path, key: &str) -> PathBuf {
    let mut p = root.to_path_buf();
    for part in key.split('/') {
        p.push(part);
    }
    p
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn atomic_write_replaces_and_leaves_no_tmp() {
        let d = std::env::temp_dir().join(format!("vr-fw-atomic-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        let p = d.join("a").join("x.json");
        write_atomic(&p, b"one").unwrap();
        write_atomic(&p, b"two").unwrap();
        assert_eq!(std::fs::read(&p).unwrap(), b"two");
        assert!(!d.join("a").join("x.json.tmp").exists());
        assert_eq!(key_path(&d, "data/x/y.bin"), d.join("data").join("x").join("y.bin"));
        let _ = std::fs::remove_dir_all(&d);
    }
}
