//! **GAME INDEX** for VR-ModLoader Studio and the VR-Framework engines (docs/app/vr-index.md).
//!
//! Reads the player's **own** Inazuma Eleven Victory Road v7.1.2 install (the encrypted CPKs through
//! `data/cpk_list.cfg.bin`, in memory, retail content first) and writes a small local index: every character,
//! variant, hissatsu, keshin / armour / mix-max, passive, team, formation, item, emblem, kit, map / stadium, match,
//! BGM / SE cue, voice bank and menu with its internal id(s), its names in the 9 languages and key fields, the root
//! text tables (id -> string per language) and PNG thumbnails (faces, emblems, keshin / armour icons).
//! Nothing from Level-5 ships with the tools: the index is built on the player's PC.
//!
//! ```no_run
//! use vr_index::{Index, BuildOptions, Category, Query};
//! let idx = Index::open_or_build(r"D:\SteamLibrary\steamapps\common\INAZUMA ELEVEN Victory Road",
//!                                &vr_index::default_out_dir(), &BuildOptions::default(), &|_, _| {})?.0;
//! let mark = idx.get_in(Category::Character, "c01000010").unwrap();
//! assert_eq!(mark.name(vr_index::lang_index("en").unwrap()), Some("Mark Evans"));
//! let hits = idx.search(&Query::new("tornado fuego").category(Category::Technique).lang(2));
//! let who = idx.resolve("Mark Evans", &[Category::Character], Some(1)); // Unique / Ambiguous / NotFound
//! # Ok::<(), vr_index::Error>(())
//! ```

pub mod audio;
pub mod build;
pub mod error;
pub mod model;
pub mod peek;
pub mod search;
pub mod source;
pub mod text;
pub mod thumbs;

use std::collections::{BTreeMap, HashMap};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

pub use build::{default_out_dir, BuildOptions};
pub use error::{Error, Result};
pub use model::{lang_index, Category, Entity, Link, Meta, TextTable, Val, LANGS, SCHEMA_VERSION};
pub use search::{normalize, Candidate, Hit, Query, Resolution};
pub use source::{GameSource, SourceOptions};

/// File names inside the index folder.
pub const INDEX_FILE: &str = "index.vri";
pub const MANIFEST_FILE: &str = "manifest.json";

/// Text tables, a separate file loaded on first use (they are two thirds of the data).
pub const TEXTS_FILE: &str = "texts.vri";

/// On-disk payload (MessagePack, deflated).
#[derive(Serialize, Deserialize)]
struct Stored {
    meta: Meta,
    entities: Vec<Entity>,
}

fn write_packed<T: Serialize>(path: &Path, v: &T) -> Result<()> {
    let bytes = rmp_serde::to_vec(v).map_err(|e| Error::Index(e.to_string()))?;
    let mut enc = flate2::write::DeflateEncoder::new(Vec::new(), flate2::Compression::new(6));
    enc.write_all(&bytes).map_err(|e| Error::Index(e.to_string()))?;
    let z = enc.finish().map_err(|e| Error::Index(e.to_string()))?;
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, z).map_err(|e| Error::io(&tmp, e))?;
    std::fs::rename(&tmp, path).map_err(|e| Error::io(path, e))
}

fn read_packed<T: for<'de> Deserialize<'de>>(path: &Path) -> Result<T> {
    let raw = std::fs::read(path).map_err(|e| Error::io(path, e))?;
    let mut plain = Vec::new();
    flate2::read::DeflateDecoder::new(&raw[..]).read_to_end(&mut plain).map_err(|e| Error::Index(format!("{}: {e}", path.display())))?;
    rmp_serde::from_slice(&plain).map_err(|e| Error::Index(format!("{}: {e}", path.display())))
}

/// Is a saved index usable for this game?
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Freshness {
    Fresh,
    /// No index in the folder (or unreadable).
    Missing,
    /// Written by another index schema version.
    SchemaChanged,
    /// The game files changed (list, exe or CPK sizes / dates).
    GameChanged,
    /// Built from another folder or with other options (mods included or not).
    OtherSource,
}

