//! Data model of the index (what is saved to disk and what the Studio / engines see).

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// Bump when the on-disk layout or the meaning of a field changes: older indexes are rebuilt.
pub const SCHEMA_VERSION: u32 = 1;

/// Game languages, in the order used by every `names` array.
pub const LANGS: [&str; 9] = ["ja", "en", "es", "fr", "de", "it", "pt", "zh_hans", "zh_hant"];

/// Index of a language code in [`LANGS`] (`"es"` -> 2). Accepts `zh-hans` / `zh_Hans` too.
pub fn lang_index(code: &str) -> Option<usize> {
    let c = code.to_ascii_lowercase().replace('-', "_");
    LANGS.iter().position(|l| *l == c)
}

/// What kind of game thing an entity is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Category {
    /// `chara_base` row (`c01000010`, `npc0010`, `k000020` keshin bodies…).
    Character,
    /// `chara_param` row: a playable variant ("spirit") of a character (`pc_para_c01000010` or `0xC19FE60C`).
    Variant,
    /// Hissatsu (`skill_config`): `whs` shoot, `who` offense, `whd` defense, `whk` keeper.
    Technique,
    /// Keshin (`wks…`, aura command type 0).
    Keshin,
    /// Keshin armour / armed form (`was` `wao` `wad` `wak`…).
    Armour,
    /// Mixi Max (`wmm…`).
    Miximax,
    /// Other aura commands (soul, mode change…), `aura_type` field says which.
    Aura,
    /// Passive skill (`ps…`, `cps…`).
    Passive,
    /// `team_config` SOCCER_TEAM_INFO (`tm_st_game_0101a`).
    Team,
    /// Formation (`fm0101`, the formation item key).
    Formation,
    /// Item (every `ITEM_*_INFO` list except emblems and hissatsu manuals).
    Item,
    /// Team emblem item (`em010001`).
    Emblem,
    /// Kit / uniform (`uniform_config`).
    Kit,
    /// Map / stadium (`map_data`, `kind` says which).
    Map,
    /// Match (`SOCCER_GAME_INFO`: `fbtl_st_0101`).
    Match,
    /// Music (`bgm_config` id / cue).
    Bgm,
    /// Sound effect cue of a language-independent bank.
    Se,
    /// Voice bank (`sound_asset/<ja|en>/<bank>.acb`).
    VoiceBank,
    /// Menu (`MENU_CREATE_INFO` name).
    Menu,
}

impl Category {
    pub const ALL: [Category; 19] = [
        Category::Character,
        Category::Variant,
        Category::Technique,
        Category::Keshin,
        Category::Armour,
        Category::Miximax,
        Category::Aura,
        Category::Passive,
        Category::Team,
        Category::Formation,
        Category::Item,
        Category::Emblem,
        Category::Kit,
        Category::Map,
        Category::Match,
        Category::Bgm,
        Category::Se,
        Category::VoiceBank,
        Category::Menu,
    ];

    /// snake_case name (`"voice_bank"`).
    pub fn as_str(self) -> &'static str {
        match self {
            Category::Character => "character",
            Category::Variant => "variant",
            Category::Technique => "technique",
            Category::Keshin => "keshin",
            Category::Armour => "armour",
            Category::Miximax => "miximax",
            Category::Aura => "aura",
            Category::Passive => "passive",
            Category::Team => "team",
            Category::Formation => "formation",
            Category::Item => "item",
            Category::Emblem => "emblem",
            Category::Kit => "kit",
            Category::Map => "map",
            Category::Match => "match",
            Category::Bgm => "bgm",
            Category::Se => "se",
            Category::VoiceBank => "voice_bank",
            Category::Menu => "menu",
        }
    }

    pub fn parse(s: &str) -> Option<Category> {
        let s = s.to_ascii_lowercase().replace('-', "_");
        Category::ALL.into_iter().find(|c| c.as_str() == s).or(match s.as_str() {
            "chara" | "characters" => Some(Category::Character),
            "hissatsu" | "skill" | "techniques" => Some(Category::Technique),
            "armor" | "armed" => Some(Category::Armour),
            "stadium" | "maps" => Some(Category::Map),
            "teams" => Some(Category::Team),
            "items" => Some(Category::Item),
            _ => None,
        })
    }
}

