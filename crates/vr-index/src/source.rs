//! Reading the player's own game files: `data/cpk_list.cfg.bin` (AES) -> which CPK holds each file -> extract in
//! memory (XOR + CRILAYLA through `l5-cpk`). Nothing is extracted to disk.
//!
//! Retail first: the installed list is usually modded (mod files are listed as loose files and lose their CPK name),
//! so for a loose entry the retail copy is looked up in the CPK tables of contents (all 936 TOCs are scanned once,
//! only when a needed file is listed as loose). Loose files are only a fallback for files that exist in no CPK
//! (or every time with [`SourceOptions::include_mods`]).

use std::collections::{BTreeMap, HashMap};
use std::fs::File;
use std::io::BufReader;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use l5_cpk::cpk::CpkArchive;
use rayon::prelude::*;

use crate::error::{Error, Result};

/// CRC-32 of the record name `CPK_ITEM_BEGIN`.
const CRC_ITEM_BEGIN: u32 = 0xF0C5_583A;
/// CRC-32 of the record name `CPK_ITEM`.
const CRC_ITEM: u32 = 0x36F3_46D3;

/// One file of `cpk_list` (paths are logical, `data/...`).
#[derive(Debug, Clone)]
pub struct ListItem {
    /// Full logical path (`data/common/...`), original case.
    pub path: String,
    /// `data/packs/<hash>.cpk`, or `None` when the list says the file is loose.
    pub cpk: Option<String>,
    /// Extracted size.
    pub size: i32,
}

/// Where the bytes of a file are read from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Origin {
    /// Inside a CPK (retail).
    Cpk(String),
    /// Loose file under the game folder.
    Loose,
}

/// Options for [`GameSource::open`].
#[derive(Debug, Clone, Default)]
pub struct SourceOptions {
    /// Use this (decrypted or AES) list instead of `<game>/data/cpk_list.cfg.bin` (e.g. a pristine copy).
    pub cpk_list: Option<PathBuf>,
    /// Read loose (mod) copies of files the list marks as loose instead of the retail CPK copy.
    pub include_mods: bool,
}

/// Parse the AES-decrypted `cpk_list` (layout: docs/formats/cpk_list.md §3).
pub fn parse_cpk_list(b: &[u8]) -> Result<Vec<ListItem>> {
    let rd = |p: usize| -> Result<u32> {
        b.get(p..p + 4).map(|s| u32::from_le_bytes(s.try_into().unwrap())).ok_or_else(|| Error::Format(format!("cpk_list truncated at 0x{p:X}")))
    };
    if b.len() < 0x20 || rd(b.len() - 0x10)? != 0x6232_7401 {
        return Err(Error::Format("cpk_list: missing T2B footer (wrong key or not decrypted?)".into()));
    }
    let count = rd(0)? as usize;
    let str_off = rd(4)? as usize;
    let str_size = rd(8)? as usize;
    let pool = b.get(str_off..str_off + str_size).ok_or_else(|| Error::Format("cpk_list: string pool out of range".into()))?;
    let s = |o: i32| -> Option<&str> {
        if o < 0 {
            return None;
        }
        let t = pool.get(o as usize..)?;
        let end = t.iter().position(|&c| c == 0).unwrap_or(t.len());
        std::str::from_utf8(&t[..end]).ok()
    };
    let mut p = 16;
    let mut out = Vec::with_capacity(count);
    for _ in 0..count {
        let crc = rd(p)?;
        let n = *b.get(p + 4).ok_or_else(|| Error::Format("cpk_list: truncated record".into()))? as usize;
        p += 5;
        p = (p + n.div_ceil(4) + 3) & !3;
        let mut v = [0i32; 5];
        for (i, slot) in v.iter_mut().enumerate().take(n.min(5)) {
            *slot = rd(p + 4 * i)? as i32;
        }
        p += 4 * n;
        if crc == CRC_ITEM && n == 5 {
            let dir = s(v[0]).unwrap_or("");
            let name = s(v[1]).unwrap_or("");
            let cpk = match (s(v[2]), s(v[3])) {
                (_, None) | (_, Some("")) => None,
                (d, Some(n)) => Some(format!("{}{}", d.unwrap_or(""), n)),
            };
            out.push(ListItem { path: format!("{dir}{name}"), cpk, size: v[4] });
        } else if crc != CRC_ITEM_BEGIN {
            return Err(Error::Format(format!("cpk_list: unexpected record 0x{crc:08X}")));
        }
    }
    Ok(out)
}