/// The loaded index.
pub struct Index {
    meta: Meta,
    entities: Vec<Entity>,
    /// Loaded from [`TEXTS_FILE`] on first use.
    texts: std::sync::OnceLock<BTreeMap<String, TextTable>>,
    dir: Option<PathBuf>,
    by_id: HashMap<String, Vec<usize>>,
    by_hash: HashMap<u32, Vec<usize>>,
    by_cat: HashMap<(Category, String), usize>,
    /// Built on the first search.
    search: std::sync::OnceLock<search::SearchIndex>,
}

impl Index {
    pub(crate) fn from_parts(meta: Meta, entities: Vec<Entity>, texts: BTreeMap<String, TextTable>) -> Self {
        let mut texts = texts;
        for t in texts.values_mut() {
            t.rebuild();
        }
        let mut idx = Index {
            meta,
            entities,
            texts: std::sync::OnceLock::new(),
            dir: None,
            by_id: HashMap::new(),
            by_hash: HashMap::new(),
            by_cat: HashMap::new(),
            search: std::sync::OnceLock::new(),
        };
        idx.reindex();
        if !texts.is_empty() {
            let _ = idx.texts.set(texts);
        }
        idx
    }

    fn reindex(&mut self) {
        self.by_id.clear();
        self.by_hash.clear();
        self.by_cat.clear();
        for (i, e) in self.entities.iter().enumerate() {
            self.by_id.entry(e.id.to_lowercase()).or_default().push(i);
            self.by_hash.entry(e.hash).or_default().push(i);
            self.by_cat.entry((e.category, e.id.to_lowercase())).or_insert(i);
        }
        self.search = std::sync::OnceLock::new();
    }

    fn texts(&self) -> &BTreeMap<String, TextTable> {
        self.texts.get_or_init(|| {
            let Some(p) = self.dir.as_ref().map(|d| d.join(TEXTS_FILE)) else { return BTreeMap::new() };
            let mut t: BTreeMap<String, TextTable> = read_packed(&p).unwrap_or_default();
            for x in t.values_mut() {
                x.rebuild();
            }
            t
        })
    }

    fn keys(&self) -> &search::SearchIndex {
        self.search.get_or_init(|| search::SearchIndex::build(&self.entities))
    }

    pub(crate) fn set_meta(&mut self, m: Meta) {
        self.meta = m;
    }

    pub(crate) fn set_thumb(&mut self, i: usize, t: String) {
        if let Some(e) = self.entities.get_mut(i) {
            e.thumb = Some(t);
        }
    }

    // ---------------------------------------------------------------- build / load / save

    /// Build the index of the game at `game_dir` and save it in `out_dir` (thumbnails in `out_dir/thumbs`).
    pub fn build(game_dir: impl AsRef<Path>, out_dir: impl AsRef<Path>, opts: &BuildOptions, progress: build::Progress<'_>) -> Result<Index> {
        let out = out_dir.as_ref();
        std::fs::create_dir_all(out).map_err(|e| Error::io(out, e))?;
        // Thumbnails of another game state are stale: start over (same state: existing PNGs are reused).
        if let Ok(old) = Self::read_manifest(out) {
            let src = GameSource::open(game_dir.as_ref(), opts.source.clone())?;
            if old.fingerprint != src.fingerprint() || old.schema_version != SCHEMA_VERSION {
                let _ = std::fs::remove_dir_all(out.join("thumbs"));
            }
        }
        let mut idx = build::build_index(game_dir.as_ref(), Some(out), opts, progress)?;
        idx.save(out)?;
        idx.dir = Some(out.to_path_buf());
        Ok(idx)
    }

    /// Load a saved index (the text tables are read on first use).
    pub fn load(dir: impl AsRef<Path>) -> Result<Index> {
        let dir = dir.as_ref();
        let t0 = std::time::Instant::now();
        let s: Stored = read_packed(&dir.join(INDEX_FILE))?;
        if s.meta.schema_version != SCHEMA_VERSION {
            return Err(Error::Index(format!("versión de esquema {} (se espera {SCHEMA_VERSION}): reconstruir", s.meta.schema_version)));
        }
        let t1 = t0.elapsed();
        let mut idx = Index::from_parts(s.meta, s.entities, BTreeMap::new());
        idx.dir = Some(dir.to_path_buf());
        if std::env::var_os("VR_INDEX_TIMING").is_some() {
            eprintln!("load: decode {:?}, reindex {:?}", t1, t0.elapsed() - t1);
        }
        Ok(idx)
    }