impl std::fmt::Display for Category {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A field value. Serialised untagged: numbers, strings, arrays, objects.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Val {
    I(i64),
    F(f64),
    S(String),
    L(Vec<Val>),
    M(BTreeMap<String, Val>),
}

impl Val {
    pub fn as_i64(&self) -> Option<i64> {
        match self {
            Val::I(v) => Some(*v),
            _ => None,
        }
    }
    pub fn as_str(&self) -> Option<&str> {
        match self {
            Val::S(s) => Some(s),
            _ => None,
        }
    }
    pub fn as_list(&self) -> Option<&[Val]> {
        match self {
            Val::L(v) => Some(v),
            _ => None,
        }
    }
}

impl From<i64> for Val {
    fn from(v: i64) -> Self {
        Val::I(v)
    }
}
impl From<i32> for Val {
    fn from(v: i32) -> Self {
        Val::I(v as i64)
    }
}
impl From<f32> for Val {
    fn from(v: f32) -> Self {
        Val::F(v as f64)
    }
}
impl From<&str> for Val {
    fn from(v: &str) -> Self {
        Val::S(v.to_string())
    }
}
impl From<String> for Val {
    fn from(v: String) -> Self {
        Val::S(v)
    }
}

/// A reference from one entity to another (resolved to the target's id at build time).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Link {
    /// Relation name (`"variant"`, `"member"`, `"formation"`, `"partner"`, `"stadium"`, `"opponent"`…).
    pub rel: String,
    pub category: Category,
    /// Target entity id.
    pub id: String,
}

/// One indexed game thing.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Entity {
    /// Readable internal id, as mod files write it (`c01000010`, `whs01980`, `tm_st_game_0101a`, `pc_para_c01000010`,
    /// or `0x1234ABCD` when the game has no name for the key).
    pub id: String,
    /// 32-bit key stored in the game tables (usually `crc32(id)`).
    pub hash: u32,
    pub category: Category,
    /// Display names in [`LANGS`] order; `""` = none in that language. Empty vector = no name of its own (variants
    /// take their character's names, see [`crate::Index::display_name`]).
    pub names: Vec<String>,
    /// Other searchable names (short / given / family names, readings).
    #[serde(default)]
    pub alt: Vec<String>,
    /// Key fields (English keys, values fixed in English where they are enums: `element = "fire"`).
    #[serde(default)]
    pub fields: BTreeMap<String, Val>,
    #[serde(default)]
    pub links: Vec<Link>,
    /// Thumbnail PNG, relative to the index folder (`thumbs/face/c01000010.png`).
    #[serde(default)]
    pub thumb: Option<String>,
}

impl Entity {
    pub fn new(category: Category, id: impl Into<String>, hash: u32) -> Self {
        Entity { id: id.into(), hash, category, names: Vec::new(), alt: Vec::new(), fields: BTreeMap::new(), links: Vec::new(), thumb: None }
    }

    /// Name in `lang` (index into [`LANGS`]), falling back to English, then Japanese, then any.
    pub fn name(&self, lang: usize) -> Option<&str> {
        let get = |i: usize| self.names.get(i).map(String::as_str).filter(|s| !s.is_empty());
        get(lang).or_else(|| get(1)).or_else(|| get(0)).or_else(|| self.names.iter().map(String::as_str).find(|s| !s.is_empty()))
    }

    pub fn field(&self, k: &str) -> Option<&Val> {
        self.fields.get(k)
    }

    pub fn links(&self, rel: &str) -> impl Iterator<Item = &Link> {
        self.links.iter().filter(move |l| l.rel == rel)
    }

