//! Data deltas of module `mods` (`<mod>/data/<table>.toml`, docs/game/engine/mod-format.md «Deltas»): at boot every
//! game table file touched by an active mod's `[[set]]` / `[[add]]` is rebuilt ONCE with the cells of every mod, and
//! the result is served through the overlay like a whole-file override. So two mods can change different cells (or
//! rows) of `chara_param` and both changes reach the game.
//!
//! * **Table -> file**: [`super::tables`] (index generated from `schemas/`): the table's file globs matched against
//!   the cpk_list keys, highest version; T2B tables of one file only (per-map / per-language tables and RDBN are
//!   refused with a WARN).
//! * **Base**: the winning whole-file override of that key (a mod's `files/data/...`), else the file the game would
//!   read: cpk_list record -> loose file under the game folder, or extracted from its CPK (decrypted and decompressed
//!   by `vr_gamefiles::cpk`; loose cfg.bin files are plain).
//! * **Apply**: first every `[[add]]` of every mod (load order), then every `[[set]]` (load order): the mod that loads
//!   later wins a cell / row both change (WARN with both values). Rows are matched by the table's key column
//!   ([`evt_modfmt::RowKey`]); a new row goes after the last row of its list (cloned from `from`, else the first row's
//!   shape with zero / empty values), the list's `_LIST_BEG` count is raised and the `__SORT_INDEX` rebuilt.
//! * **Cache**: `evt_loader/cache/merged/data/...` + `manifest.json`. The manifest keeps a hash of the inputs (delta
//!   texts, mod order, whole-file overrides, table index, merge version) and the size + mtime of every file the build
//!   read (cpk_list, base file / CPK, override): same inputs and unchanged files = the cached files are served without
//!   reading cpk_list or any table.

use super::tables::{self, TableInfo};
use super::Lvl;
use evt_modfmt::{AddOp, Cell, LoadPlan, RowKey, SetOp};
use l5_core::t2b::{build_tree, Entry, Node, T2b, TreeMode, Value};
use serde::{Deserialize, Serialize};
use sha1::{Digest, Sha1};
use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};

/// Bump when the merge rules change (invalidates every cache).
pub const MERGE_VERSION: u32 = 1;
/// Cache folder, under `evt_loader`.
pub const CACHE_REL: &str = "cache/merged";
const MANIFEST: &str = "manifest.json";

type Notes = Vec<(Lvl, String)>;

/// The ops of one mod on one game file.
#[derive(Debug, Clone, Default)]
pub struct ModOps<'a> {
    pub id: &'a str,
    pub sets: Vec<&'a SetOp>,
    pub adds: Vec<&'a AddOp>,
}

/// Row identity: the key cell's value.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
enum KeyVal {
    I(u32),
    S(String),
}

fn key_of(e: &Entry, k: usize) -> Option<KeyVal> {
    match e.values.get(k)? {
        Value::Int(v) => Some(KeyVal::I(*v as u32)),
        Value::String(Some(s)) => Some(KeyVal::S(s.clone())),
        _ => None,
    }
}

/// Candidates a delta key can match, in order (a name: text key first, then its crc32).
fn key_candidates(k: &RowKey) -> Vec<KeyVal> {
    match k {
        RowKey::Num(n) => vec![KeyVal::I(*n)],
        RowKey::Name(s) => vec![KeyVal::S(s.clone()), KeyVal::I(k.hash())],
    }
}

/// Where a table's rows are: the span (row entry ..= last child) of every row and the list's BEGIN entry.
struct ListLoc {
    spans: Vec<(usize, usize)>,
    begin: Option<usize>,
}

fn find_rows(nodes: &[Node], entries: &[Entry], hash: u32, parent: Option<usize>) -> Option<ListLoc> {
    let spans: Vec<(usize, usize)> = nodes.iter().filter(|n| entries[n.entry].hash == hash).map(|n| (n.entry, n.last_index())).collect();
    if !spans.is_empty() {
        return Some(ListLoc { spans, begin: parent });
    }
    nodes.iter().find_map(|n| find_rows(&n.children, entries, hash, Some(n.entry)))
}

fn locate(t: &T2b, info: &TableInfo) -> Result<ListLoc, String> {
    let hash = t.hash_name(&info.list).map_err(|e| e.to_string())?;
    let tree = build_tree(&t.entries, TreeMode::Counted);
    find_rows(&tree, &t.entries, hash, None).ok_or_else(|| format!("no `{}` rows in the file", info.list))
}