/// The game's file system as the index sees it.
pub struct GameSource {
    pub game_dir: PathBuf,
    pub list_path: PathBuf,
    pub opts: SourceOptions,
    items: Vec<ListItem>,
    /// lower-case path -> item index
    by_path: HashMap<String, usize>,
    /// Retail TOC map (lower-case path -> cpk), built on first need.
    retail_toc: OnceLock<HashMap<String, String>>,
    /// Statistics: files read from CPKs / loose, TOC scan time.
    pub stats: std::sync::Mutex<ReadStats>,
}

/// Counters of what was read.
#[derive(Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ReadStats {
    pub from_cpk: usize,
    pub from_loose: usize,
    pub bytes: u64,
    pub cpks_opened: usize,
    pub toc_scan_ms: Option<u128>,
    /// Loose-listed files whose retail copy was found in a CPK.
    pub retail_redirects: usize,
    /// Files read loose (no CPK copy exists), first 50.
    #[serde(default)]
    pub loose_files: Vec<String>,
}

impl GameSource {
    /// Open the game folder (the one with `nie.exe` and `data/`).
    pub fn open(game_dir: impl AsRef<Path>, opts: SourceOptions) -> Result<Self> {
        let game_dir = game_dir.as_ref().to_path_buf();
        let list_path = opts.cpk_list.clone().unwrap_or_else(|| game_dir.join("data").join("cpk_list.cfg.bin"));
        let raw = std::fs::read(&list_path).map_err(|e| Error::io(&list_path, e))?;
        let plain = if l5_cpk::aes_list::has_t2b_footer(&raw) {
            raw
        } else {
            l5_cpk::aes_list::decode(&raw).map_err(|e| Error::Format(format!("{}: {e}", list_path.display())))?.0
        };
        let items = parse_cpk_list(&plain)?;
        let by_path = items.iter().enumerate().map(|(i, it)| (it.path.to_lowercase(), i)).collect();
        Ok(Self { game_dir, list_path, opts, items, by_path, retail_toc: OnceLock::new(), stats: Default::default() })
    }

    pub fn items(&self) -> &[ListItem] {
        &self.items
    }

    pub fn item(&self, path: &str) -> Option<&ListItem> {
        self.by_path.get(&path.to_lowercase()).map(|&i| &self.items[i])
    }

    /// Every listed path starting with `prefix` (case-insensitive), sorted.
    pub fn list_prefix(&self, prefix: &str) -> Vec<&ListItem> {
        let p = prefix.to_lowercase();
        let mut v: Vec<&ListItem> = self.items.iter().filter(|it| it.path.len() >= p.len() && it.path[..p.len()].eq_ignore_ascii_case(&p)).collect();
        v.sort_by(|a, b| a.path.cmp(&b.path));
        v
    }

    /// [`list_prefix`](Self::list_prefix) without mod-only files: entries that are in a CPK (or whose retail copy
    /// is), or every entry with [`SourceOptions::include_mods`]. Retail loose files outside CPKs (movies, app_config)
    /// are dropped too; the index never needs them.
    pub fn list_retail(&self, prefix: &str) -> Vec<&ListItem> {
        let mut v = self.list_prefix(prefix);
        if !self.opts.include_mods {
            v.retain(|it| it.cpk.is_some() || self.retail_toc().contains_key(&it.path.to_lowercase()));
        }
        v
    }

    /// [`item`](Self::item) when the file is retail (or mods are included).
    pub fn retail_item(&self, path: &str) -> Option<&ListItem> {
        self.item(path).filter(|it| it.cpk.is_some() || self.opts.include_mods || self.retail_toc().contains_key(&it.path.to_lowercase()))
    }