    /// Save `index.vri` + `texts.vri` + `manifest.json` into `dir`.
    pub fn save(&self, dir: impl AsRef<Path>) -> Result<()> {
        let dir = dir.as_ref();
        std::fs::create_dir_all(dir).map_err(|e| Error::io(dir, e))?;
        write_packed(&dir.join(TEXTS_FILE), self.texts())?;
        write_packed(&dir.join(INDEX_FILE), &Stored { meta: self.meta.clone(), entities: self.entities.clone() })?;
        let m = dir.join(MANIFEST_FILE);
        std::fs::write(&m, serde_json::to_vec_pretty(&self.meta).unwrap_or_default()).map_err(|e| Error::io(&m, e))?;
        Ok(())
    }

    /// Build information of a saved index without loading it.
    pub fn read_manifest(dir: impl AsRef<Path>) -> Result<Meta> {
        let p = dir.as_ref().join(MANIFEST_FILE);
        let b = std::fs::read(&p).map_err(|e| Error::io(&p, e))?;
        serde_json::from_slice(&b).map_err(|e| Error::Index(format!("{}: {e}", p.display())))
    }

    /// Compare a saved index with the game as it is now (cheap: list / exe / CPK folder sizes and dates).
    pub fn freshness(game_dir: impl AsRef<Path>, dir: impl AsRef<Path>, opts: &SourceOptions) -> Freshness {
        let Ok(m) = Self::read_manifest(&dir) else { return Freshness::Missing };
        if !dir.as_ref().join(INDEX_FILE).is_file() {
            return Freshness::Missing;
        }
        if m.schema_version != SCHEMA_VERSION {
            return Freshness::SchemaChanged;
        }
        let Ok(src) = GameSource::open(game_dir.as_ref(), opts.clone()) else { return Freshness::GameChanged };
        if m.include_mods != opts.include_mods || m.cpk_list != src.list_path.display().to_string() {
            return Freshness::OtherSource;
        }
        if m.fingerprint != src.fingerprint() {
            return Freshness::GameChanged;
        }
        Freshness::Fresh
    }

    /// Load the saved index when it is fresh, else rebuild it. Returns `(index, rebuilt)`.
    pub fn open_or_build(game_dir: impl AsRef<Path>, dir: impl AsRef<Path>, opts: &BuildOptions, progress: build::Progress<'_>) -> Result<(Index, bool)> {
        if Self::freshness(&game_dir, &dir, &opts.source) == Freshness::Fresh {
            if let Ok(i) = Self::load(&dir) {
                return Ok((i, false));
            }
        }
        Ok((Self::build(game_dir, dir, opts, progress)?, true))
    }

    // ---------------------------------------------------------------- queries

    pub fn meta(&self) -> &Meta {
        &self.meta
    }

    /// Folder the index was loaded from / saved to.
    pub fn dir(&self) -> Option<&Path> {
        self.dir.as_deref()
    }

    pub fn entities(&self) -> &[Entity] {
        &self.entities
    }

    /// Every entity of a category, in game table order.
    pub fn list(&self, c: Category) -> impl Iterator<Item = &Entity> {
        self.entities.iter().filter(move |e| e.category == c)
    }

    /// Every entity with this id (case-insensitive), any category.
    pub fn get(&self, id: &str) -> Vec<&Entity> {
        self.by_id.get(&id.to_lowercase()).map(|v| v.iter().map(|&i| &self.entities[i]).collect()).unwrap_or_default()
    }

    /// The entity with this id in a category. Also accepts the key number (`"0xC19FE60C"`, `"-1046485492"`).
    pub fn get_in(&self, c: Category, id: &str) -> Option<&Entity> {
        if let Some(&i) = self.by_cat.get(&(c, id.to_lowercase())) {
            return Some(&self.entities[i]);
        }
        parse_key_number(id).and_then(|h| self.by_hash_in(c, h))
    }

    /// Entities whose table key is `hash`.
    pub fn by_hash(&self, hash: u32) -> Vec<&Entity> {
        self.by_hash.get(&hash).map(|v| v.iter().map(|&i| &self.entities[i]).collect()).unwrap_or_default()
    }

