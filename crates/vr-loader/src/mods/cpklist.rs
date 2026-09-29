//! In-memory `cpk_list` entries for the files the overlay serves (docs/game/engine/mod-format.md, «Por qué el
//! overlay solo no basta»). Pure logic over a snapshot of the engine's list; `hooks` reads the snapshot from the
//! `CCriFileOperate` and publishes the result.
//!
//! Why: on an overlay hit `CCriFileOperate::Open 0x4E70C0` skips the cpk_list lookup (`this+0x170 == 0`), so the
//! handle gets no size (`handle+0x134 = -1`, no `criFsLoader_SetReadUnitSize`) and every later size query has to
//! wait for the asynchronous CRI `BindFile` of the file. A file the app installs loose has a cpk_list record: the
//! handle gets the size at open time. With `this+0x170 = 1` the engine looks the **rewritten** path up in cpk_list
//! too, so the loader registers every served file as a loose record under its root-relative path
//! (`mods/<id>/files/data/...`) and hands that path to the engine: `Open` then runs exactly the branch of an
//! app-installed loose file (record found, size from the record, `BindFile` of `<root>/<path>`).
//!
//! Runtime record (0x1C bytes, built by the list loader at `0x1765B08`, read by `FindCpkListEntry 0x4E83D0`):
//! `{dir, name, cpk_dir, cpk_name}` = offsets into the string pool (`-1` = null), `crc` = crc32(dir+name) (the
//! array is sorted by it, unsigned), `cpk_crc` = crc32(cpk_dir+cpk_name) or 0, `size`. A loose record has
//! `cpk_dir = cpk_name = -1` and `cpk_crc = 0` (the loader normalises both null and `""` to that).

/// Null string offset.
pub const NULL: u32 = u32::MAX;

/// One runtime cpk_list record (layout of the engine's array, `CCriFileOperate+0x128`).
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rec {
    pub dir: u32,
    pub name: u32,
    pub cpk_dir: u32,
    pub cpk_name: u32,
    pub crc: u32,
    pub cpk_crc: u32,
    pub size: i32,
}

const _: () = assert!(std::mem::size_of::<Rec>() == 0x1C);

impl Rec {
    /// A loose record as the engine's list loader stores one.
    pub fn loose(dir: u32, name: u32, crc: u32, size: i32) -> Rec {
        Rec { dir, name, cpk_dir: NULL, cpk_name: NULL, crc, cpk_crc: 0, size }
    }

    pub fn is_loose(&self) -> bool {
        self.cpk_name == NULL
    }
}

/// Length of the string pool prefix the records use: end (NUL included) of the string at the highest offset.
/// `strlen_at(off)` = length of the pool string at `off`, None when unreadable. None when a record has no string at
/// all (an empty list).
pub fn used_pool_len(recs: &[Rec], strlen_at: impl Fn(u32) -> Option<usize>) -> Option<usize> {
    let max = recs.iter().flat_map(|r| [r.dir, r.name, r.cpk_dir, r.cpk_name]).filter(|&o| o != NULL).max()?;
    Some(max as usize + strlen_at(max)? + 1)
}

/// `abs` relative to the engine's data root `root` (`CCriFileOperate+0x18`), `/` separators, the case of `abs`
/// kept. None when `abs` is not below `root`. `root` is compared without case and may end in `/`.
pub fn root_relative(abs: &str, root: &str) -> Option<String> {
    let a = abs.replace('\\', "/");
    let r = root.replace('\\', "/");
    let r = r.trim_end_matches('/');
    if r.is_empty() || a.len() <= r.len() + 1 || !a.is_char_boundary(r.len()) {
        return None;
    }
    let (head, tail) = a.split_at(r.len());
    if !head.eq_ignore_ascii_case(r) || !tail.starts_with('/') {
        return None;
    }
    let rel = tail.trim_start_matches('/');
    (!rel.is_empty()).then(|| rel.to_string())
}

fn cstr(pool: &[u8], off: u32) -> Option<&[u8]> {
    if off == NULL {
        return None;
    }
    let s = pool.get(off as usize..)?;
    Some(&s[..s.iter().position(|&b| b == 0)?])
}

