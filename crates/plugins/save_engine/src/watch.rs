//! Fallback save detection (pure): without `save.*` events (a loader that does not publish them, e.g. the public
//! VR-ModLoader today) the plugin watches the active slot's retail file in Steam's cloud folder
//! (`<Steam>\userdata\<account>\2799860\remote\002AB8F4-USERDATALIVE[_<n>]`, docs/formats/save.md §1). Steam writes
//! that file when the game saves, so a new size / modification time = the game saved the slot; a file that
//! disappears = the slot's save was removed. Copies between slots are not seen this way (they only exist with the
//! save_slots module, which publishes events).

use std::path::Path;
use std::time::SystemTime;

/// Size and modification time of a file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Stamp {
    pub len: u64,
    pub modified: Option<SystemTime>,
}

pub fn stamp(path: &Path) -> Option<Stamp> {
    let m = std::fs::metadata(path).ok()?;
    m.is_file().then(|| Stamp { len: m.len(), modified: m.modified().ok() })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Seen {
    Nothing,
    /// The slot's file was written (or created by the first save of a new game).
    Saved,
    /// The slot's file is gone.
    Gone,
}

/// Watch of the active slot's file.
#[derive(Debug, Default)]
pub struct FileWatch {
    slot: Option<u8>,
    last: Option<Stamp>,
}

impl FileWatch {
    /// One poll: `slot` = active slot, `cur` = its file now. A new slot only takes the baseline.
    pub fn step(&mut self, slot: u8, cur: Option<Stamp>) -> Seen {
        if self.slot != Some(slot) {
            self.slot = Some(slot);
            self.last = cur;
            return Seen::Nothing;
        }
        let prev = std::mem::replace(&mut self.last, cur);
        match (prev, cur) {
            (Some(a), Some(b)) if a != b => Seen::Saved,
            (None, Some(_)) => Seen::Saved,
            (Some(_), None) => Seen::Gone,
            _ => Seen::Nothing,
        }
    }
}

/// Steam's cloud folder of the game for `account` under `steam_root`.
pub fn remote_dir(steam_root: &Path, account: u32) -> std::path::PathBuf {
    steam_root.join("userdata").join(account.to_string()).join("2799860").join("remote")
}

/// Pick the cloud folder: the active Steam account's when it holds the game's files, else the only / newest
/// `userdata\*\2799860\remote` that holds `002AB8F4-SYSTEMLIVE` (written at every boot).
pub fn find_remote_dir(steam_root: &Path, active_account: Option<u32>) -> Option<std::path::PathBuf> {
    const MARK: &str = "002AB8F4-SYSTEMLIVE";
    if let Some(a) = active_account.filter(|&a| a != 0) {
        let d = remote_dir(steam_root, a);
        if d.join(MARK).is_file() {
            return Some(d);
        }
    }
    let rd = std::fs::read_dir(steam_root.join("userdata")).ok()?;
    rd.flatten()
        .map(|e| e.path().join("2799860").join("remote"))
        .filter_map(|d| {
            let t = std::fs::metadata(d.join(MARK)).ok()?.modified().ok()?;
            Some((t, d))
        })
        .max_by_key(|(t, _)| *t)
        .map(|(_, d)| d)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn st(len: u64, s: u64) -> Option<Stamp> {
        Some(Stamp { len, modified: Some(SystemTime::UNIX_EPOCH + Duration::from_secs(s)) })
    }

    #[test]
    fn saves_and_deletes_of_the_active_slot() {
        let mut w = FileWatch::default();
        assert_eq!(w.step(2, st(10, 1)), Seen::Nothing, "baseline");
        assert_eq!(w.step(2, st(10, 1)), Seen::Nothing);
        assert_eq!(w.step(2, st(10, 2)), Seen::Saved, "same size, newer time");
        assert_eq!(w.step(2, st(11, 2)), Seen::Saved, "new size");
        assert_eq!(w.step(2, None), Seen::Gone);
        assert_eq!(w.step(2, None), Seen::Nothing);
        assert_eq!(w.step(2, st(5, 9)), Seen::Saved, "first save of a new game");
        assert_eq!(w.step(3, None), Seen::Nothing, "switch: baseline of the new slot");
        assert_eq!(w.step(3, st(5, 9)), Seen::Saved);
    }

    #[test]
    fn remote_folder_is_found_in_a_fake_steam_tree() {
        let root = std::env::temp_dir().join(format!("save-engine-steam-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        assert_eq!(find_remote_dir(&root, Some(5)), None);
        for acc in [5u32, 6] {
            std::fs::create_dir_all(remote_dir(&root, acc)).unwrap();
        }
        std::fs::write(remote_dir(&root, 6).join("002AB8F4-SYSTEMLIVE"), b"x").unwrap();
        assert_eq!(find_remote_dir(&root, Some(5)), Some(remote_dir(&root, 6)), "account 5 has no game files");
        assert_eq!(find_remote_dir(&root, None), Some(remote_dir(&root, 6)));
        std::fs::write(remote_dir(&root, 5).join("002AB8F4-SYSTEMLIVE"), b"x").unwrap();
        assert_eq!(find_remote_dir(&root, Some(5)), Some(remote_dir(&root, 5)));
        let _ = std::fs::remove_dir_all(&root);
    }
}