    pub fn by_hash_in(&self, c: Category, hash: u32) -> Option<&Entity> {
        self.by_hash.get(&hash)?.iter().map(|&i| &self.entities[i]).find(|e| e.category == c)
    }

    /// Target entities of an entity's links with relation `rel`.
    pub fn related(&self, e: &Entity, rel: &str) -> Vec<&Entity> {
        e.links(rel).filter_map(|l| self.get_in(l.category, &l.id)).collect()
    }

    /// Entities that link to `(category, id)`, with the relation name (e.g. the teams a variant plays in).
    pub fn backlinks(&self, c: Category, id: &str) -> Vec<(&Entity, &str)> {
        let mut out = Vec::new();
        for e in &self.entities {
            for l in &e.links {
                if l.category == c && l.id.eq_ignore_ascii_case(id) {
                    out.push((e, l.rel.as_str()));
                }
            }
        }
        out
    }

    /// Names in the 9 languages; entities without names of their own (variants, voice banks) use their character's.
    pub fn names<'a>(&'a self, e: &'a Entity) -> &'a [String] {
        if e.names.is_empty() {
            if let Some(c) = e.links("chara").next().and_then(|l| self.get_in(Category::Character, &l.id)) {
                return &c.names;
            }
        }
        &e.names
    }

    /// Display name in `lang` (index into [`LANGS`]; English, Japanese, then the id as fallbacks).
    pub fn display_name(&self, e: &Entity, lang: usize) -> String {
        let names = self.names(e);
        let get = |i: usize| names.get(i).map(String::as_str).filter(|s| !s.is_empty());
        let base = get(lang).or_else(|| get(1)).or_else(|| get(0)).map(str::to_string).unwrap_or_else(|| e.id.clone());
        match e.category {
            Category::Variant => {
                let pos = e.field("position").and_then(Val::as_str).unwrap_or("");
                let el = e.field("element").and_then(Val::as_str).unwrap_or("");
                format!("{base} ({pos} · {el})")
            }
            _ => base,
        }
    }

    /// Absolute path of an entity's thumbnail PNG, when there is one.
    pub fn thumb_path(&self, e: &Entity) -> Option<PathBuf> {
        Some(self.dir.as_ref()?.join(e.thumb.as_ref()?))
    }

    /// Text tables present (`chara_text`, `skill_text`, `menu_text`…).
    pub fn text_files(&self) -> impl Iterator<Item = &str> {
        self.texts().keys().map(String::as_str)
    }

    pub fn text_table(&self, file: &str) -> Option<&TextTable> {
        self.texts().get(file)
    }

    /// A text by table, id and language (e.g. a technique's description: `text("skill_text", desc_text, 2)`).
    pub fn text(&self, file: &str, id: u32, lang: usize) -> Option<&str> {
        self.texts().get(file)?.get(id, lang)
    }

    /// Full-text search inside one text table (substring, normalised), `(id, string)` pairs.
    pub fn search_text(&self, file: &str, query: &str, lang: usize, limit: usize) -> Vec<(u32, &str)> {
        let q = normalize(query);
        let Some(t) = self.texts().get(file) else { return Vec::new() };
        t.ids
            .iter()
            .enumerate()
            .map(|(r, i)| (*i, t.row(lang, r)))
            .filter(|(_, s)| !s.is_empty() && normalize(s).contains(&q))
            .take(if limit == 0 { 50 } else { limit })
            .collect()
    }

