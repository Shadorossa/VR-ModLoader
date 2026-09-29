//! Name search: normalisation (case, accents, full-width, katakana -> hiragana), ranked fuzzy matching and
//! "readable name -> id" resolution with ambiguity reporting.

use serde::Serialize;

use crate::model::{Category, Entity, LANGS};

/// Search-friendly form: lower case, Latin accents removed, full-width ASCII folded, katakana -> hiragana,
/// punctuation -> space, spaces collapsed.
pub fn normalize(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut space = true;
    for c in s.chars() {
        let mut push = |c: char| {
            if c.is_alphanumeric() || c == '_' {
                out.push(c);
                space = false;
            } else if !space {
                out.push(' ');
                space = true;
            }
        };
        if c.is_ascii() {
            if !matches!(c, '\'' | '`') {
                push(c.to_ascii_lowercase());
            }
            continue;
        }
        match fold_char(c) {
            Fold::One(c) => push(c),
            Fold::Str(t) => t.chars().for_each(&mut push),
            Fold::Lower(c) => c.to_lowercase().for_each(&mut push),
        }
    }
    while out.ends_with(' ') {
        out.pop();
    }
    out
}

enum Fold {
    One(char),
    Str(&'static str),
    Lower(char),
}

fn fold_char(c: char) -> Fold {
    let c = match c as u32 {
        0xFF01..=0xFF5E => return Fold::One(char::from_u32(c as u32 - 0xFEE0).unwrap_or(c).to_ascii_lowercase()), // full-width ASCII
        0x30A1..=0x30F6 => return Fold::One(char::from_u32(c as u32 - 0x60).unwrap_or(c)),                          // katakana -> hiragana
        0x3000 => return Fold::One(' '),
        _ => c,
    };
    Fold::Str(match c {
        'á' | 'à' | 'â' | 'ä' | 'ã' | 'å' | 'ā' | 'Á' | 'À' | 'Â' | 'Ä' | 'Ã' | 'Å' | 'Ā' => "a",
        'é' | 'è' | 'ê' | 'ë' | 'ē' | 'É' | 'È' | 'Ê' | 'Ë' | 'Ē' => "e",
        'í' | 'ì' | 'î' | 'ï' | 'ī' | 'Í' | 'Ì' | 'Î' | 'Ï' | 'Ī' => "i",
        'ó' | 'ò' | 'ô' | 'ö' | 'õ' | 'ō' | 'ø' | 'Ó' | 'Ò' | 'Ô' | 'Ö' | 'Õ' | 'Ō' | 'Ø' => "o",
        'ú' | 'ù' | 'û' | 'ü' | 'ū' | 'Ú' | 'Ù' | 'Û' | 'Ü' | 'Ū' => "u",
        'ñ' | 'Ñ' => "n",
        'ç' | 'Ç' => "c",
        'ý' | 'ÿ' | 'Ý' => "y",
        'ß' => "ss",
        'œ' | 'Œ' => "oe",
        'æ' | 'Æ' => "ae",
        '’' | '´' => "",
        _ => return Fold::Lower(c),
    })
}

/// One searchable string of an entity.
#[derive(Debug, Clone)]
pub(crate) struct Key {
    pub ent: u32,
    /// Language index, 255 = id / alt name.
    pub lang: u8,
    pub text: String,
    /// ASCII letters / digits present (typo pre-filter), 0 for non-ASCII keys.
    pub mask: u64,
}

/// Bit set of the ASCII letters and digits of `s` (`None` if `s` is not ASCII).
pub(crate) fn char_mask(s: &str) -> Option<u64> {
    if !s.is_ascii() {
        return None;
    }
    let mut m = 0u64;
    for b in s.bytes() {
        match b {
            b'a'..=b'z' => m |= 1 << (b - b'a'),
            b'0'..=b'9' => m |= 1 << (26 + b - b'0'),
            b'_' => m |= 1 << 36,
            _ => {}
        }
    }
    Some(m)
}

/// Precomputed keys (built when the index is loaded).
#[derive(Debug, Default, Clone)]
pub(crate) struct SearchIndex {
    pub keys: Vec<Key>,
}

pub(crate) const ID_LANG: u8 = 254;
pub(crate) const ALT_LANG: u8 = 255;

impl SearchIndex {
    pub fn build(ents: &[Entity]) -> Self {
        let by_id: std::collections::HashMap<(Category, &str), usize> = ents.iter().enumerate().map(|(i, e)| ((e.category, e.id.as_str()), i)).collect();
        let mut keys = Vec::new();
        for (i, e) in ents.iter().enumerate() {
            let i = i as u32;
            let t = e.id.to_lowercase();
            keys.push(Key { ent: i, lang: ID_LANG, mask: char_mask(&t).unwrap_or(0), text: t });
            let mut names: &[String] = &e.names;
            let mut alt: &[String] = &e.alt;
            if names.is_empty() {
                // variants / voice banks: their character's names
                if let Some(c) = e.links("chara").next().and_then(|l| by_id.get(&(Category::Character, l.id.as_str()))) {
                    names = &ents[*c].names;
                    alt = &ents[*c].alt;
                }
            }
            let mut seen: Vec<String> = Vec::new();
            for (l, n) in names.iter().enumerate() {
                let t = normalize(n);
                if t.is_empty() || seen.contains(&t) {
                    continue;
                }
                seen.push(t.clone());
                keys.push(Key { ent: i, lang: l as u8, mask: char_mask(&t).unwrap_or(0), text: t });
            }
            for n in alt {
                let t = normalize(n);
                if t.is_empty() || seen.contains(&t) {
                    continue;
                }
                seen.push(t.clone());
                keys.push(Key { ent: i, lang: ALT_LANG, mask: char_mask(&t).unwrap_or(0), text: t });
            }
        }
        SearchIndex { keys }
    }
}

/// A search request.
#[derive(Debug, Clone, Default)]
pub struct Query {
    pub text: String,
    /// Only these categories (empty = all).
    pub categories: Vec<Category>,
    /// Language used for the returned `name` and preferred when ranking (index into [`LANGS`]; default English).
    pub lang: Option<usize>,
    /// Maximum number of hits (0 = 50).
    pub limit: usize,
    /// Field filters: every `(field, value)` must match the entity's field (string or number, case-insensitive).
    pub fields: Vec<(String, String)>,
}

impl Query {
    pub fn new(text: impl Into<String>) -> Self {
        Query { text: text.into(), ..Default::default() }
    }
    pub fn category(mut self, c: Category) -> Self {
        self.categories.push(c);
        self
    }
    pub fn lang(mut self, l: usize) -> Self {
        self.lang = Some(l);
        self
    }
    pub fn limit(mut self, n: usize) -> Self {
        self.limit = n;
        self
    }
    pub fn field(mut self, k: impl Into<String>, v: impl Into<String>) -> Self {
        self.fields.push((k.into(), v.into()));
        self
    }
}

/// One search result.
#[derive(Debug, Clone, Serialize)]
pub struct Hit {
    /// Position in [`crate::Index::entities`].
    pub index: usize,
    pub id: String,
    pub category: Category,
    /// Display name in the query language.
    pub name: String,
    /// The string that matched and its language (`"id"`, `"alt"` or a language code).
    pub matched: String,
    pub matched_lang: &'static str,
    pub score: u32,
}

pub(crate) fn lang_label(l: u8) -> &'static str {
    match l {
        ID_LANG => "id",
        ALT_LANG => "alt",
        l => LANGS.get(l as usize).copied().unwrap_or("?"),
    }
}