    pub(crate) fn set(&mut self, k: &str, v: impl Into<Val>) {
        self.fields.insert(k.to_string(), v.into());
    }

    pub(crate) fn link(&mut self, rel: &str, category: Category, id: impl Into<String>) {
        self.links.push(Link { rel: rel.to_string(), category, id: id.into() });
    }
}

/// One text table (`data/common/text/<lang>/<file>.cfg.bin`): row-aligned ids and strings per language.
///
/// Stored compactly: one string per language with the rows joined by `\0` (fast to load), row offsets rebuilt
/// on load.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TextTable {
    /// Text ids (`crc32(key)`, unsigned), sorted.
    pub ids: Vec<u32>,
    cols: Vec<String>,
    #[serde(skip)]
    offs: Vec<Vec<u32>>,
}

impl TextTable {
    /// `strings[lang][row]` (lang in [`LANGS`] order; `""` = missing).
    pub fn new(ids: Vec<u32>, strings: Vec<Vec<String>>) -> Self {
        let cols = strings.into_iter().map(|col| col.iter().map(|s| s.replace('\0', "")).collect::<Vec<_>>().join("\0")).collect();
        let mut t = TextTable { ids, cols, offs: Vec::new() };
        t.rebuild();
        t
    }

    /// Recompute row offsets (after deserialising).
    pub(crate) fn rebuild(&mut self) {
        let n = self.ids.len();
        self.offs = self
            .cols
            .iter()
            .map(|c| {
                let mut o = Vec::with_capacity(n + 1);
                o.push(0u32);
                for (i, b) in c.bytes().enumerate() {
                    if b == 0 {
                        o.push(i as u32 + 1);
                    }
                }
                o.push(c.len() as u32 + 1);
                o
            })
            .collect();
    }

    pub fn len(&self) -> usize {
        self.ids.len()
    }

    pub fn is_empty(&self) -> bool {
        self.ids.is_empty()
    }

    /// String of row `row` in language `lang` (`""` when missing).
    pub fn row(&self, lang: usize, row: usize) -> &str {
        let (Some(c), Some(o)) = (self.cols.get(lang), self.offs.get(lang)) else { return "" };
        match (o.get(row), o.get(row + 1)) {
            (Some(&a), Some(&b)) if b > a && (b as usize - 1) <= c.len() => &c[a as usize..b as usize - 1],
            _ => "",
        }
    }

    pub fn get(&self, id: u32, lang: usize) -> Option<&str> {
        let row = self.ids.binary_search(&id).ok()?;
        Some(self.row(lang, row)).filter(|s| !s.is_empty())
    }

    /// Every language of one id.
    pub fn all(&self, id: u32) -> Option<Vec<String>> {
        let row = self.ids.binary_search(&id).ok()?;
        Some((0..LANGS.len()).map(|l| self.row(l, row).to_string()).collect())
    }
}

/// Build information stored with the index.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Meta {
    pub schema_version: u32,
    /// Crate version that wrote the index.
    pub tool_version: String,
    /// `"7.1.2"` when the Steam build id is known.
    pub game_version: Option<String>,
    pub steam_build: Option<u64>,
    /// [`crate::source::GameSource::fingerprint`] of the inputs.
    pub fingerprint: String,
    pub game_dir: String,
    pub cpk_list: String,
    /// Mod overlays were read (`include_mods`).
    pub include_mods: bool,
    /// Seconds since the Unix epoch.
    pub built_at: u64,
    pub build_ms: u64,
    /// Entities per category.
    pub counts: BTreeMap<String, usize>,
    /// Versioned tables actually read (`chara_base_1.03.98.00.cfg.bin`…).
    pub tables: Vec<String>,
    /// Non-fatal problems met while building.
    pub warnings: Vec<String>,
    pub read: Option<crate::source::ReadStats>,
    pub thumbs: usize,
}