fn key_map(t: &T2b, loc: &ListLoc, k: usize) -> HashMap<KeyVal, usize> {
    let mut m = HashMap::with_capacity(loc.spans.len());
    for (i, &(s, _)) in loc.spans.iter().enumerate() {
        if let Some(kv) = key_of(&t.entries[s], k) {
            m.entry(kv).or_insert(i);
        }
    }
    m
}

fn lookup<T: Copy>(m: &HashMap<KeyVal, T>, k: &RowKey) -> Option<T> {
    key_candidates(k).iter().find_map(|c| m.get(c).copied())
}

/// `c` written over a cell that holds `old` (the cell keeps its type).
fn convert(old: &Value, c: &Cell) -> Result<Value, String> {
    Ok(match (old, c) {
        (Value::Int(_), Cell::Int(v)) => {
            if !(i32::MIN as i64..=u32::MAX as i64).contains(v) {
                return Err(format!("{v} does not fit 32 bits"));
            }
            Value::Int(*v as u32 as i32)
        }
        // a name in a hash cell: its crc32 (like row keys)
        (Value::Int(_), Cell::Str(s)) => Value::Int(evt_modfmt::crc32(s.as_bytes()) as i32),
        (Value::Float(_), Cell::Int(v)) => Value::Float(*v as f32),
        (Value::Float(_), Cell::Float(v)) => Value::Float(*v as f32),
        (Value::String(_), Cell::Str(s)) => Value::String(Some(s.clone())),
        (old, new) => {
            let ty = match old {
                Value::Int(_) => "integer",
                Value::Float(_) => "decimal",
                Value::String(_) => "text",
            };
            return Err(format!("{new} does not fit a {ty} cell"));
        }
    })
}

fn col_index(info: &TableInfo, c: &evt_modfmt::Column) -> Result<usize, String> {
    match c {
        evt_modfmt::Column::Index(i) => Ok(*i as usize),
        evt_modfmt::Column::Name(n) => info.column(n).ok_or_else(|| format!("table `{}` has no column `{n}`", info.id)),
    }
}

fn set_cell(row: &mut Entry, col: usize, c: &Cell) -> Result<(), String> {
    let width = row.values.len();
    let slot = row.values.get_mut(col).ok_or_else(|| format!("column #{col} is past the end of the row ({width} values)"))?;
    *slot = convert(slot, c)?;
    Ok(())
}

/// Result of merging one file.
#[derive(Debug)]
pub struct Merged {
    pub bytes: Vec<u8>,
    pub notes: Notes,
    /// Ops applied (cells set + rows added).
    pub applied: usize,
}

/// Row source of a new row while a table's adds are processed.
#[derive(Clone, Copy)]
enum Slot {
    Base(usize),
    New(usize),
}

