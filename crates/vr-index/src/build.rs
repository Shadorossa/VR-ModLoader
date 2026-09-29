//! Building the index from the game's own files (docs/app/vr-index.md §3).

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::Instant;

use l5_core::rdbn::{self, Rdbn};
use l5_core::t2b::{T2b, Value};
use l5_core::CfgBin;
use rayon::prelude::*;

use crate::error::Result;
use crate::model::*;
use crate::source::{self, GameSource, SourceOptions};
use crate::text::{self, ParsedText};
use crate::thumbs;

/// What to build.
#[derive(Debug, Clone)]
pub struct BuildOptions {
    pub source: SourceOptions,
    /// Extract thumbnails (face icons, emblems, keshin / armour icons) as PNG.
    pub thumbs: bool,
    /// Longest side of a thumbnail in pixels (the game icons are 256; a mip level is used when it fits).
    pub thumb_size: u32,
    /// Keep every root text table (id -> string per language) in the index, not only the names of entities.
    pub texts: bool,
    /// Index BGM / SE cues and voice banks (reads the language-independent ACB headers).
    pub audio: bool,
}

impl Default for BuildOptions {
    fn default() -> Self {
        BuildOptions { source: SourceOptions::default(), thumbs: true, thumb_size: 64, texts: true, audio: true }
    }
}

/// Progress callback: (step, detail).
pub type Progress<'a> = &'a (dyn Fn(&str, &str) + Sync);

// ------------------------------------------------------------------ value helpers

fn t_i(v: &[Value], i: usize) -> i64 {
    match v.get(i) {
        Some(Value::Int(n)) => *n as i64,
        Some(Value::Float(f)) => *f as i64,
        _ => 0,
    }
}
fn t_h(v: &[Value], i: usize) -> u32 {
    match v.get(i) {
        Some(Value::Int(n)) => *n as u32,
        _ => 0,
    }
}
fn t_s(v: &[Value], i: usize) -> Option<&str> {
    v.get(i).and_then(|x| x.as_str())
}

/// Hashes that mean "nothing": 0, -1, crc32("INVALID"), crc32("0").
fn is_null(h: u32) -> bool {
    h == 0 || h == u32::MAX || h == 0xC4DC_2B9A || h == crc("0")
}

pub(crate) fn crc(s: &str) -> u32 {
    crc32fast::hash(s.as_bytes())
}

fn hex(h: u32) -> String {
    format!("0x{h:08X}")
}

/// Rows of one T2B list.
fn t2b_rows<'a>(t: &'a T2b, list: &str) -> Vec<&'a [Value]> {
    t.entries.iter().filter(|e| e.name.as_deref() == Some(list)).map(|e| e.values.as_slice()).collect()
}

/// `[start, count]` of `ref_list` for every row of `parent` (T2B companion rows, schema `[[child]] by = "slice"`).
fn t2b_slices(t: &T2b, parent: &str, ref_list: &str) -> Vec<(usize, usize)> {
    let mut out: Vec<(usize, usize)> = Vec::new();
    let mut in_row = false;
    for e in &t.entries {
        let n = e.name.as_deref().unwrap_or("");
        if n == parent {
            out.push((0, 0));
            in_row = true;
        } else if in_row && n == ref_list {
            if let Some(last) = out.last_mut() {
                *last = (t_i(&e.values, 0).max(0) as usize, t_i(&e.values, 1).max(0) as usize);
            }
        }
    }
    out
}

/// Named access to one RDBN list.
struct RList<'a> {
    fields: HashMap<&'a str, usize>,
    rows: &'a [rdbn::Row],
}

fn rlist<'a>(r: &'a Rdbn, name: &str) -> Option<RList<'a>> {
    let l = r.lists.iter().find(|l| l.name == name)?;
    let ty = r.types.get(l.type_index)?;
    let fields = ty.fields.iter().enumerate().map(|(i, f)| (f.name.as_str(), i)).collect();
    Some(RList { fields, rows: &l.rows })
}

