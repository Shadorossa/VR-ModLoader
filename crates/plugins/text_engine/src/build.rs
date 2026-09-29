//! The text merge (text-specific, on top of [`crate::fw::layer`]): every active mod's `[new]` / `[replace]` → one
//! claim per (language, table, id, variant) → the winner is written into the base table of that language.
//!
//! 1. **New texts** get a stable id: `crc32("<mod id>.<name>")`; when that collides with a text id of the game (all
//!    root tables of `en` + the target table in every language) or of a key sorted before it, `crc32("<key>#1")`,
//!    `#2`… (keys are allocated in sorted order, so the result does not depend on the load order).
//! 2. **Claims**: for each of the 9 languages the mod's value by the fallback chain (language → `all` → the mod's
//!    default language → for new texts the first language written); precedence tiers in [`crate::decl`]. Replacing
//!    another mod's text (`othermod.key`) is a claim on the same row. Conflicts between explicit values of two mods
//!    → WARN, the mod that loads later wins.
//! 3. **Apply**: per (language, table): the row is found → its string is set; not found and the claim belongs to a new
//!    text → a row is added (cloned layout, list count raised); else WARN. A key without a table is searched in the
//!    root tables of each language (every table that has the row is changed).

use crate::decl::{self, ModText};
use crate::fw::layer::{Claim, Layer};
use crate::fw::Notes;
use crate::keys::{self, CharaIndex, KeyRef};
use crate::lang::{LANGS, ROOT_TABLES};
use crate::table::{Kind, TextTable};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

/// A base table (bytes + where it came from).
#[derive(Debug, Clone)]
pub struct Base {
    pub bytes: Vec<u8>,
    pub label: String,
}

/// Where the build reads base tables.
pub trait Tables {
    /// Base of `text/<lang>/<table>`: Ok(None) = no such file in that language.
    fn table(&mut self, lang: &str, table: &str) -> Result<Option<Base>, String>;
    /// `chara_base` (for `chara.<id>.<field>` keys).
    fn chara_base(&mut self) -> Result<Vec<u8>, String>;
}

/// One active mod with texts, in load order.
#[derive(Debug, Clone)]
pub struct ModInput {
    pub id: String,
    pub load_index: u32,
    pub text: ModText,
}

/// A new text: its id and where it lives.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct KeyInfo {
    pub id: u32,
    pub table: String,
    pub kind: Kind,
    pub owner: String,
}

/// A replace key a mod used, resolved.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Ref {
    pub table: String,
    pub id: u32,
    pub variant: i32,
}

/// What Lua / other plugins need after the build (kept in the cache manifest).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Index {
    /// New texts: `<mod id>.<name>` → id.
    pub keys: BTreeMap<String, KeyInfo>,
    /// Replace keys of the mods → row (bare keys: the first table where the row was found).
    pub refs: BTreeMap<String, Ref>,
    /// Final strings of the rows the mods set or added: lang → table → `"<id>#<variant>"` → text.
    pub texts: BTreeMap<String, BTreeMap<String, BTreeMap<String, String>>>,
}

pub fn row_key(id: u32, variant: i32) -> String {
    format!("{id}#{variant}")
}

impl Index {
    /// A mod-set string (`table` None = any table).
    pub fn text(&self, lang: &str, table: Option<&str>, id: u32, variant: i32) -> Option<&str> {
        let by_table = self.texts.get(lang)?;
        let rk = row_key(id, variant);
        match table {
            Some(t) => by_table.get(t)?.get(&rk).map(String::as_str),
            None => by_table.values().find_map(|m| m.get(&rk)).map(String::as_str),
        }
    }
}

/// A merged table.
#[derive(Debug, Clone)]
pub struct Output {
    pub lang: &'static str,
    pub table: String,
    /// The merged file (unpadded).
    pub bytes: Vec<u8>,
    pub base_len: usize,
    pub base_label: String,
    /// Mods whose values went in (load order).
    pub mods: Vec<String>,
    pub changed: usize,
    pub added: usize,
}

#[derive(Debug, Clone, Default)]
pub struct Built {
    pub outputs: Vec<Output>,
    pub index: Index,
    pub notes: Notes,
}