/// Apply `mods` (load order) to the T2B file `base`; tables are looked up in `index`. Ops that cannot apply are
/// skipped with a WARN; Err = the file itself is unusable.
pub fn merge_file(base: &[u8], index: &HashMap<String, TableInfo>, mods: &[ModOps]) -> Result<Merged, String> {
    let mut t = T2b::parse(base).map_err(|e| format!("not a T2B cfg.bin: {e}"))?;
    let sorted = t.sorted_lists();
    let mut notes: Notes = Vec::new();
    let mut applied = 0usize;
    fn skip(notes: &mut Notes, id: &str, what: String, e: String) {
        notes.push((Lvl::Warn, format!("mods: {id}: {what}: {e} (skipped)")));
    }

    // ---- phase 1: new rows, table by table (every mod, load order)
    let mut add_tables: Vec<&str> = mods.iter().flat_map(|m| m.adds.iter().map(|a| a.table.as_str())).collect();
    add_tables.sort();
    add_tables.dedup();
    let mut row_owner: BTreeMap<(String, KeyVal), Vec<String>> = BTreeMap::new();
    for table in add_tables {
        let Some(info) = index.get(table) else { continue };
        let Some(k) = info.key else {
            for m in mods {
                for a in m.adds.iter().filter(|a| a.table == table) {
                    skip(&mut notes, m.id, format!("add {table}[{}]", a.key), "the table has no key column".into());
                }
            }
            continue;
        };
        let loc = match locate(&t, info) {
            Ok(l) => l,
            Err(e) => {
                for m in mods {
                    for a in m.adds.iter().filter(|a| a.table == table) {
                        skip(&mut notes, m.id, format!("add {table}[{}]", a.key), e.clone());
                    }
                }
                continue;
            }
        };
        let mut keys: HashMap<KeyVal, Slot> = key_map(&t, &loc, k).into_iter().map(|(kv, i)| (kv, Slot::Base(i))).collect();
        let mut replaced: BTreeMap<usize, Vec<Entry>> = BTreeMap::new();
        let mut appended: Vec<Vec<Entry>> = Vec::new();
        for m in mods {
            for a in m.adds.iter().filter(|a| a.table == table) {
                let what = format!("add {table}[{}]", a.key);
                let block = |slot: Slot| -> Vec<Entry> {
                    match slot {
                        Slot::Base(i) => replaced.get(&i).cloned().unwrap_or_else(|| {
                            let (s, e) = loc.spans[i];
                            t.entries[s..=e].to_vec()
                        }),
                        Slot::New(j) => appended[j].clone(),
                    }
                };
                // the row: a clone of `from`, else the first row's shape with zero / empty values
                let mut blk = match &a.from {
                    Some(f) => match lookup(&keys, &RowKey::parse(f)) {
                        Some(slot) => block(slot),
                        None => {
                            skip(&mut notes, m.id, what, format!("`from` row {f} not found"));
                            continue;
                        }
                    },
                    None => {
                        let mut e = t.entries[loc.spans[0].0].clone();
                        for v in &mut e.values {
                            *v = match v {
                                Value::Int(_) => Value::Int(0),
                                Value::Float(_) => Value::Float(0.0),
                                Value::String(_) => Value::String(None),
                            };
                        }
                        vec![e]
                    }
                };
                let rk = RowKey::parse(&a.key);
                let kv = match blk[0].values.get(k) {
                    Some(Value::Int(_)) => KeyVal::I(rk.hash()),
                    Some(Value::String(_)) => KeyVal::S(a.key.clone()),
                    _ => {
                        skip(&mut notes, m.id, what, format!("key column #{k} missing or decimal"));
                        continue;
                    }
                };
                blk[0].values[k] = match &kv {
                    KeyVal::I(v) => Value::Int(*v as i32),
                    KeyVal::S(s) => Value::String(Some(s.clone())),
                };
                let mut bad = None;
                for (name, c) in &a.values {
                    if let Err(e) = info.column(name).ok_or_else(|| format!("table `{table}` has no column `{name}`")).and_then(|ci| set_cell(&mut blk[0], ci, c)) {
                        bad = Some(format!("{name}: {e}"));
                        break;
                    }
                }
                if let Some(e) = bad {
                    skip(&mut notes, m.id, what, e);
                    continue;
                }
                let owners = row_owner.entry((table.to_string(), kv.clone())).or_default();
                match keys.get(&kv).copied() {
                    Some(Slot::New(j)) => appended[j] = blk,
                    Some(Slot::Base(i)) => {
                        if owners.is_empty() {
                            notes.push((Lvl::Warn, format!("mods: {}: {what}: the row already exists in the game table: replaced", m.id)));
                        }
                        replaced.insert(i, blk);
                    }
                    None => {
                        appended.push(blk);
                        keys.insert(kv.clone(), Slot::New(appended.len() - 1));
                    }
                }
                owners.retain(|o| o.as_str() != m.id);
                owners.push(m.id.to_string());
                applied += 1;
            }
        }
        // write back: new rows after the last row (indices of the old rows unchanged), then the replaced rows from
        // the last one up
        let last = loc.spans.last().map(|s| s.1).unwrap_or(0);
        let n_new = appended.len();
        t.entries.splice(last + 1..last + 1, appended.into_iter().flatten());
        for (i, blk) in replaced.into_iter().rev() {
            let (s, e) = loc.spans[i];
            t.entries.splice(s..=e, blk);
        }
        if n_new > 0 {
            if let Some(b) = loc.begin {
                if let Some(Value::Int(n)) = t.entries[b].values.first_mut() {
                    if *n as usize == loc.spans.len() {
                        *n += n_new as i32;
                    }
                }
            }
        }
    }
    for ((table, kv), owners) in &row_owner {
        if owners.len() > 1 {
            let k = match kv {
                KeyVal::I(v) => format!("{}", *v as i32),
                KeyVal::S(s) => s.clone(),
            };
            notes.push((Lvl::Warn, format!("mods: data conflict: new row {table}[{k}]: {} (winner: {})", owners.join(", "), owners.last().unwrap())));
        }
    }

    // ---- phase 2: cells (every mod, load order)
    let mut maps: HashMap<&str, Option<(usize, ListLoc, HashMap<KeyVal, usize>)>> = HashMap::new();
    // (table, row, column) -> [(mod, key as written, column as written, value)]
    let mut cells: BTreeMap<(String, KeyVal, usize), Vec<(String, String, String, String)>> = BTreeMap::new();
    for m in mods {
        for s in &m.sets {
            let what = format!("set {}[{}].{}", s.table, s.key, s.column);
            let Some(info) = index.get(s.table.as_str()) else { continue };
            let entry = maps.entry(s.table.as_str()).or_insert_with(|| {
                let k = info.key?;
                let loc = locate(&t, info).ok()?;
                let km = key_map(&t, &loc, k);
                Some((k, loc, km))
            });
            let Some((_, loc, km)) = entry.as_ref() else {
                let why = if info.key.is_none() { "the table has no key column".to_string() } else { format!("no `{}` rows in the file", info.list) };
                skip(&mut notes, m.id, what, why);
                continue;
            };
            let rk = RowKey::parse(&s.key);
            let Some(row) = lookup(km, &rk) else {
                skip(&mut notes, m.id, what, "row not found".into());
                continue;
            };
            let col = match col_index(info, &s.column) {
                Ok(c) => c,
                Err(e) => {
                    skip(&mut notes, m.id, what, e);
                    continue;
                }
            };
            let ent = loc.spans[row].0;
            if let Err(e) = set_cell(&mut t.entries[ent], col, &s.value) {
                skip(&mut notes, m.id, what, e);
                continue;
            }
            applied += 1;
            let kv = key_candidates(&rk).into_iter().find(|c| km.get(c) == Some(&row)).unwrap_or(KeyVal::I(0));
            let v = cells.entry((s.table.clone(), kv, col)).or_default();
            v.retain(|(id, ..)| id.as_str() != m.id);
            v.push((m.id.to_string(), s.key.clone(), s.column.to_string(), s.value.to_string()));
        }
    }
    for ((table, _, _), v) in &cells {
        if v.len() > 1 {
            let (_, key, col, _) = v.last().unwrap();
            let who: Vec<String> = v.iter().map(|(id, _, _, val)| format!("{id} = {val}")).collect();
            notes.push((Lvl::Warn, format!("mods: data conflict: cell {table}[{key}].{col}: {} (winner: {})", who.join(", "), v.last().unwrap().0)));
        }
    }

    if !sorted.is_empty() {
        t.rebuild_sort_indexes_with(&sorted).map_err(|e| format!("sort index: {e}"))?;
    }
    let bytes = t.to_bytes().map_err(|e| format!("write: {e}"))?;
    T2b::parse(&bytes).map_err(|e| format!("merged file does not parse back: {e}"))?;
    Ok(Merged { bytes, notes, applied })
}