/// Optimal-string-alignment distance of two short byte strings, bounded (`max + 1` = "too far").
fn osa(a: &[u8], b: &[u8], max: usize) -> usize {
    const N: usize = 48;
    if a.len().abs_diff(b.len()) > max {
        return max + 1;
    }
    if a.len() >= N || b.len() >= N {
        return max + 1;
    }
    let m = b.len();
    let mut pp = [0usize; N];
    let mut p = [0usize; N];
    let mut c = [0usize; N];
    for (j, x) in p.iter_mut().enumerate().take(m + 1) {
        *x = j;
    }
    for i in 1..=a.len() {
        c[0] = i;
        let mut best = c[0];
        for j in 1..=m {
            let cost = usize::from(a[i - 1] != b[j - 1]);
            let mut v = (p[j] + 1).min(c[j - 1] + 1).min(p[j - 1] + cost);
            if i > 1 && j > 1 && a[i - 1] == b[j - 2] && a[i - 2] == b[j - 1] {
                v = v.min(pp[j - 2] + 1);
            }
            c[j] = v;
            best = best.min(v);
        }
        if best > max {
            return max + 1;
        }
        pp = p;
        p = c;
    }
    p[m]
}

/// Score of `key` for the normalised query (`None` = no match). Higher is better.
pub(crate) fn score(q: &str, qwords: &[&str], key: &str, typos: bool) -> Option<u32> {
    if key == q {
        return Some(1000);
    }
    if let Some(rest) = key.strip_prefix(q) {
        return Some(900 - rest.len().min(100) as u32);
    }
    if let Some(p) = key.find(q) {
        let word_start = key[..p].ends_with(' ');
        return Some(if word_start { 800 } else { 700 } - (p.min(50) as u32));
    }
    if qwords.len() > 1 && qwords.iter().all(|w| key.contains(w)) {
        return Some(600);
    }
    if !typos || !key.is_ascii() {
        return None;
    }
    // typo tolerance (Latin scripts), word by word
    let mut total = 0u32;
    for w in qwords {
        let wb = w.as_bytes();
        if wb.len() < 4 {
            if key.split(' ').any(|k| k.starts_with(w)) {
                total += 100;
                continue;
            }
            return None;
        }
        let max = if wb.len() <= 6 { 1 } else { 2 };
        let mut best = max + 1;
        for k in key.split(' ') {
            let kb = k.as_bytes();
            if kb.len() + max < wb.len() {
                continue;
            }
            let full = osa(wb, kb, max);
            // a longer word: compare with its prefix too (typing in progress)
            let pre = if kb.len() > wb.len() { osa(wb, &kb[..wb.len()], max) + 1 } else { max + 1 };
            best = best.min(full.min(pre));
            if best == 0 {
                break;
            }
        }
        if best > max {
            return None;
        }
        total += 100 - 25 * best as u32;
    }
    Some(300 + total / qwords.len().max(1) as u32 * 2)
}

