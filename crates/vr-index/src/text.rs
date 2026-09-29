//! Text tables (`data/common/text/<lang>/*.cfg.bin`, docs/game/data/text-localization.md) and markup helpers.

use std::collections::BTreeMap;

use l5_core::CfgBin;
use l5_core::t2b::Value;

/// Strings of one text file of one language: id -> (form 0 / first variant) string, plus the NOUN_INFO name forms
/// 11 (surname) and 12 (given name) when present.
#[derive(Debug, Default, Clone)]
pub struct ParsedText {
    pub main: BTreeMap<u32, String>,
    pub forms: BTreeMap<u32, Vec<String>>,
}

/// Parse a TEXT_INFO / NOUN_INFO file.
pub fn parse_text(bytes: &[u8]) -> ParsedText {
    let mut out = ParsedText::default();
    let Ok(CfgBin::T2b(t)) = CfgBin::parse(bytes) else { return out };
    for e in &t.entries {
        let name = e.display_name();
        let v = &e.values;
        if name == "TEXT_INFO" && v.len() >= 3 {
            // Multi-line / random talk texts have variants 1..n: keep the first one met.
            if let (Some(id), Value::String(Some(s))) = (v[0].as_int(), &v[2]) {
                out.main.entry(id as u32).or_insert_with(|| s.clone());
            }
        } else if name == "NOUN_INFO" && v.len() >= 6 {
            if let (Some(id), Some(form)) = (v[0].as_int(), v[1].as_int()) {
                let id = id as u32;
                let s = v[2..v.len().min(10)].iter().find_map(|x| x.as_str().filter(|s| !s.is_empty()).map(str::to_string));
                if let Some(s) = s {
                    if form == 0 {
                        out.main.insert(id, s);
                    } else {
                        out.main.entry(id).or_insert_with(|| s.clone());
                        let f = out.forms.entry(id).or_default();
                        if !f.contains(&s) {
                            f.push(s);
                        }
                    }
                }
            }
        }
    }
    out
}

/// Display form of a game string: furigana `[漢字/かな]` -> `漢字`, `\n` -> space, name tags / glyph tags kept.
pub fn clean(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(i) = rest.find('[') {
        out.push_str(&rest[..i]);
        let after = &rest[i + 1..];
        match (after.find('/'), after.find(']')) {
            (Some(sl), Some(end)) if sl < end => {
                out.push_str(&after[..sl]);
                rest = &after[end + 1..];
            }
            _ => {
                out.push('[');
                rest = after;
            }
        }
    }
    out.push_str(rest);
    out.replace("\\n", " ")
}

/// Reading of a furigana string (`[円堂/えんどう] [守/まもる]` -> `えんどう まもる`), `None` without furigana.
pub fn reading(s: &str) -> Option<String> {
    if !s.contains('[') {
        return None;
    }
    let mut out = String::new();
    let mut rest = s;
    let mut any = false;
    while let Some(i) = rest.find('[') {
        out.push_str(&rest[..i]);
        let after = &rest[i + 1..];
        match (after.find('/'), after.find(']')) {
            (Some(sl), Some(end)) if sl < end => {
                out.push_str(&after[sl + 1..end]);
                rest = &after[end + 1..];
                any = true;
            }
            _ => {
                out.push('[');
                rest = after;
            }
        }
    }
    out.push_str(rest);
    any.then_some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn furigana() {
        assert_eq!(clean("[円堂/えんどう] [守/まもる]"), "円堂 守");
        assert_eq!(reading("[円堂/えんどう] [守/まもる]").as_deref(), Some("えんどう まもる"));
        assert_eq!(clean("Linea 1\\nLinea 2"), "Linea 1 Linea 2");
        assert_eq!(clean("a [b] c"), "a [b] c");
        assert_eq!(reading("Mark"), None);
    }
}