    /// Ranked fuzzy search by id or name in any language.
    pub fn search(&self, q: &Query) -> Vec<Hit> {
        let lang = q.lang.unwrap_or(1);
        let limit = if q.limit == 0 { 50 } else { q.limit };
        let nq = normalize(&q.text);
        if nq.is_empty() {
            return Vec::new();
        }
        let words: Vec<&str> = nq.split(' ').collect();
        let ok_ent = |i: usize| {
            let e = &self.entities[i];
            (q.categories.is_empty() || q.categories.contains(&e.category)) && q.fields.iter().all(|(k, v)| field_matches(e, k, v))
        };
        let mut best: HashMap<u32, (u32, usize)> = HashMap::new(); // ent -> (score, key index)
        // typo pre-filter: a key may miss at most 1 (short word) or 2 distinct letters of each query word
        let wmasks: Vec<(u64, u32)> = words
            .iter()
            .filter(|w| w.len() >= 4)
            .filter_map(|w| search::char_mask(w).map(|m| (m, if w.len() <= 6 { 1 } else { 2 })))
            .collect();
        let ascii_q = nq.is_ascii();
        let run = |typos: bool, best: &mut HashMap<u32, (u32, usize)>| {
            for (ki, k) in self.keys().keys.iter().enumerate() {
                if typos {
                    if !ascii_q || k.mask == 0 || best.contains_key(&k.ent) || wmasks.iter().any(|(m, max)| (m & !k.mask).count_ones() > *max) {
                        continue;
                    }
                }
                let Some(mut s) = search::score(&nq, &words, &k.text, typos) else { continue };
                if !ok_ent(k.ent as usize) {
                    continue;
                }
                if k.lang == search::ID_LANG {
                    s += 100;
                } else if k.lang as usize == lang {
                    s += 20;
                }
                let b = best.entry(k.ent).or_insert((0, ki));
                if s > b.0 {
                    *b = (s, ki);
                }
            }
        };
        run(false, &mut best);
        if best.values().filter(|b| b.0 >= 600).count() < limit && words.iter().any(|w| w.chars().count() >= 3) {
            run(true, &mut best);
        }
        let mut hits: Vec<Hit> = best
            .into_iter()
            .map(|(ent, (score, ki))| {
                let e = &self.entities[ent as usize];
                let k = &self.keys().keys[ki];
                Hit {
                    index: ent as usize,
                    id: e.id.clone(),
                    category: e.category,
                    name: self.display_name(e, lang),
                    matched: k.text.clone(),
                    matched_lang: search::lang_label(k.lang),
                    score,
                }
            })
            .collect();
        hits.sort_by(|a, b| b.score.cmp(&a.score).then(a.category.cmp(&b.category)).then(a.index.cmp(&b.index)));
        hits.truncate(limit);
        hits
    }

    /// Resolve a readable name (or an id) to exactly one entity, reporting ambiguity. For tools and validators:
    /// mod files store ids, names are only a help.
    pub fn resolve(&self, name: &str, categories: &[Category], lang: Option<usize>) -> Resolution {
        let in_cat = |e: &Entity| categories.is_empty() || categories.contains(&e.category);
        let dl = lang.unwrap_or(1);
        // 1. exact id / key number
        let mut by_id: Vec<usize> = self.by_id.get(&name.trim().to_lowercase()).cloned().unwrap_or_default();
        if by_id.is_empty() {
            if let Some(h) = parse_key_number(name.trim()) {
                by_id = self.by_hash.get(&h).cloned().unwrap_or_default();
            }
        }
        by_id.retain(|&i| in_cat(&self.entities[i]));
        if !by_id.is_empty() {
            let c: Vec<Candidate> = by_id.iter().map(|&i| self.candidate(i, "id".into(), dl)).collect();
            return if c.len() == 1 { Resolution::Unique { candidate: c.into_iter().next().unwrap() } } else { Resolution::Ambiguous { candidates: c } };
        }
        // 2. exact (normalised) name in any language
        let nq = normalize(name);
        let mut exact: Vec<(usize, u8)> = Vec::new();
        for k in &self.keys().keys {
            if k.lang != search::ID_LANG && k.text == nq && in_cat(&self.entities[k.ent as usize]) && !exact.iter().any(|(e, _)| *e == k.ent as usize) {
                exact.push((k.ent as usize, k.lang));
            }
        }
        // Entities without a name of their own (variants, voice banks) only count when asked for explicitly.
        if exact.iter().any(|(e, _)| !self.entities[*e].names.is_empty()) {
            let explicit = |c: Category| categories.contains(&c);
            exact.retain(|(e, _)| {
                let en = &self.entities[*e];
                !en.names.is_empty() || explicit(en.category)
            });
        }
        if exact.len() > 1 {
            if let Some(l) = lang {
                let same: Vec<(usize, u8)> = exact.iter().copied().filter(|(_, kl)| *kl as usize == l).collect();
                if same.len() == 1 {
                    exact = same;
                }
            }
        }
        match exact.len() {
            0 => {}
            // Without a requested language, show names in the language that matched.
            1 => {
                let (i, l) = exact[0];
                let dl = lang.unwrap_or(if (l as usize) < LANGS.len() { l as usize } else { dl });
                return Resolution::Unique { candidate: self.candidate(i, format!("name:{}", search::lang_label(l)), dl) };
            }
            _ => {
                return Resolution::Ambiguous {
                    candidates: exact
                        .into_iter()
                        .map(|(i, l)| {
                            let dl = lang.unwrap_or(if (l as usize) < LANGS.len() { l as usize } else { dl });
                            self.candidate(i, format!("name:{}", search::lang_label(l)), dl)
                        })
                        .collect(),
                };
            }
        }
        // 3. suggestions
        let mut q = Query::new(name).limit(5);
        q.categories = categories.to_vec();
        q.lang = lang;
        Resolution::NotFound { suggestions: self.search(&q).into_iter().map(|h| self.candidate(h.index, "fuzzy".into(), dl)).collect() }
    }