    /// Scan every `data/packs*/*.cpk` TOC once: lower-case path -> cpk path (relative to the game folder).
    fn retail_toc(&self) -> &HashMap<String, String> {
        self.retail_toc.get_or_init(|| {
            let t0 = std::time::Instant::now();
            let mut cpks: Vec<String> = self.items.iter().filter_map(|i| i.cpk.clone()).collect();
            cpks.sort();
            cpks.dedup();
            // CPKs no longer named by a modded list are still on disk.
            if let Ok(rd) = std::fs::read_dir(self.game_dir.join("data").join("packs")) {
                for e in rd.flatten() {
                    let n = e.file_name().to_string_lossy().into_owned();
                    if n.to_lowercase().ends_with(".cpk") {
                        let rel = format!("data/packs/{n}");
                        if cpks.binary_search(&rel).is_err() {
                            cpks.push(rel);
                        }
                    }
                }
            }
            let parts: Vec<Vec<(String, String)>> = cpks
                .par_iter()
                .map(|c| match CpkArchive::open(self.game_dir.join(c)) {
                    Ok(a) => a.entries().iter().map(|e| (e.path().to_lowercase(), c.clone())).collect(),
                    Err(_) => Vec::new(),
                })
                .collect();
            let mut m = HashMap::new();
            for p in parts {
                for (k, v) in p {
                    m.entry(k).or_insert(v);
                }
            }
            if let Ok(mut s) = self.stats.lock() {
                s.toc_scan_ms = Some(t0.elapsed().as_millis());
            }
            m
        })
    }

    /// Where `path` is read from (retail CPK preferred), or `None` if the game has no such file.
    pub fn origin(&self, path: &str) -> Option<Origin> {
        let it = self.item(path);
        if let Some(it) = it {
            if let Some(c) = &it.cpk {
                return Some(Origin::Cpk(c.clone()));
            }
            if self.opts.include_mods && self.game_dir.join(&it.path).is_file() {
                return Some(Origin::Loose);
            }
        }
        if let Some(c) = self.retail_toc().get(&path.to_lowercase()) {
            if let Ok(mut s) = self.stats.lock() {
                s.retail_redirects += 1;
            }
            return Some(Origin::Cpk(c.clone()));
        }
        let p = self.game_dir.join(it.map_or(path, |i| i.path.as_str()));
        p.is_file().then_some(Origin::Loose)
    }

    /// True when `path` exists in some retail CPK (listed there, or found in a TOC).
    pub fn is_retail(&self, path: &str) -> bool {
        matches!(self.origin(path), Some(Origin::Cpk(_)))
    }

    /// Read one file.
    pub fn read(&self, path: &str) -> Result<Vec<u8>> {
        let mut m = self.read_many(&[path.to_string()]);
        m.remove(path).unwrap_or_else(|| Err(Error::NotFound(path.into())))
    }