// ---------------------------------------------------------------- game files

/// Where the base tables come from.
pub trait Source {
    /// Every game file key (`data/...`, lowercase).
    fn keys(&mut self) -> Result<Vec<String>, String>;
    /// Bytes of `key` as the game reads it, plus the files the result depends on (for the cache check).
    fn read(&mut self, key: &str) -> Result<(Vec<u8>, Vec<PathBuf>), String>;
}

/// The installed game: `data/cpk_list.cfg.bin` decides loose file or CPK (like `CCriFileOperate::Open`).
pub struct GameSource {
    pub game_dir: PathBuf,
    list: Option<(vr_gamefiles::cpk_list::CpkList, HashMap<String, usize>)>,
}

impl GameSource {
    pub fn new(game_dir: &Path) -> GameSource {
        GameSource { game_dir: game_dir.to_path_buf(), list: None }
    }
    fn list_path(&self) -> PathBuf {
        self.game_dir.join("data").join("cpk_list.cfg.bin")
    }
    fn list(&mut self) -> Result<&(vr_gamefiles::cpk_list::CpkList, HashMap<String, usize>), String> {
        if self.list.is_none() {
            let l = vr_gamefiles::cpk::read_list(&self.list_path()).map_err(|e| format!("cpk_list: {e}"))?;
            let idx = l.path_index();
            self.list = Some((l, idx));
        }
        Ok(self.list.as_ref().unwrap())
    }
}