/// Base bytes cached per (lang, table); tables are parsed on use (9 × `chara_text` parsed at once would be ~100 MB).
struct Loader<'a> {
    src: &'a mut dyn Tables,
    bytes: HashMap<(String, String), Option<Base>>,
    notes: Notes,
}

impl Loader<'_> {
    fn base(&mut self, lang: &str, table: &str) -> Option<&Base> {
        let k = (lang.to_string(), table.to_string());
        if !self.bytes.contains_key(&k) {
            let b = match self.src.table(lang, table) {
                Ok(b) => b,
                Err(e) => {
                    self.notes.error(format!("text/{lang}/{table}: {e}"));
                    None
                }
            };
            self.bytes.insert(k.clone(), b);
        }
        self.bytes.get(&k).and_then(Option::as_ref)
    }

    fn parsed(&mut self, lang: &str, table: &str) -> Option<(TextTable, usize, String)> {
        let b = self.base(lang, table)?.clone();
        match TextTable::parse(&b.bytes) {
            Ok(t) => Some((t, b.bytes.len(), b.label)),
            Err(e) => {
                self.notes.error(format!("text/{lang}/{table} ({}): {e}", b.label));
                self.bytes.insert((lang.to_string(), table.to_string()), None);
                None
            }
        }
    }
}