    /// Read many files at once: grouped by CPK, one archive open per CPK, CPKs in parallel.
    /// Result keys are the requested paths.
    pub fn read_many(&self, paths: &[String]) -> BTreeMap<String, Result<Vec<u8>>> {
        let mut by_cpk: BTreeMap<String, Vec<&String>> = BTreeMap::new();
        let mut loose: Vec<&String> = Vec::new();
        let mut out: BTreeMap<String, Result<Vec<u8>>> = BTreeMap::new();
        for p in paths {
            match self.origin(p) {
                Some(Origin::Cpk(c)) => by_cpk.entry(c).or_default().push(p),
                Some(Origin::Loose) => loose.push(p),
                None => {
                    out.insert(p.clone(), Err(Error::NotFound(p.clone())));
                }
            }
        }
        let groups: Vec<(String, Vec<&String>)> = by_cpk.into_iter().collect();
        let res: Vec<Vec<(String, Result<Vec<u8>>)>> = groups
            .par_iter()
            .map(|(cpk, files)| {
                let full = self.game_dir.join(cpk);
                let mut a: CpkArchive<BufReader<File>> = match CpkArchive::open(&full) {
                    Ok(a) => a,
                    Err(e) => return files.iter().map(|f| ((*f).clone(), Err(Error::Format(format!("{}: {e}", full.display()))))).collect(),
                };
                let idx: HashMap<String, usize> = a.entries().iter().enumerate().map(|(i, e)| (e.path().to_lowercase(), i)).collect();
                files
                    .iter()
                    .map(|f| {
                        let r = match idx.get(&f.to_lowercase()) {
                            Some(&i) => {
                                let e = a.entries()[i].clone();
                                a.extract(&e).map_err(|e| Error::Format(format!("{f} en {cpk}: {e}")))
                            }
                            None => Err(Error::NotFound(format!("{f} (no está en {cpk})"))),
                        };
                        ((*f).clone(), r)
                    })
                    .collect()
            })
            .collect();
        let mut st = ReadStats::default();
        st.cpks_opened = groups.len();
        for v in res {
            for (k, r) in v {
                if let Ok(b) = &r {
                    st.from_cpk += 1;
                    st.bytes += b.len() as u64;
                }
                out.insert(k, r);
            }
        }
        let lr: Vec<(String, Result<Vec<u8>>)> = loose
            .par_iter()
            .map(|p| {
                let real = self.item(p).map_or(p.as_str(), |i| i.path.as_str()).to_string();
                let fp = self.game_dir.join(&real);
                ((*p).clone(), std::fs::read(&fp).map_err(|e| Error::io(&fp, e)))
            })
            .collect();
        for (k, r) in lr {
            if let Ok(b) = &r {
                st.from_loose += 1;
                st.bytes += b.len() as u64;
                if st.loose_files.len() < 50 {
                    st.loose_files.push(k.clone());
                }
            }
            out.insert(k, r);
        }
        if let Ok(mut s) = self.stats.lock() {
            s.from_cpk += st.from_cpk;
            s.from_loose += st.from_loose;
            s.bytes += st.bytes;
            s.cpks_opened += st.cpks_opened;
            for f in st.loose_files {
                if s.loose_files.len() < 50 {
                    s.loose_files.push(f);
                }
            }
        }
        out
    }

    /// Latest retail version of a versioned table: `dir` + `stem_<ver>.cfg.bin` (e.g. `chara_base_1.03.98.00`).
    /// Versions compare numerically; retail (CPK) copies win over mod-only files.
    pub fn latest(&self, dir: &str, stem: &str) -> Option<String> {
        let prefix = format!("{dir}{stem}_");
        let mut best: Option<(bool, Vec<u32>, String)> = None;
        for it in self.list_prefix(&prefix) {
            let rest = &it.path[prefix.len()..];
            let Some(ver) = rest.strip_suffix(".cfg.bin") else { continue };
            if ver.is_empty() || !ver.chars().all(|c| c.is_ascii_digit() || c == '.') {
                continue;
            }
            let v: Vec<u32> = ver.split('.').filter_map(|x| x.parse().ok()).collect();
            let retail = it.cpk.is_some() || self.opts.include_mods || self.is_retail(&it.path);
            let key = (retail, v, it.path.clone());
            if best.as_ref().is_none_or(|b| (key.0, &key.1) > (b.0, &b.1)) {
                best = Some(key);
            }
        }
        best.map(|b| b.2).or_else(|| {
            // Unversioned file.
            let p = format!("{dir}{stem}.cfg.bin");
            self.item(&p).map(|i| i.path.clone())
        })
    }

    /// Cheap fingerprint of the inputs: list + exe + packs folder (sizes and mtimes).
    pub fn fingerprint(&self) -> String {
        let mut h = crc32fast::Hasher::new();
        let mut add = |p: &Path| {
            if let Ok(m) = std::fs::metadata(p) {
                h.update(p.to_string_lossy().as_bytes());
                h.update(&m.len().to_le_bytes());
                let t = m.modified().ok().and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok()).map_or(0, |d| d.as_secs());
                h.update(&t.to_le_bytes());
            }
        };
        add(&self.list_path);
        add(&self.game_dir.join("nie.exe"));
        let packs = self.game_dir.join("data").join("packs");
        add(&packs);
        let mut n = 0u64;
        let mut total = 0u64;
        if let Ok(rd) = std::fs::read_dir(&packs) {
            for e in rd.flatten() {
                if let Ok(m) = e.metadata() {
                    n += 1;
                    total = total.wrapping_add(m.len());
                }
            }
        }
        h.update(&n.to_le_bytes());
        h.update(&total.to_le_bytes());
        format!("{:08x}-{}-{}", h.finalize(), n, total)
    }
}