impl GameSource {
    /// Plugin API `game_file_path`: the loose file the game reads for `key` (cpk_list loose record, or a key the list
    /// does not have but the game folder does); None when the game reads it from a CPK.
    pub fn loose_path(&mut self, key: &str) -> Result<Option<PathBuf>, String> {
        let gd = self.game_dir.clone();
        let (l, idx) = self.list()?;
        match idx.get(&key.to_ascii_lowercase()) {
            Some(&i) if l.items[i].is_loose() => Ok(Some(gd.join(l.items[i].path()))),
            Some(_) => Ok(None),
            None => {
                let p = gd.join(key);
                if p.is_file() {
                    Ok(Some(p))
                } else {
                    Err(format!("{key}: not in cpk_list"))
                }
            }
        }
    }

    /// Size + mtime of the CPK that holds `key` (to re-extract a cached copy when the CPK changes).
    pub fn cpk_stamp(&mut self, key: &str) -> Result<String, String> {
        let gd = self.game_dir.clone();
        let (l, idx) = self.list()?;
        let i = *idx.get(&key.to_ascii_lowercase()).ok_or_else(|| format!("{key}: not in cpk_list"))?;
        let cpk = gd.join(l.items[i].cpk_path().unwrap_or_default());
        let d = stat(&cpk).ok_or_else(|| format!("{}: not found", cpk.display()))?;
        Ok(format!("{} {} {}", d.path, d.size, d.mtime))
    }
}

impl Source for GameSource {
    fn keys(&mut self) -> Result<Vec<String>, String> {
        Ok(self.list()?.1.keys().cloned().collect())
    }
    fn read(&mut self, key: &str) -> Result<(Vec<u8>, Vec<PathBuf>), String> {
        let lp = self.list_path();
        let gd = self.game_dir.clone();
        let (l, idx) = self.list()?;
        let loose = gd.join(key);
        let Some(&i) = idx.get(&key.to_ascii_lowercase()) else {
            return std::fs::read(&loose).map(|b| (b, vec![lp, loose.clone()])).map_err(|e| format!("{key}: not in cpk_list ({e})"));
        };
        let it = &l.items[i];
        if it.is_loose() {
            let p = gd.join(it.path());
            return std::fs::read(&p).map(|b| (b, vec![lp, p.clone()])).map_err(|e| format!("{}: {e}", p.display()));
        }
        let cpk = gd.join(it.cpk_path().unwrap_or_default());
        match vr_gamefiles::cpk::extract_from_cpk(&cpk, &it.dir, &it.name) {
            Ok(b) => Ok((b, vec![lp, cpk])),
            // an all-loose dump (no data/packs): the extracted file itself
            Err(e) => std::fs::read(&loose).map(|b| (b, vec![lp, loose.clone()])).map_err(|_| e.to_string()),
        }
    }
}