impl<'a> RList<'a> {
    fn v(&self, row: &'a rdbn::Row, f: &str) -> Option<&'a rdbn::Value> {
        row.get(*self.fields.get(f)?)?.first()
    }
    fn u(&self, row: &'a rdbn::Row, f: &str) -> u32 {
        self.v(row, f).map_or(0, r_u32)
    }
    fn i(&self, row: &'a rdbn::Row, f: &str) -> i64 {
        self.v(row, f).map_or(0, |v| r_u32(v) as i32 as i64)
    }
    fn s(&self, row: &'a rdbn::Row, f: &str) -> Option<&'a str> {
        match self.v(row, f)? {
            rdbn::Value::String(Some(s)) => Some(s),
            _ => None,
        }
    }
}

pub(crate) fn r_u32(v: &rdbn::Value) -> u32 {
    use rdbn::Value as V;
    match v {
        V::Bool(b) => *b as u32,
        V::Byte(n) => *n as u32,
        V::SByte(n) => *n as i32 as u32,
        V::Short(n) => *n as i32 as u32,
        V::Int(n) => *n as u32,
        V::Hash(h) => *h,
        V::Float(f) => *f as i32 as u32,
        _ => 0,
    }
}

// ------------------------------------------------------------------ enums (schemas/enums.toml)

fn element(v: i64) -> &'static str {
    match v {
        1 => "wind",
        2 => "forest",
        3 => "fire",
        4 => "mountain",
        5 => "void",
        _ => "none",
    }
}
fn position(v: i64) -> &'static str {
    match v {
        1 => "GK",
        2 => "FW",
        3 => "MF",
        4 => "DF",
        _ => "none",
    }
}
fn skill_type(v: i64) -> &'static str {
    match v {
        1 => "shoot",
        2 => "offense",
        3 => "defense",
        4 => "keeper",
        _ => "other",
    }
}
fn gender(v: i64) -> &'static str {
    match v {
        1 => "male",
        2 => "female",
        4 => "other",
        5 => "unknown",
        _ => "none",
    }
}
fn chara_kind(v: i64) -> &'static str {
    match v {
        10 => "player",
        20 => "creature",
        _ => "npc",
    }
}
fn item_category(v: i64) -> &'static str {
    match v {
        0 => "system",
        10 => "consumable",
        20 => "key_item",
        30 => "shoes",
        40 => "misanga",
        50 => "accessory",
        60 => "special_gear",
        70 => "battle_recovery",
        80 => "kit",
        90 => "emblem",
        100 => "formation",
        110 => "special_tactics",
        120 => "title",
        130 => "super_tactics",
        140 => "costume",
        150 => "craft_object",
        160 => "skill_manual",
        170 => "animal",
        180 => "kizuna_link",
        190 => "name_plate",
        200 => "performance",
        210 => "stat_beans",
        220 => "treasure",
        230 => "synergy_flag",
        240 => "user_title",
        _ => "other",
    }
}
fn map_kind(v: i64) -> &'static str {
    match v {
        1 => "field",
        2 => "dungeon",
        4 => "stadium",
        5 => "event",
        6 => "interior",
        8 => "battle",
        9 => "online_hub",
        _ => "other",
    }
}
fn game_type(v: i64) -> &'static str {
    match v {
        1 => "full",
        2 => "small",
        3 => "training_test",
        4 => "rpg_dribble_training",
        _ => "other",
    }
}
fn aura_category(key: &str) -> (Category, &'static str) {
    let p: String = key.chars().take_while(|c| c.is_ascii_alphabetic()).collect();
    match p.as_str() {
        "wks" => (Category::Keshin, "keshin"),
        "was" | "wao" | "wad" | "wak" => (Category::Armour, "armed"),
        "wmm" => (Category::Miximax, "miximax"),
        "wss" => (Category::Aura, "soul"),
        "wkt" => (Category::Aura, "keshin_tactic"),
        "wap" => (Category::Aura, "awakening"),
        _ => (Category::Aura, "other"),
    }
}

// ------------------------------------------------------------------ builder state

struct Texts {
    /// file stem -> per language parsed text
    files: BTreeMap<String, Vec<ParsedText>>,
}

impl Texts {
    fn names(&self, file: &str, id: u32) -> Vec<String> {
        if is_null(id) {
            return Vec::new();
        }
        let Some(langs) = self.files.get(file) else { return Vec::new() };
        let v: Vec<String> = langs.iter().map(|t| t.main.get(&id).map(|s| text::clean(s)).unwrap_or_default()).collect();
        if v.iter().all(String::is_empty) { Vec::new() } else { v }
    }
    fn raw(&self, file: &str, lang: usize, id: u32) -> Option<&str> {
        self.files.get(file)?.get(lang)?.main.get(&id).map(String::as_str)
    }
    fn forms(&self, file: &str, id: u32) -> Vec<String> {
        let mut out = Vec::new();
        if let Some(langs) = self.files.get(file) {
            for t in langs {
                if let Some(f) = t.forms.get(&id) {
                    out.extend(f.iter().map(|s| text::clean(s)));
                }
            }
        }
        out
    }
    fn to_tables(&self) -> BTreeMap<String, TextTable> {
        self.files
            .par_iter()
            .map(|(k, langs)| {
                let ids: BTreeSet<u32> = langs.iter().flat_map(|t| t.main.keys().copied()).collect();
                let ids: Vec<u32> = ids.into_iter().collect();
                let strings = langs.iter().map(|t| ids.iter().map(|i| t.main.get(i).cloned().unwrap_or_default()).collect()).collect();
                (k.clone(), TextTable::new(ids, strings))
            })
            .collect()
    }
}

struct Ctx {
    ents: Vec<Entity>,
    /// (category, hash) -> entity index
    by_hash: HashMap<(Category, u32), usize>,
    warnings: Vec<String>,
}

impl Ctx {
    fn push(&mut self, e: Entity) -> usize {
        let i = self.ents.len();
        self.by_hash.entry((e.category, e.hash)).or_insert(i);
        self.ents.push(e);
        i
    }
    fn id_of(&self, cat: Category, h: u32) -> Option<&str> {
        self.by_hash.get(&(cat, h)).map(|&i| self.ents[i].id.as_str())
    }
    /// First category among `cats` that has this hash.
    fn find(&self, cats: &[Category], h: u32) -> Option<(Category, &str)> {
        cats.iter().find_map(|&c| self.id_of(c, h).map(|id| (c, id)))
    }
}