/// What `FindCpkListEntry 0x4E83D0` returns for `path` (already root-relative): binary search on the crc
/// (unsigned), back to the first equal crc, then the first record whose `dir` is a prefix of `path` and whose
/// `name` is the rest.
pub fn find(pool: &[u8], recs: &[Rec], path: &str) -> Option<usize> {
    let p = path.as_bytes();
    let crc = crc32fast::hash(p);
    let mut i = recs.partition_point(|r| r.crc < crc);
    while i < recs.len() && recs[i].crc == crc {
        let r = &recs[i];
        let dir = cstr(pool, r.dir).unwrap_or(b"");
        let name = cstr(pool, r.name);
        if p.starts_with(dir) && name.is_some_and(|n| n == &p[dir.len()..]) {
            return Some(i);
        }
        i += 1;
    }
    None
}

/// Split a root-relative path the way cpk_list does (`dir` keeps the trailing `/`).
fn split(path: &str) -> (&str, &str) {
    match path.rfind('/') {
        Some(i) => (&path[..=i], &path[i + 1..]),
        None => ("", path),
    }
}

/// New string pool and records with every `(root-relative path, size)` of `adds` registered as a loose record.
/// The pool keeps `pool` byte for byte (old offsets stay valid) and appends the new strings; the records stay sorted
/// by crc (new ones after existing equal crcs). A path the list already has is turned loose with the new size
/// instead of being added twice.
pub fn with_loose_entries(pool: &[u8], recs: &[Rec], adds: &[(String, i32)]) -> (Vec<u8>, Vec<Rec>) {
    let mut new_pool = pool.to_vec();
    let mut out = recs.to_vec();
    let mut interned: std::collections::HashMap<String, u32> = std::collections::HashMap::new();
    let mut intern = |s: &str, new_pool: &mut Vec<u8>| -> u32 {
        *interned.entry(s.to_string()).or_insert_with(|| {
            let o = new_pool.len() as u32;
            new_pool.extend_from_slice(s.as_bytes());
            new_pool.push(0);
            o
        })
    };
    let mut added = false;
    for (path, size) in adds {
        if let Some(i) = find(pool, recs, path) {
            let r = &mut out[i];
            *r = Rec::loose(r.dir, r.name, r.crc, *size);
            continue;
        }
        let (dir, name) = split(path);
        let d = if dir.is_empty() { NULL } else { intern(dir, &mut new_pool) };
        let n = intern(name, &mut new_pool);
        out.push(Rec::loose(d, n, crc32fast::hash(path.as_bytes()), *size));
        added = true;
    }
    if added {
        out.sort_by_key(|r| r.crc); // stable: existing records keep their order among equal crcs
    }
    (new_pool, out)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A small list the way the engine holds it: one CPK record, one loose record, pool with shared dirs.
    fn sample() -> (Vec<u8>, Vec<Rec>) {
        let mut pool = Vec::new();
        let mut put = |s: &str| {
            let o = pool.len() as u32;
            pool.extend_from_slice(s.as_bytes());
            pool.push(0);
            o
        };
        let face = put("data/dx11/menu/200_icon/10_icon_chr/face/");
        let mark = put("c01000010_l.g4tx");
        let packs = put("data/packs/");
        let cpk = put("e832856918ebb97cb4430f715e6bd525.cpk");
        let movie = put("data/dx11/movie/");
        let logo = put("L5logo.usm");
        let crc = |s: &str| crc32fast::hash(s.as_bytes());
        let mut recs = vec![
            Rec {
                dir: face,
                name: mark,
                cpk_dir: packs,
                cpk_name: cpk,
                crc: crc("data/dx11/menu/200_icon/10_icon_chr/face/c01000010_l.g4tx"),
                cpk_crc: crc("data/packs/e832856918ebb97cb4430f715e6bd525.cpk"),
                size: 131_648,
            },
            Rec::loose(movie, logo, crc("data/dx11/movie/L5logo.usm"), 3_278_304),
        ];
        recs.sort_by_key(|r| r.crc);
        (pool, recs)
    }

    #[test]
    fn record_layout_is_the_engines() {
        assert_eq!(std::mem::size_of::<Rec>(), 0x1C);
        assert_eq!(std::mem::offset_of!(Rec, crc), 0x10);
        assert_eq!(std::mem::offset_of!(Rec, cpk_crc), 0x14);
        assert_eq!(std::mem::offset_of!(Rec, size), 0x18);
    }

    #[test]
    fn pool_prefix_ends_after_the_last_string() {
        let (pool, recs) = sample();
        let strlen = |o: u32| cstr(&pool, o).map(|s| s.len());
        assert_eq!(used_pool_len(&recs, strlen), Some(pool.len()));
        assert_eq!(used_pool_len(&[], strlen), None);
    }

    #[test]
    fn root_relative_paths() {
        let root = "D:/SteamLibrary/steamapps/common/INAZUMA ELEVEN Victory Road";
        let abs = "D:/SteamLibrary/steamapps/common/INAZUMA ELEVEN Victory Road/mods/evt_test_blond_mark/files/data/dx11/x.g4tx";
        assert_eq!(root_relative(abs, root).as_deref(), Some("mods/evt_test_blond_mark/files/data/dx11/x.g4tx"));
        // case / separators / trailing slash of the root do not matter; the case of the file path is kept
        assert_eq!(
            root_relative(&abs.replace('/', "\\"), "d:\\steamlibrary\\steamapps\\common\\inazuma eleven victory road\\").as_deref(),
            Some("mods/evt_test_blond_mark/files/data/dx11/x.g4tx")
        );
        assert!(root_relative("D:/Other/mods/a/files/data/x", root).is_none());
        assert!(root_relative(&format!("{root}x/mods/a"), root).is_none()); // same prefix, other folder
        assert!(root_relative(root, root).is_none());
        assert!(root_relative(abs, "").is_none());
    }

    #[test]
    fn find_matches_the_engine_lookup() {
        let (pool, recs) = sample();
        let i = find(&pool, &recs, "data/dx11/menu/200_icon/10_icon_chr/face/c01000010_l.g4tx").unwrap();
        assert_eq!(recs[i].size, 131_648);
        assert!(find(&pool, &recs, "data/dx11/movie/L5logo.usm").is_some_and(|i| recs[i].is_loose()));
        assert!(find(&pool, &recs, "data/dx11/movie/l5logo.usm").is_none()); // exact bytes, like the engine
        assert!(find(&pool, &recs, "data/nope.bin").is_none());
    }

    #[test]
    fn served_files_become_loose_records_under_their_mod_path() {
        let (pool, recs) = sample();
        let rel = "mods/evt_test_blond_mark/files/data/dx11/menu/200_icon/10_icon_chr/face/c01000010_l.g4tx";
        let voice = "mods/pack/files/data/common/sound_asset/es/c01000010.acb";
        let adds = vec![(rel.to_string(), 131_648), (voice.to_string(), 4_096)];
        let (np, nr) = with_loose_entries(&pool, &recs, &adds);
        // old strings keep their offsets, old records are all still found
        assert_eq!(&np[..pool.len()], &pool[..]);
        assert_eq!(nr.len(), recs.len() + 2);
        assert!(nr.windows(2).all(|w| w[0].crc <= w[1].crc));
        for r in &recs {
            let p = format!(
                "{}{}",
                String::from_utf8_lossy(cstr(&pool, r.dir).unwrap()),
                String::from_utf8_lossy(cstr(&pool, r.name).unwrap())
            );
            assert_eq!(nr[find(&np, &nr, &p).unwrap()], *r);
        }
        // the new ones: loose, the size of the mod file, the crc the engine computes for the path it gets
        let r = nr[find(&np, &nr, rel).unwrap()];
        assert_eq!((r.cpk_dir, r.cpk_name, r.cpk_crc, r.size), (NULL, NULL, 0, 131_648));
        assert_eq!(r.crc, crc32fast::hash(rel.as_bytes()));
        assert_eq!(cstr(&np, r.dir).unwrap(), b"mods/evt_test_blond_mark/files/data/dx11/menu/200_icon/10_icon_chr/face/");
        assert_eq!(nr[find(&np, &nr, voice).unwrap()].size, 4_096);
        // registering again (a reloaded list that already has them) turns nothing into duplicates
        let (np2, nr2) = with_loose_entries(&np, &nr, &adds);
        assert_eq!((np2.len(), nr2.len()), (np.len(), nr.len()));
    }

    #[test]
    fn a_path_already_in_the_list_is_switched_to_loose() {
        let (pool, recs) = sample();
        let p = "data/dx11/menu/200_icon/10_icon_chr/face/c01000010_l.g4tx";
        let (np, nr) = with_loose_entries(&pool, &recs, &[(p.to_string(), 99)]);
        assert_eq!((np.len(), nr.len()), (pool.len(), recs.len()));
        let r = nr[find(&np, &nr, p).unwrap()];
        assert!(r.is_loose());
        assert_eq!((r.cpk_crc, r.size), (0, 99));
    }
}