// ---------------------------------------------------------------- cache

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
struct Dep {
    path: String,
    size: u64,
    mtime: u128,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Output {
    /// Game key (`data/...`).
    pub key: String,
    /// File under the cache folder.
    pub rel: String,
    pub size: u64,
    /// Mods whose deltas went in, load order.
    pub mods: Vec<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct Manifest {
    inputs: String,
    deps: Vec<Dep>,
    outputs: Vec<Output>,
    /// Log lines of the build (lvl, text), repeated on a cache hit.
    notes: Vec<(String, String)>,
}

fn stat(p: &Path) -> Option<Dep> {
    let m = std::fs::metadata(p).ok()?;
    let mtime = m.modified().ok()?.duration_since(std::time::UNIX_EPOCH).ok()?.as_nanos();
    Some(Dep { path: p.to_string_lossy().into_owned(), size: m.len(), mtime })
}

fn lvl_str(l: Lvl) -> &'static str {
    match l {
        Lvl::Info => "info",
        Lvl::Warn => "warn",
        Lvl::Error => "error",
    }
}

fn lvl_parse(s: &str) -> Lvl {
    match s {
        "warn" => Lvl::Warn,
        "error" => Lvl::Error,
        _ => Lvl::Info,
    }
}

/// Hash of everything a merge result depends on besides the game files: merge version, table index, and every
/// active mod with deltas (id, load position, delta file names and texts), plus which mod serves which whole file.
pub fn inputs_hash(plan: &LoadPlan) -> String {
    let mut h = Sha1::new();
    h.update(format!("merge v{MERGE_VERSION}\n").as_bytes());
    h.update(tables::INDEX_TEXT.as_bytes());
    for m in plan.mods.iter().filter(|m| !m.deltas.is_empty()) {
        h.update(format!("\0mod {}\n", m.id()).as_bytes());
        for d in &m.deltas {
            h.update(format!("\0delta {}\n", d.rel).as_bytes());
            let raw = std::fs::read(m.dir.join(evt_modfmt::DATA_DIR).join(&d.rel)).unwrap_or_else(|e| e.to_string().into_bytes());
            h.update(&raw);
        }
    }
    for (k, (id, _)) in plan.file_overrides() {
        h.update(format!("\0file {k} {id}\n").as_bytes());
    }
    h.finalize().iter().map(|b| format!("{b:02x}")).collect()
}

/// What the overlay serves after the merge.
#[derive(Debug, Default)]
pub struct Outcome {
    /// (game key, merged file, label for the log).
    pub served: Vec<(String, PathBuf, String)>,
    pub notes: Notes,
}

fn read_manifest(p: &Path) -> Option<Manifest> {
    serde_json::from_slice(&std::fs::read(p).ok()?).ok()
}

fn write_atomic(p: &Path, bytes: &[u8]) -> Result<(), String> {
    if let Some(d) = p.parent() {
        std::fs::create_dir_all(d).map_err(|e| format!("{}: {e}", d.display()))?;
    }
    let tmp = p.with_extension("tmp");
    std::fs::write(&tmp, bytes).map_err(|e| format!("{}: {e}", tmp.display()))?;
    std::fs::rename(&tmp, p).map_err(|e| format!("{}: {e}", p.display()))
}

fn label(mods: &[String]) -> String {
    format!("delta merge [{}]", mods.join(", "))
}

/// Merge every active mod's deltas (see the module docs). `cache` = `<evt_loader>/cache/merged`.
pub fn run(plan: &LoadPlan, src: &mut dyn Source, cache: &Path) -> Outcome {
    run_with(plan, src, cache, tables::index())
}

/// [`run`] with an explicit table index (tests).
pub fn run_with(plan: &LoadPlan, src: &mut dyn Source, cache: &Path, index: &HashMap<String, TableInfo>) -> Outcome {
    let t0 = std::time::Instant::now();
    let mut out = Outcome::default();
    let delta_mods: Vec<_> = plan.mods.iter().filter(|m| !m.deltas.is_empty()).collect();
    if delta_mods.is_empty() {
        return out;
    }
    let inputs = inputs_hash(plan);
    let mpath = cache.join(MANIFEST);
    let old = read_manifest(&mpath);
    // ---- cache hit: same inputs, same game files, outputs intact
    if let Some(m) = &old {
        let deps_ok = m.deps.iter().all(|d| stat(Path::new(&d.path)).as_ref() == Some(d));
        let outs_ok = m.outputs.iter().all(|o| std::fs::metadata(cache.join(&o.rel)).ok().map(|x| x.len()) == Some(o.size));
        if m.inputs == inputs && deps_ok && outs_ok {
            for o in &m.outputs {
                out.served.push((o.key.clone(), cache.join(&o.rel), label(&o.mods)));
            }
            out.notes.extend(m.notes.iter().map(|(l, s)| (lvl_parse(l), s.clone())));
            out.notes.push((
                Lvl::Info,
                format!("mods: data deltas: {} merged table file(s) from the cache ({} ms)", m.outputs.len(), t0.elapsed().as_millis()),
            ));
            return out;
        }
    }
    // ---- rebuild: group every op by game file
    let mut notes: Notes = Vec::new();
    let mut resolved: HashMap<String, Option<String>> = HashMap::new();
    let mut keys: Option<Vec<String>> = None;
    // file key -> ops per mod (load order)
    let mut files: BTreeMap<String, Vec<ModOps>> = BTreeMap::new();
    for m in &delta_mods {
        for d in &m.deltas {
            for table in d.delta.tables() {
                let file = match resolved.get(table) {
                    Some(f) => f.clone(),
                    None => {
                        let r = match index.get(table) {
                            None => Err(format!("unknown table `{table}` (no T2B schema; RDBN tables are not supported by deltas yet)")),
                            Some(info) => {
                                if keys.is_none() {
                                    match src.keys() {
                                        Ok(k) => keys = Some(k),
                                        Err(e) => {
                                            notes.push((Lvl::Error, format!("mods: data deltas: {e}: no delta applied")));
                                            keys = Some(Vec::new());
                                        }
                                    }
                                }
                                tables::resolve_file(info, keys.as_ref().unwrap().iter().map(String::as_str))
                            }
                        };
                        let f = match r {
                            Ok(f) => Some(f),
                            Err(e) => {
                                notes.push((Lvl::Warn, format!("mods: data table {table}: {e}: its deltas are skipped")));
                                None
                            }
                        };
                        resolved.insert(table.to_string(), f.clone());
                        f
                    }
                };
                let Some(file) = file else { continue };
                let list = files.entry(file).or_default();
                if list.last().map(|o| o.id) != Some(m.id()) {
                    list.push(ModOps { id: m.id(), ..Default::default() });
                }
                let ops = list.last_mut().unwrap();
                ops.sets.extend(d.delta.set.iter().filter(|s| s.table == table));
                ops.adds.extend(d.delta.add.iter().filter(|a| a.table == table));
            }
        }
    }
    let overrides = plan.file_overrides();
    let mut deps: Vec<PathBuf> = Vec::new();
    let mut outputs: Vec<Output> = Vec::new();
    for (key, mods) in &files {
        let base = match overrides.get(key) {
            Some((id, p)) => std::fs::read(p).map(|b| (b, vec![p.clone()])).map_err(|e| format!("override of {id} {}: {e}", p.display())),
            None => src.read(key),
        };
        let (bytes, d) = match base {
            Ok(x) => x,
            Err(e) => {
                notes.push((Lvl::Error, format!("mods: data deltas: {key}: cannot read the base file ({e}): not merged")));
                continue;
            }
        };
        let ids: Vec<String> = mods.iter().map(|m| m.id.to_string()).collect();
        match merge_file(&bytes, index, mods) {
            Ok(mg) => {
                notes.extend(mg.notes);
                let rel = key.clone();
                if let Err(e) = write_atomic(&cache.join(&rel), &mg.bytes) {
                    notes.push((Lvl::Error, format!("mods: data deltas: {key}: cannot write the merged file ({e})")));
                    continue;
                }
                let base_from = overrides.get(key).map(|(id, _)| format!("override of {id}")).unwrap_or_else(|| "game file".into());
                notes.push((Lvl::Info, format!("mods: {key}: {} delta op(s) of {} merged over the {base_from}", mg.applied, ids.join(", "))));
                deps.extend(d);
                outputs.push(Output { key: key.clone(), rel, size: mg.bytes.len() as u64, mods: ids });
            }
            Err(e) => notes.push((Lvl::Error, format!("mods: data deltas: {key}: {e}: not merged"))),
        }
    }
    // stale outputs of the previous build
    if let Some(m) = &old {
        for o in m.outputs.iter().filter(|o| !outputs.iter().any(|n| n.rel == o.rel)) {
            let _ = std::fs::remove_file(cache.join(&o.rel));
        }
    }
    deps.sort();
    deps.dedup();
    let manifest = Manifest {
        inputs,
        deps: deps.iter().filter_map(|p| stat(p)).collect(),
        outputs: outputs.clone(),
        notes: notes.iter().map(|(l, s)| (lvl_str(*l).to_string(), s.clone())).collect(),
    };
    if let Err(e) = serde_json::to_vec_pretty(&manifest).map_err(|e| e.to_string()).and_then(|b| write_atomic(&mpath, &b)) {
        notes.push((Lvl::Warn, format!("mods: data deltas: cache manifest not written ({e}): rebuilt on every start")));
    }
    for o in &outputs {
        out.served.push((o.key.clone(), cache.join(&o.rel), label(&o.mods)));
    }
    out.notes = notes;
    out.notes.push((Lvl::Info, format!("mods: data deltas: {} table file(s) merged, cache rebuilt ({} ms)", outputs.len(), t0.elapsed().as_millis())));
    out
}

#[cfg(test)]
mod tests;