/// Steam build id of the install (`steamapps/appmanifest_2799860.acf`), when found.
pub fn steam_build_id(game_dir: &Path) -> Option<u64> {
    // <lib>/steamapps/common/<game> -> <lib>/steamapps/appmanifest_2799860.acf
    let steamapps = game_dir.parent()?.parent()?;
    let txt = std::fs::read_to_string(steamapps.join("appmanifest_2799860.acf")).ok()?;
    let line = txt.lines().find(|l| l.trim_start().starts_with("\"buildid\""))?;
    line.split('"').filter(|s| !s.trim().is_empty()).nth(1)?.parse().ok()
}

/// Human game version for a Steam build id.
pub fn game_version_of_build(build: u64) -> Option<&'static str> {
    match build {
        24370575 => Some("7.1.2"),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Minimal plaintext list in the Level-5 layout (docs/formats/cpk_list.md §3).
    fn list_bytes(items: &[(&str, &str, Option<(&str, &str)>, i32)]) -> Vec<u8> {
        let mut pool: Vec<u8> = Vec::new();
        let mut intern = |s: &str| -> i32 {
            let o = pool.len() as i32;
            pool.extend_from_slice(s.as_bytes());
            pool.push(0);
            o
        };
        let refs: Vec<[i32; 5]> = items
            .iter()
            .map(|(d, n, c, size)| {
                let (cd, cn) = c.map_or((-1, -1), |(a, b)| (intern(a), intern(b)));
                [intern(d), intern(n), cd, cn, *size]
            })
            .collect();
        let str_off = (16 + 12 + 28 * items.len()).div_ceil(16) * 16;
        let mut out = Vec::new();
        out.extend_from_slice(&((items.len() + 1) as u32).to_le_bytes());
        out.extend_from_slice(&(str_off as u32).to_le_bytes());
        out.extend_from_slice(&(pool.len() as u32).to_le_bytes());
        out.extend_from_slice(&0u32.to_le_bytes());
        out.extend_from_slice(&CRC_ITEM_BEGIN.to_le_bytes());
        out.extend_from_slice(&[1, 0x01, 0xFF, 0xFF]);
        out.extend_from_slice(&(items.len() as i32).to_le_bytes());
        for r in &refs {
            out.extend_from_slice(&CRC_ITEM.to_le_bytes());
            out.extend_from_slice(&[5, 0x00, 0x01, 0xFF]);
            for v in r {
                out.extend_from_slice(&v.to_le_bytes());
            }
        }
        out.resize(str_off, 0xFF);
        out.extend_from_slice(&pool);
        out.resize(out.len().div_ceil(16) * 16, 0xFF);
        out.extend_from_slice(&[0x01, 0x74, 0x32, 0x62, 0xFE, 0x01, 0x01, 0x00, 0x01, 0x00, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF]);
        out
    }

    #[test]
    fn parse_list() {
        let b = list_bytes(&[
            ("data/common/gamedata/character/", "chara_base_1.03.98.00.cfg.bin", Some(("data/packs/", "abc.cpk")), 100),
            ("data/common/gamedata/character/", "chara_base_1.03.99.00.cfg.bin", None, 7),
        ]);
        let items = parse_cpk_list(&b).unwrap();
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].path, "data/common/gamedata/character/chara_base_1.03.98.00.cfg.bin");
        assert_eq!(items[0].cpk.as_deref(), Some("data/packs/abc.cpk"));
        assert_eq!(items[1].cpk, None);
        assert_eq!(items[1].size, 7);
        // the AES layer round-trips through l5-cpk
        let enc = l5_cpk::aes_list::encrypt(&b);
        assert_eq!(parse_cpk_list(&l5_cpk::aes_list::decode(&enc).unwrap().0).unwrap().len(), 2);
        assert!(parse_cpk_list(&b[..b.len() - 16]).is_err());
    }
}