type RowId = (String, String, u32, i32);
type Groups = BTreeMap<(String, String), Vec<(u32, i32, Claim<String>)>>;
type Summary = BTreeMap<String, (Vec<&'static str>, usize, usize, BTreeSet<String>)>;

/// Collapse per-language messages: `(message key) → languages`.
#[derive(Default)]
struct PerLang(BTreeMap<String, Vec<&'static str>>);

impl PerLang {
    fn add(&mut self, msg: String, lang: &'static str) {
        let v = self.0.entry(msg).or_default();
        if !v.contains(&lang) {
            v.push(lang);
        }
    }
    fn langs(v: &[&str]) -> String {
        if v.len() == LANGS.len() {
            "all languages".to_string()
        } else {
            v.join(", ")
        }
    }
}

fn lang_static(l: &str) -> &'static str {
    LANGS.iter().copied().find(|x| *x == l).unwrap_or("??")
}

/// Merge the texts of `mods` (load order) over the base tables of `src`.
pub fn build(mods: &[ModInput], src: &mut dyn Tables) -> Built {
    let mut ld = Loader { src, bytes: HashMap::new(), notes: Notes::default() };
    let mut notes = Notes::default();
    let mut index = Index::default();
    for m in mods {
        for w in &m.text.warnings {
            notes.warn(format!("{}: {w}", m.id));
        }
    }

    // ---- 1. new texts and their ids
    struct NewKey<'a> {
        full: String,
        name: &'a str,
        m: &'a ModInput,
        table: String,
        kind: Kind,
    }
    let mut news: Vec<NewKey> = Vec::new();
    for m in mods {
        for name in m.text.new_names() {
            let (table, kind, w) = m.text.new_meta(name);
            for x in w {
                notes.warn(format!("{}: {x}", m.id));
            }
            news.push(NewKey { full: decl::full_key(&m.id, name), name, m, table, kind });
        }
    }
    news.sort_by(|a, b| a.full.cmp(&b.full));
    news.dedup_by(|a, b| a.full == b.full);
    if !news.is_empty() {
        let mut used: HashSet<u32> = HashSet::new();
        let mut tables: BTreeSet<(&str, String)> = ROOT_TABLES.iter().map(|t| ("en", t.to_string())).collect();
        for nk in &news {
            for l in LANGS {
                tables.insert((l, nk.table.clone()));
            }
        }
        for (l, t) in &tables {
            if let Some((tt, _, _)) = ld.parsed(l, t) {
                used.extend(tt.ids());
            }
        }
        for nk in &news {
            let first = vr_framework::ids::crc32(&nk.full);
            let id = vr_framework::ids::probe_id(&nk.full, |id| used.contains(&id));
            if id != first {
                notes.info(format!("{}: id of new text `{}` is {id:#010X} (crc32 {first:#010X} is already a text id)", nk.m.id, nk.full));
            }
            used.insert(id);
            index.keys.insert(nk.full.clone(), KeyInfo { id, table: nk.table.clone(), kind: nk.kind, owner: nk.m.id.clone() });
        }
    }

    // ---- 2. claims
    let mut layer: Layer<RowId, String> = Layer::new(decl::TIER_ALL);
    let mut adds: HashMap<RowId, Kind> = HashMap::new();
    let mut hints: HashMap<RowId, Kind> = HashMap::new();
    fn claim(layer: &mut Layer<RowId, String>, k: RowId, text: &str, tier: u8, m: &ModInput, origin: &str) {
        layer.claim(k, Claim { value: text.to_string(), tier, load_index: m.load_index, owner: m.id.clone(), origin: origin.to_string() });
    }
    for nk in &news {
        let ki = &index.keys[&nk.full];
        for l in LANGS {
            if let Some((d, tier)) = nk.m.text.resolve_new(nk.name, l) {
                let k: RowId = (l.to_string(), ki.table.clone(), ki.id, 0);
                adds.insert(k.clone(), ki.kind);
                claim(&mut layer, k, &d.text, tier, nk.m, &format!("new {}", nk.full));
            }
        }
    }
    let is_mod_key = |k: &str| index.keys.contains_key(k);
    let parsed: Vec<(&ModInput, &str, Result<KeyRef, String>)> =
        mods.iter().flat_map(|m| m.text.replace_keys().into_iter().map(move |k| (m, k))).map(|(m, k)| (m, k, keys::parse_key(k, &is_mod_key))).collect();
    let chara: Result<CharaIndex, String> = if parsed.iter().any(|(_, _, r)| matches!(r, Ok(KeyRef::Alias { .. }))) {
        ld.src.chara_base().and_then(|b| CharaIndex::from_chara_base(&b))
    } else {
        Err("not loaded".into())
    };
    if let (Err(e), true) = (&chara, parsed.iter().any(|(_, _, r)| matches!(r, Ok(KeyRef::Alias { .. })))) {
        notes.error(format!("chara_base: {e}: chara.<id>.<field> keys are skipped"));
    }
    let mut bare_missing = PerLang::default();
    for (m, rkey, r) in &parsed {
        let origin = format!("replace {rkey}");
        let target = match r {
            Err(e) => {
                notes.warn(format!("{}: replace `{rkey}`: {e} (skipped)", m.id));
                continue;
            }
            Ok(KeyRef::Mod(full)) => {
                let ki = &index.keys[full];
                Some((ki.table.clone(), Some(ki.kind), ki.id, 0, true))
            }
            Ok(k @ (KeyRef::Alias { .. } | KeyRef::Table { .. })) => match keys::target_of(k, chara.as_ref().map_err(String::as_str)) {
                Ok(Some((t, hint, id, v))) => Some((t, hint, id, v, false)),
                Ok(None) => None,
                Err(e) => {
                    notes.warn(format!("{}: replace `{rkey}`: {e} (skipped)", m.id));
                    continue;
                }
            },
            Ok(KeyRef::Bare { .. }) => None,
        };
        match (target, r) {
            (Some((table, hint, id, v, is_new)), _) => {
                index.refs.insert(rkey.to_string(), Ref { table: table.clone(), id, variant: v });
                for l in LANGS {
                    if let Some((text, tier)) = m.text.resolve_replace(rkey, l) {
                        let k: RowId = (l.to_string(), table.clone(), id, v);
                        if let Some(h) = hint {
                            if is_new {
                                adds.insert(k.clone(), h);
                            } else {
                                hints.insert(k.clone(), h);
                            }
                        }
                        claim(&mut layer, k, text, tier, m, &origin);
                    }
                }
            }
            (None, Ok(KeyRef::Bare { id, variant })) => {
                for l in LANGS {
                    let Some((text, tier)) = m.text.resolve_replace(rkey, l) else { continue };
                    let mut found = false;
                    for t in ROOT_TABLES {
                        let hit = ld.parsed(l, t).is_some_and(|(tt, _, _)| tt.find(None, *id, *variant).is_some());
                        if hit {
                            found = true;
                            index.refs.entry(rkey.to_string()).or_insert(Ref { table: t.to_string(), id: *id, variant: *variant });
                            claim(&mut layer, (l.to_string(), t.to_string(), *id, *variant), text, tier, m, &origin);
                        }
                    }
                    if !found {
                        bare_missing.add(format!("{}: replace `{rkey}`: no text {id:#010X}#{variant} in any root table of", m.id), l);
                    }
                }
            }
            _ => {}
        }
    }
    for (msg, langs) in &bare_missing.0 {
        notes.warn(format!("{msg} {} (skipped; write <table>:<key> for other tables)", PerLang::langs(langs)));
    }

    // conflicts, one line per row and pair of mods
    let mut conf = PerLang::default();
    for c in &layer.conflicts {
        let (l, t, id, v) = &c.key;
        conf.add(
            format!(
                "text conflict: {t}[{id:#010X}#{v}]: {} = {:?}, {} = {:?} (winner: {}) in",
                c.loser.owner, c.loser.value, c.winner.owner, c.winner.value, c.winner.owner
            ),
            lang_static(l),
        );
    }
    for (msg, langs) in &conf.0 {
        notes.warn(format!("{msg} {}", PerLang::langs(langs)));
    }

    // ---- 3. apply per (lang, table)
    let mut groups: Groups = BTreeMap::new();
    for ((l, t, id, v), c) in layer.into_map() {
        groups.entry((l, t)).or_default().push((id, v, c));
    }
    let mut missing_file = PerLang::default();
    let mut not_found = PerLang::default();
    let mut summary: Summary = BTreeMap::new();
    let mut outputs = Vec::new();
    // language by language (drop parsed tables early)
    for ((l, t), claims) in groups {
        let lang = lang_static(&l);
        let Some((mut tt, base_len, label)) = ld.parsed(&l, &t) else {
            let owners: BTreeSet<&str> = claims.iter().map(|(_, _, c)| c.owner.as_str()).collect();
            missing_file.add(format!("text/<lang>/{t}: no such text file for {} text(s) of {} in", claims.len(), owners.into_iter().collect::<Vec<_>>().join(", ")), lang);
            continue;
        };
        let mut by_kind: BTreeMap<Kind, Vec<(u32, i32, String)>> = BTreeMap::new();
        let (mut changed, mut owners) = (0usize, Vec::<(u32, String)>::new());
        let mut applied: Vec<(u32, i32, String)> = Vec::new();
        for (id, v, c) in claims {
            let rid: RowId = (l.clone(), t.clone(), id, v);
            let add = adds.get(&rid).copied();
            match tt.find(add.or(hints.get(&rid).copied()), id, v) {
                Some((_, idx)) => {
                    if tt.set_text(idx, &c.value) {
                        changed += 1;
                        owners.push((c.load_index, c.owner.clone()));
                    }
                    applied.push((id, v, c.value));
                }
                None => match add {
                    Some(k) => {
                        by_kind.entry(k).or_default().push((id, v, c.value.clone()));
                        owners.push((c.load_index, c.owner.clone()));
                        applied.push((id, v, c.value));
                    }
                    None => not_found.add(format!("{}: {}: no text {id:#010X}#{v} in text/<lang>/{t} of", c.owner, c.origin), lang),
                },
            }
        }
        let mut added = 0usize;
        for (k, rows) in by_kind {
            match tt.add_rows(k, &rows) {
                Ok(()) => added += rows.len(),
                Err(e) => {
                    notes.warn(format!("text/{l}/{t}: {} new text(s) not added: {e}", rows.len()));
                    applied.retain(|(id, v, _)| !rows.iter().any(|(i2, v2, _)| i2 == id && v2 == v));
                }
            }
        }
        let tex = index.texts.entry(l.clone()).or_default().entry(t.clone()).or_default();
        for (id, v, s) in applied {
            tex.insert(row_key(id, v), s);
        }
        if changed + added == 0 {
            continue;
        }
        let bytes = match tt.to_bytes() {
            Ok(b) => b,
            Err(e) => {
                notes.error(format!("text/{l}/{t}: cannot write the merged table: {e}"));
                continue;
            }
        };
        owners.sort();
        owners.dedup();
        let mods_in: Vec<String> = {
            let mut v: Vec<String> = Vec::new();
            for (_, o) in owners {
                if !v.contains(&o) {
                    v.push(o);
                }
            }
            v
        };
        let s = summary.entry(t.clone()).or_default();
        s.0.push(lang);
        s.1 += changed;
        s.2 += added;
        s.3.extend(mods_in.iter().cloned());
        outputs.push(Output { lang, table: t, bytes, base_len, base_label: label, mods: mods_in, changed, added });
    }
    for (msg, langs) in &missing_file.0 {
        notes.warn(format!("{msg} {} (skipped)", PerLang::langs(langs)));
    }
    for (msg, langs) in &not_found.0 {
        notes.warn(format!("{msg} {} (skipped)", PerLang::langs(langs)));
    }
    for (t, (langs, ch, ad, ms)) in &summary {
        notes.info(format!(
            "text/{t}: {} changed, {} added in {} by {}",
            ch,
            ad,
            PerLang::langs(langs),
            ms.iter().cloned().collect::<Vec<_>>().join(", ")
        ));
    }
    let mut all = ld.notes;
    all.extend(notes);
    Built { outputs, index, notes: all }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::fw::discover::DeclFile;
    use crate::table::tests::sample;

    /// Every language has the sample `menu_text` (+ a `chara_text` with Mark's noun rows) unless `missing` says no.
    pub struct Fake {
        pub missing: Vec<(String, String)>,
        pub reads: usize,
    }

    pub fn chara_text() -> Vec<u8> {
        use l5_core::hash::crc32_str;
        use l5_core::t2b::{Entry, T2b, Value};
        let e = |n: &str, v: Vec<Value>| Entry { name: Some(n.into()), hash: crc32_str(n), values: v };
        let noun = |id: u32, form: i32, s: &str| {
            let mut v = vec![Value::Int(id as i32), Value::Int(form), Value::String(None), Value::String(None), Value::String(None), Value::String(Some(s.into()))];
            v.extend(std::iter::repeat_n(Value::String(None), 4));
            v.extend(std::iter::repeat_n(Value::Int(0), 5));
            e("NOUN_INFO", v)
        };
        T2b {
            entries: vec![
                e("TEXT_INFO_BEGIN", vec![Value::Int(0)]),
                e("TEXT_INFO_END", vec![]),
                e("NOUN_INFO_BEGIN", vec![Value::Int(3)]),
                noun(0xE5530F12, 0, "Mark Evans"),
                noun(0xE5530F12, 11, "Evans"),
                noun(0xE5530F12, 12, "Mark"),
                e("NOUN_INFO_END", vec![]),
            ],
            ..Default::default()
        }
        .to_bytes()
        .unwrap()
    }

    pub fn chara_base() -> Vec<u8> {
        use l5_core::hash::crc32_str;
        use l5_core::t2b::{Entry, T2b, Value};
        let mut v: Vec<Value> = (0..34).map(|_| Value::Int(0)).collect();
        v[0] = Value::Int(crc32_str("c01000010") as i32);
        v[1] = Value::String(Some("c01000010".into()));
        v[3] = Value::Int(0xE5530F12u32 as i32);
        v[4] = Value::Int(2);
        v[5] = Value::Int(3);
        v[19] = Value::Int(4);
        T2b { entries: vec![Entry { name: Some("CHARA_BASE_INFO".into()), hash: crc32_str("CHARA_BASE_INFO"), values: v }], ..Default::default() }.to_bytes().unwrap()
    }

    impl Tables for Fake {
        fn table(&mut self, lang: &str, table: &str) -> Result<Option<Base>, String> {
            self.reads += 1;
            if self.missing.iter().any(|(l, t)| (l == lang || l == "*") && t == table) {
                return Ok(None);
            }
            Ok(match table {
                "menu_text" => Some(Base { bytes: sample(), label: "game".into() }),
                "chara_text" => Some(Base { bytes: chara_text(), label: "game".into() }),
                _ => None,
            })
        }
        fn chara_base(&mut self) -> Result<Vec<u8>, String> {
            Ok(chara_base())
        }
    }

    pub fn modin(id: &str, li: u32, files: &[(&str, &str)]) -> ModInput {
        let f: Vec<DeclFile> = files.iter().map(|(r, t)| DeclFile { rel: r.to_string(), text: t.to_string() }).collect();
        ModInput { id: id.into(), load_index: li, text: ModText::parse(id, &f) }
    }

    fn out(b: &Built, l: &str, t: &str) -> TextTable {
        TextTable::parse(&b.outputs.iter().find(|o| o.lang == l && o.table == t).unwrap_or_else(|| panic!("no output {l}/{t}")).bytes).unwrap()
    }

    fn txt(t: &TextTable, kind: Option<Kind>, id: u32, v: i32) -> Option<String> {
        t.find(kind, id, v).and_then(|(_, i)| t.text(i).map(str::to_string))
    }

    #[test]
    fn rename_add_fallback_and_conflicts() {
        let a = modin(
            "moda",
            1,
            &[
                ("text/en.toml", "[new]\ngreeting = \"Hello\"\n[replace]\n\"chara.c01000010.name\" = \"Mark MOD\"\n\"chara.c01000010.given\" = \"Marky\"\n\"menu_text:10\" = \"Hi A\"\n"),
                ("text/es.toml", "[new]\ngreeting = \"Hola\"\n[replace]\n\"menu_text:10\" = \"Hola A\"\n"),
            ],
        );
        let b = modin(
            "modb",
            2,
            &[
                ("text/en.toml", "[replace]\n\"menu_text:10\" = \"Hi B\"\n\"moda.greeting\" = \"Hello from B\"\n\"20\" = \"World B\"\n\"999\" = \"nothing\"\n\"menu_text:12345\" = \"no row\"\n"),
                ("text/fr.toml", "[replace]\n\"moda.greeting\" = \"Bonjour\"\n"),
            ],
        );
        let mut src = Fake { missing: vec![], reads: 0 };
        let r = build(&[a, b], &mut src);
        let gid = l5_core::hash::crc32_str("moda.greeting");
        assert_eq!(r.index.keys["moda.greeting"], KeyInfo { id: gid, table: "menu_text".into(), kind: Kind::Text, owner: "moda".into() });
        // en: B (later) wins menu_text:10 and moda.greeting; the new row exists
        let en = out(&r, "en", "menu_text");
        assert_eq!(txt(&en, None, 10, 0).as_deref(), Some("Hi B"));
        assert_eq!(txt(&en, None, gid, 0).as_deref(), Some("Hello from B"));
        assert_eq!(txt(&en, None, 20, 0).as_deref(), Some("World B"));
        // es: A's own Spanish beats B's default-language fallback; fr: B's explicit French; de: both are default-language
        // fallbacks (en) → the later mod (B)
        let es = out(&r, "es", "menu_text");
        assert_eq!((txt(&es, None, 10, 0).as_deref(), txt(&es, None, gid, 0).as_deref()), (Some("Hola A"), Some("Hola")));
        assert_eq!(txt(&out(&r, "fr", "menu_text"), None, gid, 0).as_deref(), Some("Bonjour"));
        let de = out(&r, "de", "menu_text");
        assert_eq!((txt(&de, None, gid, 0).as_deref(), txt(&de, None, 10, 0).as_deref()), (Some("Hello from B"), Some("Hi B")));
        // chara alias: every language (default-language fallback), only the addressed forms
        let ja = out(&r, "ja", "chara_text");
        assert_eq!(txt(&ja, Some(Kind::Noun), 0xE5530F12, 0).as_deref(), Some("Mark MOD"));
        assert_eq!(txt(&ja, Some(Kind::Noun), 0xE5530F12, 12).as_deref(), Some("Marky"));
        assert_eq!(txt(&ja, Some(Kind::Noun), 0xE5530F12, 11).as_deref(), Some("Evans"));
        // notes: conflict on menu_text:10 (en; de/… are A's fallback so no conflict there), missing rows
        let warns: Vec<&str> = r.notes.at(crate::fw::Lvl::Warn).collect();
        assert!(warns.iter().any(|w| w.starts_with("text conflict: menu_text[0x0000000A#0]: moda = \"Hi A\", modb = \"Hi B\" (winner: modb) in en")), "{warns:#?}");
        assert!(warns.iter().any(|w| w.contains("conflict") && w.contains(&format!("{gid:#010X}"))), "{warns:#?}");
        assert!(warns.iter().any(|w| w.contains("replace `999`") && w.contains("any root table")), "{warns:#?}");
        assert!(warns.iter().any(|w| w.contains("replace menu_text:12345") && w.contains("0x00003039")), "{warns:#?}");
        // index for Lua
        assert_eq!(r.index.text("fr", None, gid, 0), Some("Bonjour"));
        assert_eq!(r.index.text("en", Some("menu_text"), 10, 0), Some("Hi B"));
        assert_eq!(r.index.refs["chara.c01000010.name"], Ref { table: "chara_text".into(), id: 0xE5530F12, variant: 0 });
        assert_eq!(r.index.refs["20"].table, "menu_text");
        // one output per touched (lang, table): 9 menu_text + 9 chara_text
        assert_eq!(r.outputs.len(), 18);
        assert!(r.outputs.iter().all(|o| o.base_len > 0 && o.base_label == "game"));
    }

    #[test]
    fn ids_avoid_collisions_and_do_not_depend_on_load_order() {
        // two load orders give the same ids
        let a = || modin("a", 0, &[("text/en.toml", "[new]\nx = \"1\"\ny = \"2\"\n")]);
        let b = || modin("b", 1, &[("text/en.toml", "[new]\nx = \"3\"\n")]);
        let mut src = Fake { missing: vec![], reads: 0 };
        let r1 = build(&[a(), b()], &mut src);
        let mut b2 = b();
        b2.load_index = 0;
        let mut a2 = a();
        a2.load_index = 1;
        let r2 = build(&[b2, a2], &mut src);
        assert_eq!(r1.index.keys.iter().map(|(k, v)| (k.clone(), v.id)).collect::<Vec<_>>(), r2.index.keys.iter().map(|(k, v)| (k.clone(), v.id)).collect::<Vec<_>>());
        assert_eq!(r1.index.keys.len(), 3);
        // allocation skips taken ids: pretend the game already has crc32("a.x")
        let taken = l5_core::hash::crc32_str("a.x");
        struct Taken(u32);
        impl Tables for Taken {
            fn table(&mut self, _l: &str, t: &str) -> Result<Option<Base>, String> {
                if t != "menu_text" {
                    return Ok(None);
                }
                let mut tt = TextTable::parse(&sample()).unwrap();
                tt.add_rows(Kind::Text, &[(self.0, 0, "game".into())]).unwrap();
                Ok(Some(Base { bytes: tt.to_bytes().unwrap(), label: "game".into() }))
            }
            fn chara_base(&mut self) -> Result<Vec<u8>, String> {
                Err("none".into())
            }
        }
        let r = build(&[a()], &mut Taken(taken));
        let id = r.index.keys["a.x"].id;
        assert_eq!(id, l5_core::hash::crc32_str("a.x#1"));
        let en = out(&r, "en", "menu_text");
        assert_eq!(txt(&en, None, taken, 0).as_deref(), Some("game"));
        assert_eq!(txt(&en, None, id, 0).as_deref(), Some("1"));
    }

    #[test]
    fn missing_tables_and_bad_keys_are_reported_not_fatal() {
        let m = modin(
            "m",
            0,
            &[(
                "text.toml",
                "[en.new]\nn = { text = \"Name\", table = \"chara_text\" }\nq = { text = \"Q\", table = \"quest_title_text\" }\n[en.replace]\n\"chara.c09999999.name\" = \"x\"\n\"Bad:1\" = \"y\"\n",
            )],
        );
        let mut src = Fake { missing: vec![("ja".into(), "chara_text".into())], reads: 0 };
        let r = build(&[m], &mut src);
        let warns: Vec<&str> = r.notes.at(crate::fw::Lvl::Warn).collect();
        assert!(warns.iter().any(|w| w.contains("c09999999") && w.contains("not in chara_base")), "{warns:#?}");
        assert!(warns.iter().any(|w| w.contains("Bad:1")), "{warns:#?}");
        assert!(warns.iter().any(|w| w.contains("text/<lang>/quest_title_text: no such text file") && w.contains("all languages")), "{warns:#?}");
        assert!(warns.iter().any(|w| w.contains("text/<lang>/chara_text: no such text file") && w.ends_with("in ja (skipped)")), "{warns:#?}");
        // the noun went into chara_text of the 8 other languages
        let id = r.index.keys["m.n"].id;
        assert_eq!(r.index.keys["m.n"].kind, Kind::Noun);
        assert_eq!(txt(&out(&r, "en", "chara_text"), Some(Kind::Noun), id, 0).as_deref(), Some("Name"));
        assert_eq!(r.outputs.len(), 8);
    }
}