fn add_alt(e: &mut Entity, s: &str) {
    let s = s.trim();
    if s.is_empty() || e.names.iter().any(|n| n == s) || e.alt.iter().any(|n| n == s) {
        return;
    }
    e.alt.push(s.to_string());
}

// ------------------------------------------------------------------ build

/// Build the index of the game at `game_dir` into memory (see [`crate::Index::build`] to also save it).
pub fn build_index(game_dir: &Path, out_dir: Option<&Path>, opts: &BuildOptions, progress: Progress<'_>) -> Result<crate::Index> {
    let t0 = Instant::now();
    progress("cpk_list", "leyendo y descifrando la lista de archivos");
    let src = GameSource::open(game_dir, opts.source.clone())?;
    let mut warnings = Vec::new();

    // --- which tables
    let gd = "data/common/gamedata/";
    let want: &[(&str, &str, &str)] = &[
        ("chara_base", "character/", "chara_base"),
        ("chara_param", "character/", "chara_param"),
        ("belong_team", "character/", "belong_team_config"),
        ("uniform", "character/", "uniform_config"),
        ("skill", "skill/", "skill_config"),
        ("aura", "skill/", "aura_skill_config"),
        ("passive", "skill/", "passive_skill_config"),
        ("team", "team/", "team_config"),
        ("formation", "formation/", "formation_config"),
        ("item", "item/", "item_config"),
        ("soccer_game", "soccer/", "soccer_game_config"),
        ("menu", "menu/", "menu_create_setting"),
    ];
    let mut paths: BTreeMap<&str, String> = BTreeMap::new();
    for (k, dir, stem) in want {
        match src.latest(&format!("{gd}{dir}"), stem) {
            Some(p) => {
                paths.insert(k, p);
            }
            None => warnings.push(format!("tabla no encontrada: {gd}{dir}{stem}_*")),
        }
    }
    for (k, dir, stem) in [
        ("series", "data/common/gamedata/character/", "chara_series_config"),
        ("map", "data/common/map/", "map_data"),
        ("bgm", "data/common/sound/", "bgm_config"),
        ("match_bgm", "data/common/sound/", "soccer_game_bgm_config"),
        ("map_sound", "data/common/sound/", "map_sound"),
    ] {
        match src.latest(dir, stem) {
            Some(p) => {
                paths.insert(k, p);
            }
            None => warnings.push(format!("tabla no encontrada: {dir}{stem}")),
        }
    }

    // --- text files: every root file of every language
    let mut text_paths: Vec<(String, usize, String)> = Vec::new(); // (stem, lang, path)
    for (li, lang) in LANGS.iter().enumerate() {
        let prefix = format!("data/common/text/{lang}/");
        for it in src.list_retail(&prefix) {
            let rest = &it.path[prefix.len()..];
            if rest.contains('/') || !rest.ends_with(".cfg.bin") {
                continue;
            }
            text_paths.push((rest.trim_end_matches(".cfg.bin").to_string(), li, it.path.clone()));
        }
    }

    // --- read everything in one batch (grouped by CPK)
    progress("lectura", &format!("{} tablas + {} textos desde los CPK", paths.len(), text_paths.len()));
    let mut all: Vec<String> = paths.values().cloned().collect();
    all.extend(text_paths.iter().map(|t| t.2.clone()));
    let mut files = src.read_many(&all);
    let mut take = |p: &str| -> Option<Vec<u8>> {
        match files.remove(p) {
            Some(Ok(b)) => Some(b),
            Some(Err(e)) => {
                warnings.push(format!("{p}: {e}"));
                None
            }
            None => None,
        }
    };
    let mut raw_tables: BTreeMap<&str, Vec<u8>> = BTreeMap::new();
    for (k, p) in &paths {
        if let Some(b) = take(p) {
            raw_tables.insert(k, b);
        }
    }
    let raw_texts: Vec<(String, usize, Vec<u8>)> = text_paths.iter().filter_map(|(s, l, p)| take(p).map(|b| (s.clone(), *l, b))).collect();

    // --- parse
    progress("parseo", "tablas y textos");
    let parsed: BTreeMap<&str, CfgBin> = raw_tables
        .par_iter()
        .filter_map(|(k, b)| CfgBin::parse(b).ok().map(|d| (*k, d)))
        .collect();
    for k in raw_tables.keys() {
        if !parsed.contains_key(k) {
            warnings.push(format!("no se pudo leer la tabla {k}"));
        }
    }
    drop(raw_tables);
    let texts_parsed: Vec<(String, usize, ParsedText)> = raw_texts.into_par_iter().map(|(s, l, b)| (s, l, text::parse_text(&b))).collect();
    let mut tfiles: BTreeMap<String, Vec<ParsedText>> = BTreeMap::new();
    for (s, l, t) in texts_parsed {
        tfiles.entry(s).or_insert_with(|| vec![ParsedText::default(); LANGS.len()])[l] = t;
    }
    let texts = Texts { files: tfiles };

    let t2b = |k: &str| parsed.get(k).and_then(CfgBin::as_t2b);
    let rdb = |k: &str| parsed.get(k).and_then(CfgBin::as_rdbn);
    let mut cx = Ctx { ents: Vec::new(), by_hash: HashMap::new(), warnings: Vec::new() };

    // --- audio bank names (from the list, nothing read yet)
    let mut voice_banks: BTreeMap<String, Vec<&str>> = BTreeMap::new();
    let mut root_banks: Vec<String> = Vec::new();
    for it in src.list_retail("data/common/sound_asset/") {
        let rest = &it.path["data/common/sound_asset/".len()..];
        let Some(stem) = rest.strip_suffix(".acb") else { continue };
        match stem.split_once('/') {
            None => root_banks.push(stem.to_string()),
            Some((lang @ ("ja" | "en"), b)) if !b.contains('/') => voice_banks.entry(b.to_string()).or_default().push(if lang == "ja" { "ja" } else { "en" }),
            _ => {}
        }
    }
    let voice_by_crc: HashMap<u32, &str> = voice_banks.keys().map(|b| (crc(b), b.as_str())).collect();

    // ================================================================ characters
    progress("entidades", "personajes");
    // series names
    let mut series: HashMap<u32, String> = HashMap::new();
    if let Some(l) = rdb("series").and_then(|r| rlist(r, "m_charaSeriesInfoList")) {
        for row in l.rows {
            let id = l.u(row, "charaSeriesId");
            let n = texts.names("chara_add_info_text", l.u(row, "charaSeriesNameTextId"));
            if let Some(n) = n.get(1).or(n.first()).filter(|s| !s.is_empty()) {
                series.insert(id, n.clone());
            }
        }
    }
    // belong teams (school) names
    let mut belong: HashMap<u32, String> = HashMap::new();
    if let Some(l) = rdb("belong_team").and_then(|r| rlist(r, "m_belongTeamInfoList")) {
        for row in l.rows {
            let n = texts.names("team_text", l.u(row, "teamNameTextId"));
            if let Some(n) = n.get(1).filter(|s| !s.is_empty()).or(n.first()) {
                belong.insert(l.u(row, "belongTeamId"), n.clone());
            }
        }
    }
    let param_rows: Vec<&[Value]> = t2b("chara_param").map(|t| t2b_rows(t, "CHARA_PARAM_INFO")).unwrap_or_default();
    let param_by_hash: HashMap<u32, &[Value]> = param_rows.iter().map(|r| (t_h(r, 0), *r)).collect();
    if let Some(t) = t2b("chara_base") {
        for r in t2b_rows(t, "CHARA_BASE_INFO") {
            let h = t_h(r, 0);
            let id = t_s(r, 1).map(str::to_string).unwrap_or_else(|| hex(h));
            let mut e = Entity::new(Category::Character, id.clone(), h);
            e.names = texts.names("chara_text", t_h(r, 3));
            for s in texts.forms("chara_text", t_h(r, 3)) {
                add_alt(&mut e, &s);
            }
            for s in texts.names("chara_text", t_h(r, 4)) {
                add_alt(&mut e, &s);
            }
            if let Some(ja) = texts.raw("chara_text", 0, t_h(r, 3)).and_then(text::reading) {
                add_alt(&mut e, &ja);
            }
            e.set("kind", chara_kind(t_i(r, 10)));
            e.set("gender", gender(t_i(r, 11)));
            if let Some(s) = series.get(&t_h(r, 15)) {
                e.set("series", s.as_str());
            }
            let schools: Vec<Val> = (16..=18).filter_map(|c| belong.get(&t_h(r, c))).map(|s| Val::S(s.clone())).collect();
            if !schools.is_empty() {
                e.set("schools", Val::L(schools));
            }
            let dt = t_h(r, 19);
            if !is_null(dt) {
                e.set("desc_text", dt as i64);
            }
            // voice bank: 0 = own bank named like the character, else crc32 of another bank
            let vref = t_h(r, 9);
            let bank = if vref == 0 { voice_banks.contains_key(&id).then_some(id.as_str()) } else { voice_by_crc.get(&vref).copied() };
            if let Some(b) = bank {
                e.set("voice_bank", b);
            }
            // element / position of the base variant
            if let Some(p) = param_by_hash.get(&crc(&format!("pc_para_{id}"))) {
                e.set("element", element(t_i(p, 2)));
                e.set("position", position(t_i(p, 3)));
            }
            cx.push(e);
        }
    }

    // ================================================================ techniques / auras / passives
    progress("entidades", "supertécnicas, keshin, armaduras, pasivas");
    if let Some(l) = rdb("skill").and_then(|r| rlist(r, "m_skillInfoList")) {
        for row in l.rows {
            let h = l.u(row, "skillID");
            let id = l.s(row, "skillIDStr").map(str::to_string).unwrap_or_else(|| hex(h));
            let mut e = Entity::new(Category::Technique, id, h);
            e.names = texts.names("skill_text", l.u(row, "skillNameId"));
            e.set("type", skill_type(l.i(row, "category")));
            e.set("element", element(l.i(row, "element")));
            e.set("power_min", l.i(row, "power_min"));
            e.set("power_max", l.i(row, "power_max"));
            e.set("tp", l.i(row, "consumeTp"));
            let players = match l.i(row, "partnerType") {
                0 => 1,
                n => n,
            };
            e.set("players", players);
            if let Some(ev) = l.s(row, "eventIDName") {
                e.set("event", ev);
            }
            let d = l.u(row, "skillDescId");
            if !is_null(d) {
                e.set("desc_text", d as i64);
            }
            for f in ["partner1", "partner2", "partner3"] {
                let p = l.u(row, f);
                if let Some(pid) = cx.id_of(Category::Character, p).map(str::to_string) {
                    e.link("partner", Category::Character, pid);
                }
            }
            cx.push(e);
        }
    } else {
        warnings.push("skill_config: m_skillInfoList no encontrada".into());
    }
    if let Some(t) = t2b("aura") {
        let owners_all: Vec<u32> = t2b_rows(t, "AURA_CMD_CHARA").iter().map(|r| t_h(r, 0)).collect();
        let slices = t2b_slices(t, "AURA_CMD_INFO", "AURA_CMD_INFO_REF_CHARA");
        for (i, r) in t2b_rows(t, "AURA_CMD_INFO").into_iter().enumerate() {
            let h = t_h(r, 0);
            let key = t_s(r, 1).map(str::to_string).unwrap_or_else(|| hex(h));
            let (cat, kind) = aura_category(&key);
            let mut e = Entity::new(cat, key, h);
            e.names = texts.names("skill_text", t_h(r, 2));
            e.set("aura_type", kind);
            e.set("aura_type_raw", t_i(r, 10));
            e.set("power_min", t_i(r, 4));
            e.set("power_max", t_i(r, 5));
            let d = t_h(r, 3);
            if !is_null(d) {
                e.set("desc_text", d as i64);
            }
            if let Some(s) = cx.id_of(Category::Technique, t_h(r, 6)).map(str::to_string) {
                e.link("linked_skill", Category::Technique, s);
            }
            if let Some(c) = cx.id_of(Category::Character, t_h(r, 7)).map(str::to_string) {
                e.link("aura_chara", Category::Character, c);
            }
            let (s, n) = slices.get(i).copied().unwrap_or((0, 0));
            for o in owners_all.iter().skip(s).take(n) {
                if let Some(c) = cx.id_of(Category::Character, *o).map(str::to_string) {
                    e.link("owner", Category::Character, c);
                }
            }
            e.set("chara_param_raw", t_h(r, 13) as i64); // resolved to a variant link below
            cx.push(e);
        }
    }
    if let Some(t) = t2b("passive") {
        for r in t2b_rows(t, "PASSIVE_SKILL_INFO") {
            let h = t_h(r, 0);
            let id = t_s(r, 6).map(str::to_string).unwrap_or_else(|| hex(h));
            let mut e = Entity::new(Category::Passive, id, h);
            e.names = texts.names("skill_text", t_h(r, 1));
            cx.push(e);
        }
    }

    // ================================================================ items, emblems, formations, kits
    progress("entidades", "objetos, emblemas, formaciones, equipaciones");
    let mut formation_key: HashMap<u32, String> = HashMap::new();
    let mut kit_item: HashMap<u32, (String, Vec<String>)> = HashMap::new();
    if let Some(t) = t2b("item") {
        let mut lists: Vec<String> = t.entries.iter().filter_map(|e| e.name.clone()).filter(|n| n.starts_with("ITEM_") && n.ends_with("_INFO")).collect();
        lists.sort();
        lists.dedup();
        for list in lists {
            if list == "ITEM_SPECIAL_SKILL_INFO" {
                continue; // same ids as the hissatsu / auras / passives (manuals)
            }
            let sub = list.trim_start_matches("ITEM_").trim_end_matches("_INFO").to_lowercase();
            for r in t2b_rows(t, &list) {
                let h = t_h(r, 0);
                let key = t_s(r, 11).map(str::to_string).unwrap_or_else(|| hex(h));
                let cat = if list == "ITEM_EMBLEM_INFO" { Category::Emblem } else { Category::Item };
                let mut e = Entity::new(cat, key.clone(), h);
                e.names = texts.names("item_text", t_h(r, 2));
                e.set("list", sub.as_str());
                e.set("item_category", item_category(t_i(r, 5)));
                e.set("rarity", t_i(r, 12));
                e.set("icon_index", t_i(r, 15));
                let d = t_h(r, 3);
                if !is_null(d) {
                    e.set("desc_text", d as i64);
                }
                let linked = t_h(r, 16);
                if list == "ITEM_FORMATION_INFO" && !is_null(linked) {
                    formation_key.entry(linked).or_insert_with(|| key.clone());
                }
                if r.len() > 18 && matches!(sub.as_str(), "fashion" | "costume") {
                    let kit = t_h(r, 18);
                    if !is_null(kit) {
                        kit_item.entry(kit).or_insert_with(|| (key.clone(), e.names.clone()));
                    }
                }
                if !is_null(linked) {
                    e.set("linked_raw", linked as i64);
                }
                cx.push(e);
            }
        }
    }
    if let Some(l) = rdb("formation").and_then(|r| rlist(r, "m_SoccerFormationInfoList")) {
        for row in l.rows {
            let h = l.u(row, "formId");
            let id = formation_key.get(&h).cloned().unwrap_or_else(|| hex(h));
            let mut e = Entity::new(Category::Formation, id, h);
            e.names = texts.names("item_text", l.u(row, "nounId"));
            e.set("power_offense", l.i(row, "powerOffense"));
            e.set("power_defense", l.i(row, "powerDefense"));
            let d = l.u(row, "descId");
            if !is_null(d) {
                e.set("desc_text", d as i64);
            }
            cx.push(e);
        }
    }
    if let Some(l) = rdb("uniform").and_then(|r| rlist(r, "m_UniformInfoList")) {
        for row in l.rows {
            let h = l.u(row, "nameId");
            let (id, names) = match kit_item.get(&h) {
                Some((key, names)) => {
                    let bare = key.strip_prefix("uni_").unwrap_or(key);
                    let id = if crc(bare) == h { bare.to_string() } else if crc(key) == h { key.clone() } else { hex(h) };
                    (id, names.clone())
                }
                None => (hex(h), Vec::new()),
            };
            let mut e = Entity::new(Category::Kit, id, h);
            e.names = names;
            if let Some((key, _)) = kit_item.get(&h) {
                e.link("item", Category::Item, key.clone());
            }
            cx.push(e);
        }
    }
    // item -> linked object (formation / skill / aura / passive)
    for i in 0..cx.ents.len() {
        let Some(raw) = cx.ents[i].fields.get("linked_raw").and_then(Val::as_i64) else { continue };
        let h = raw as u32;
        let hit = cx
            .find(&[Category::Formation, Category::Technique, Category::Keshin, Category::Armour, Category::Miximax, Category::Aura, Category::Passive], h)
            .map(|(c, id)| (c, id.to_string()));
        let e = &mut cx.ents[i];
        e.fields.remove("linked_raw");
        if let Some((c, id)) = hit {
            e.link("linked", c, id);
        }
    }

    // ================================================================ variants
    progress("entidades", "variantes (chara_param)");
    let skill_cats = [Category::Technique, Category::Keshin, Category::Armour, Category::Miximax, Category::Aura, Category::Passive, Category::Item];
    let mut chara_variants: HashMap<usize, Vec<String>> = HashMap::new();
    for r in &param_rows {
        let h = t_h(r, 0);
        let ch = t_h(r, 1);
        let chara = cx.by_hash.get(&(Category::Character, ch)).copied();
        let id = match chara.map(|c| cx.ents[c].id.clone()) {
            Some(cid) if crc(&format!("pc_para_{cid}")) == h => format!("pc_para_{cid}"),
            _ => hex(h),
        };
        let mut e = Entity::new(Category::Variant, id.clone(), h);
        if let Some(c) = chara {
            e.link("chara", Category::Character, cx.ents[c].id.clone());
            chara_variants.entry(c).or_default().push(id.clone());
        }
        e.set("element", element(t_i(r, 2)));
        e.set("position", position(t_i(r, 3)));
        e.set("sub_position", position(t_i(r, 4)));
        e.set("build_type", t_i(r, 5));
        e.set("growth_pattern", t_i(r, 7));
        e.set("play_style", t_i(r, 8));
        e.set("rank", t_i(r, 9));
        e.set("special_rarity", t_i(r, 41));
        let mut skills = Vec::new();
        for (sc, lc) in [(11, 12), (13, 14), (15, 16), (17, 18), (19, 20), (21, 22)] {
            let sh = t_h(r, sc);
            if is_null(sh) {
                continue;
            }
            let sid = cx.find(&skill_cats, sh).map(|(c, s)| (c, s.to_string()));
            let mut m = BTreeMap::new();
            m.insert("skill".to_string(), Val::S(sid.as_ref().map_or_else(|| hex(sh), |x| x.1.clone())));
            m.insert("level".to_string(), Val::I(t_i(r, lc)));
            skills.push(Val::M(m));
            if let Some((c, s)) = sid {
                e.link("skill", c, s);
            }
        }
        if !skills.is_empty() {
            e.set("skills", Val::L(skills));
        }
        cx.push(e);
    }
    for (c, vs) in chara_variants {
        for v in vs {
            cx.ents[c].link("variant", Category::Variant, v);
        }
    }
    // aura -> owning variant
    for i in 0..cx.ents.len() {
        let Some(raw) = cx.ents[i].fields.get("chara_param_raw").and_then(Val::as_i64) else { continue };
        let v = cx.id_of(Category::Variant, raw as u32).map(str::to_string);
        let e = &mut cx.ents[i];
        e.fields.remove("chara_param_raw");
        if let Some(v) = v {
            e.link("variant", Category::Variant, v);
        }
    }

    // ================================================================ teams
    progress("entidades", "equipos");
    if let Some(t) = t2b("team") {
        let members = t2b_rows(t, "SOCCER_TEAM_MEMBER");
        let slices = t2b_slices(t, "SOCCER_TEAM_INFO", "SOCCER_TEAM_INFO_REF_MEMBER");
        for (i, r) in t2b_rows(t, "SOCCER_TEAM_INFO").into_iter().enumerate() {
            let h = t_h(r, 0);
            let id = t_s(r, 1).map(str::to_string).unwrap_or_else(|| hex(h));
            let mut e = Entity::new(Category::Team, id, h);
            e.names = texts.names("team_text", t_h(r, 3));
            e.set("level", t_i(r, 8));
            let d = t_h(r, 2);
            if !is_null(d) {
                e.set("desc_text", d as i64);
            }
            for (col, rel, cat) in [(4, "formation", Category::Formation), (5, "kit", Category::Kit), (6, "emblem", Category::Emblem)] {
                let x = t_h(r, col);
                if let Some(id) = cx.id_of(cat, x).map(str::to_string) {
                    e.set(rel, id.as_str());
                    e.link(rel, cat, id);
                } else if !is_null(x) {
                    e.set(rel, hex(x));
                }
            }
            let (s, n) = slices.get(i).copied().unwrap_or((0, 0));
            let mut list = Vec::new();
            for m in members.iter().skip(s).take(n) {
                let vh = t_h(m, 0);
                let vi = cx.by_hash.get(&(Category::Variant, vh)).copied();
                let mut mm = BTreeMap::new();
                let vid = vi.map_or_else(|| hex(vh), |x| cx.ents[x].id.clone());
                let chara = vi.and_then(|x| cx.ents[x].links("chara").next().map(|l| l.id.clone()));
                mm.insert("variant".to_string(), Val::S(vid.clone()));
                if let Some(c) = &chara {
                    mm.insert("chara".to_string(), Val::S(c.clone()));
                }
                mm.insert("slot".to_string(), Val::I(t_i(m, 8)));
                mm.insert("number".to_string(), Val::I(t_i(m, 10)));
                if t_i(m, 11) != 0 {
                    mm.insert("captain".to_string(), Val::I(1));
                }
                list.push(Val::M(mm));
                if vi.is_some() {
                    e.link("member", Category::Variant, vid);
                }
                if let Some(c) = chara {
                    e.link("member_chara", Category::Character, c);
                }
            }
            e.set("members", Val::L(list));
            cx.push(e);
        }
    }

    // ================================================================ maps / matches
    progress("entidades", "mapas, estadios, partidos");
    if let Some(l) = rdb("map").and_then(|r| rlist(r, "m_MapInfoList")) {
        for row in l.rows {
            let h = l.u(row, "map_id");
            let name = l.s(row, "map_name").unwrap_or("");
            let id = if name.is_empty() { hex(h) } else { name.to_string() };
            let mut e = Entity::new(Category::Map, id, h);
            e.names = texts.names("map_text", crc(&format!("map_name_{name}")));
            e.set("kind", map_kind(l.i(row, "mapType")));
            if let Some(b) = l.s(row, "base_path") {
                e.set("base_path", b);
            }
            cx.push(e);
        }
    }
    // match BGM sets (for the match entities)
    let mut bgm_sets: HashMap<u32, Vec<u32>> = HashMap::new();
    if let Some(l) = rdb("match_bgm").and_then(|r| rlist(r, "m_SoccerGameBgmSetInfoList")) {
        for row in l.rows {
            let id = row.first().and_then(|f| f.first()).map_or(0, r_u32);
            let ids: Vec<u32> = row.get(2).map(|v| v.iter().map(r_u32).filter(|x| !is_null(*x)).collect()).unwrap_or_default();
            bgm_sets.insert(id, ids);
        }
    }
    let mut match_bgm: Vec<(usize, Vec<u32>)> = Vec::new();
    if let Some(t) = t2b("soccer_game") {
        let diffs = t2b_rows(t, "SOCCER_GAME_DIFFICULTY");
        let slices = t2b_slices(t, "SOCCER_GAME_INFO", "SOCCER_GAME_INFO_REF_DIFFICULTY");
        for (i, r) in t2b_rows(t, "SOCCER_GAME_INFO").into_iter().enumerate() {
            let h = t_h(r, 0);
            let id = t_s(r, 1).map(str::to_string).unwrap_or_else(|| hex(h));
            let mut e = Entity::new(Category::Match, id, h);
            let title = t_h(r, 4);
            e.names = texts.names("soccer_game_title", title);
            if e.names.is_empty() {
                e.names = texts.names("soccer_common_text", title);
            }
            e.set("game_type", game_type(t_i(r, 2)));
            if let Some(m) = cx.id_of(Category::Map, t_h(r, 3)).map(str::to_string) {
                e.set("stadium", m.as_str());
                e.link("stadium", Category::Map, m);
            }
            let (s, n) = slices.get(i).copied().unwrap_or((0, 0));
            let mut list = Vec::new();
            let mut bgm_ids: Vec<u32> = Vec::new();
            for d in diffs.iter().skip(s).take(n) {
                let mut m = BTreeMap::new();
                m.insert("difficulty".to_string(), Val::I(t_i(d, 0)));
                for (col, lcol, key) in [(1, 2, "opponent"), (5, 6, "own_team")] {
                    let th = t_h(d, col);
                    if is_null(th) {
                        continue;
                    }
                    let tid = cx.id_of(Category::Team, th).map(str::to_string);
                    m.insert(key.to_string(), Val::S(tid.clone().unwrap_or_else(|| hex(th))));
                    m.insert(format!("{key}_level"), Val::I(t_i(d, lcol)));
                    if let Some(tid) = tid {
                        if !e.links.iter().any(|l| l.rel == key && l.id == tid) {
                            e.link(key, Category::Team, tid);
                        }
                    }
                }
                m.insert("minutes".to_string(), Val::I(t_i(d, 20)));
                if let Some(set) = bgm_sets.get(&t_h(d, 18)) {
                    for b in set {
                        if !bgm_ids.contains(b) {
                            bgm_ids.push(*b);
                        }
                    }
                }
                list.push(Val::M(m));
            }
            e.set("difficulties", Val::L(list));
            let idx = cx.push(e);
            if !bgm_ids.is_empty() {
                match_bgm.push((idx, bgm_ids));
            }
        }
    }

    // ================================================================ menus
    if let Some(t) = t2b("menu") {
        for r in t2b_rows(t, "MENU_CREATE_INFO") {
            if let Some(n) = t_s(r, 0) {
                cx.push(Entity::new(Category::Menu, n, crc(n)));
            }
        }
    }

    // ================================================================ audio
    if opts.audio {
        progress("audio", "bancos BGM / SE (cabeceras ACB)");
        let maps: HashMap<u32, String> = cx.ents.iter().filter(|e| e.category == Category::Map).map(|e| (e.hash, e.id.clone())).collect();
        let charas: HashSet<String> = cx.ents.iter().filter(|e| e.category == Category::Character).map(|e| e.id.clone()).collect();
        for e in crate::audio::index_audio(&src, &root_banks, &voice_banks, rdb("bgm"), t2b("map_sound"), &maps, &charas, &mut warnings) {
            cx.push(e);
        }
        // match -> bgm ids
        for (mi, ids) in match_bgm {
            let names: Vec<String> = ids.iter().map(|h| cx.id_of(Category::Bgm, *h).map_or_else(|| hex(*h), str::to_string)).collect();
            cx.ents[mi].set("bgm", Val::L(names.into_iter().map(Val::S).collect()));
        }
        // technique -> its SE track (ev60_<digits>_me) when that cue exists
        let se: HashSet<String> = cx.ents.iter().filter(|e| e.category == Category::Se).map(|e| e.id.clone()).collect();
        for e in cx.ents.iter_mut().filter(|e| e.category == Category::Technique) {
            if e.id.len() >= 8 && e.id.is_char_boundary(3) {
                let cue = format!("ev60_{}_me", &e.id[3..8]);
                if se.contains(&cue) {
                    e.set("se_cue", cue);
                }
            }
        }
    }

    let mut index = crate::Index::from_parts(Meta::default(), cx.ents, if opts.texts { texts.to_tables() } else { BTreeMap::new() });
    warnings.extend(cx.warnings);

    // ================================================================ thumbnails
    let mut nthumbs = 0;
    if let (true, Some(out)) = (opts.thumbs, out_dir) {
        progress("miniaturas", "caras, emblemas, keshin y armaduras");
        nthumbs = thumbs::extract(&src, &mut index, out, opts.thumb_size, progress, &mut warnings)?;
    }

    // ================================================================ meta
    let steam_build = source::steam_build_id(game_dir);
    let mut counts = BTreeMap::new();
    for e in index.entities() {
        *counts.entry(e.category.as_str().to_string()).or_insert(0) += 1;
    }
    let meta = Meta {
        schema_version: SCHEMA_VERSION,
        tool_version: env!("CARGO_PKG_VERSION").to_string(),
        game_version: steam_build.and_then(source::game_version_of_build).map(str::to_string),
        steam_build,
        fingerprint: src.fingerprint(),
        game_dir: game_dir.display().to_string(),
        cpk_list: src.list_path.display().to_string(),
        include_mods: opts.source.include_mods,
        built_at: std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs()),
        build_ms: t0.elapsed().as_millis() as u64,
        counts,
        tables: paths.values().map(|p| p.rsplit('/').next().unwrap_or(p).to_string()).collect(),
        warnings,
        read: src.stats.lock().ok().map(|s| s.clone()),
        thumbs: nthumbs,
    };
    index.set_meta(meta);
    Ok(index)
}

/// Where the index of a game lives by default: `<out>` as given, or `%LOCALAPPDATA%\VR-ModLoader\index`.
pub fn default_out_dir() -> PathBuf {
    std::env::var_os("LOCALAPPDATA").map(PathBuf::from).unwrap_or_else(std::env::temp_dir).join("VR-ModLoader").join("index")
}