    fn candidate(&self, i: usize, matched: String, lang: usize) -> Candidate {
        let e = &self.entities[i];
        Candidate { index: i, id: e.id.clone(), category: e.category, name: self.display_name(e, lang), detail: self.detail(e), matched }
    }

    /// One-line summary of the fields that tell entities apart.
    pub fn detail(&self, e: &Entity) -> String {
        let f = |k: &str| e.field(k).map(|v| match v {
            Val::S(s) => s.clone(),
            Val::I(i) => i.to_string(),
            Val::F(x) => x.to_string(),
            _ => String::new(),
        });
        let parts: Vec<String> = match e.category {
            Category::Character => ["position", "element", "series", "kind"].iter().filter_map(|k| f(k)).collect(),
            Category::Variant => {
                let mut v: Vec<String> = ["position", "element"].iter().filter_map(|k| f(k)).collect();
                v.push(format!("rank {}", f("rank").unwrap_or_default()));
                if let Some(c) = e.links("chara").next() {
                    v.push(c.id.clone());
                }
                v
            }
            Category::Technique => ["type", "element", "power_max", "tp"].iter().filter_map(|k| f(k).map(|x| format!("{k} {x}"))).collect(),
            Category::Team => ["level", "formation"].iter().filter_map(|k| f(k).map(|x| format!("{k} {x}"))).collect(),
            Category::Item | Category::Emblem => ["item_category", "list", "rarity"].iter().filter_map(|k| f(k)).collect(),
            Category::Map => ["kind", "base_path"].iter().filter_map(|k| f(k)).collect(),
            Category::Match => ["game_type", "stadium"].iter().filter_map(|k| f(k)).collect(),
            Category::Bgm | Category::Se => ["bank", "cue"].iter().filter_map(|k| f(k)).collect(),
            _ => ["aura_type", "power_max"].iter().filter_map(|k| f(k)).collect(),
        };
        let mut s = e.category.as_str().to_string();
        for p in parts.into_iter().filter(|p| !p.is_empty()) {
            s.push_str(" · ");
            s.push_str(&p);
        }
        s
    }
}

fn field_matches(e: &Entity, k: &str, v: &str) -> bool {
    match e.field(k) {
        Some(Val::S(s)) => s.eq_ignore_ascii_case(v),
        Some(Val::I(i)) => v.parse::<i64>().is_ok_and(|x| x == *i),
        Some(Val::L(l)) => l.iter().any(|x| matches!(x, Val::S(s) if s.eq_ignore_ascii_case(v))),
        _ => false,
    }
}

/// `"0xC19FE60C"` / `"-1046485492"` / `"3248481804"` -> 32-bit key.
pub fn parse_key_number(s: &str) -> Option<u32> {
    if let Some(h) = s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
        return u32::from_str_radix(h, 16).ok();
    }
    if s.is_empty() || !s.trim_start_matches('-').chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    s.parse::<i64>().ok().filter(|v| *v >= i32::MIN as i64 && *v <= u32::MAX as i64).map(|v| v as u32)
}