/// Result of [`crate::Index::resolve`].
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum Resolution {
    /// Exactly one entity has this id or name.
    Unique { candidate: Candidate },
    /// Several entities match exactly: the tool must ask (or the mod must write the id).
    Ambiguous { candidates: Vec<Candidate> },
    /// No exact match; closest names.
    NotFound { suggestions: Vec<Candidate> },
}

impl Resolution {
    pub fn unique(&self) -> Option<&Candidate> {
        match self {
            Resolution::Unique { candidate } => Some(candidate),
            _ => None,
        }
    }
}

/// An entity proposed by [`Resolution`].
#[derive(Debug, Clone, Serialize)]
pub struct Candidate {
    pub index: usize,
    pub id: String,
    pub category: Category,
    pub name: String,
    /// Short description to tell candidates apart (`"GK · mountain · Inazuma Eleven"`).
    pub detail: String,
    /// How it matched: `id`, `name:<lang>`, `alt`, `fuzzy`.
    pub matched: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_folds() {
        assert_eq!(normalize("Trampolín  Relámpago!"), "trampolin relampago");
        assert_eq!(normalize("ＭＡＲＫ"), "mark");
        assert_eq!(normalize("エンドウ"), "えんどう");
        assert_eq!(normalize("D'Artagnan"), "dartagnan");
        assert_eq!(normalize("c01000010"), "c01000010");
    }

    #[test]
    fn scores() {
        let s = |q: &str, k: &str| {
            let q = normalize(q);
            let w: Vec<&str> = q.split(' ').collect();
            score(&q, &w, &normalize(k), true)
        };
        assert_eq!(s("mark evans", "Mark Evans"), Some(1000));
        assert!(s("mark", "Mark Evans").unwrap() > s("evans", "Mark Evans").unwrap());
        assert!(s("evans", "Mark Evans").unwrap() >= 700);
        assert!(s("evans mark", "Mark Evans").unwrap() >= 600);
        assert!(s("mrak evans", "Mark Evans").is_some()); // transposition
        assert!(s("tornado fuego", "Tornado de fuego").is_some());
        assert!(s("zzzz", "Mark Evans").is_none());
    }
}
