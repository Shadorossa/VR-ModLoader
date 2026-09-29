//! Cue conflicts between the `audio.toml` declarations of the active mods (the audio engine's format,
//! docs/game/media/audio-engine.md §3), computed offline: two mods that replace the same SE cue / music id, give
//! the same voice line, add the same new SE / track name or register the same bank. The declarations are compared
//! by their literal text (a character name and its bank id are not matched: that needs the game index, which the
//! Studio builds). Read tolerantly: an unreadable file is simply skipped (the audio engine reports it in the log).

use std::collections::BTreeMap;

use evt_modfmt::LoadPlan;
use toml::Value;

use crate::model::ConflictRow;

pub const FILE: &str = "audio.toml";

fn list(v: Option<&Value>) -> Vec<String> {
    match v {
        Some(Value::String(s)) => vec![s.trim().to_string()],
        Some(Value::Array(a)) => a.iter().filter_map(|x| x.as_str()).map(|s| s.trim().to_string()).collect(),
        _ => Vec::new(),
    }
}

fn s(t: &toml::Table, k: &str) -> Option<String> {
    t.get(k).and_then(Value::as_str).map(|x| x.trim().to_string()).filter(|x| !x.is_empty())
}

/// `(kind, key)` of every declaration of one `audio.toml` text.
pub fn keys(text: &str) -> Vec<(String, String)> {
    let Ok(doc) = text.parse::<toml::Table>() else { return Vec::new() };
    let mut out = Vec::new();
    let arr = |k: &str| doc.get(k).and_then(Value::as_array).cloned().unwrap_or_default();
    for b in arr("bank") {
        if let Some(n) = b.as_table().and_then(|t| s(t, "name")) {
            out.push(("bank".into(), n.to_lowercase()));
        }
    }
    for x in arr("sfx") {
        let Some(t) = x.as_table() else { continue };
        if let Some(r) = s(t, "replace") {
            out.push(("sfx".into(), r.to_lowercase()));
        } else if let Some(tech) = s(t, "technique") {
            out.push(("sfx".into(), format!("technique:{tech}")));
        }
        if let Some(a) = s(t, "add") {
            out.push(("new sfx".into(), a));
        }
    }
    for x in arr("music") {
        let Some(t) = x.as_table() else { continue };
        if let Some(r) = s(t, "replace") {
            out.push(("music".into(), r));
        }
        if let Some(a) = s(t, "add") {
            out.push(("new music".into(), a));
        }
        for ctx in list(t.get("play_in")) {
            out.push(("music".into(), ctx));
        }
    }
    for x in arr("voice") {
        let Some(t) = x.as_table() else { continue };
        let who: Vec<String> = if t.contains_key("bank") { list(t.get("bank")) } else { list(t.get("character")) };
        let line = s(t, "cue").or_else(|| s(t, "technique").map(|v| format!("technique:{v}"))).or_else(|| s(t, "armour").map(|v| format!("armour:{v}")));
        if let Some(line) = line {
            for w in who {
                out.push(("voice".into(), format!("{w}/{line}")));
            }
        }
    }
    out.sort();
    out.dedup();
    out
}

/// Conflicts among the active mods of `plan` (load order kept).
pub fn conflicts(plan: &LoadPlan) -> Vec<ConflictRow> {
    let mut owners: BTreeMap<(String, String), Vec<String>> = BTreeMap::new();
    for m in &plan.mods {
        let Ok(text) = std::fs::read_to_string(m.dir.join(FILE)) else { continue };
        for k in keys(&text) {
            owners.entry(k).or_default().push(m.manifest.id.clone());
        }
    }
    owners
        .into_iter()
        .filter(|(_, v)| v.len() > 1)
        .map(|((kind, what), mods)| ConflictRow { kind: format!("audio {kind}"), what, mods })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn declaration_keys() {
        let t = r#"
[[bank]]
name = "evt_fwa_se"
[[sfx]]
replace = "EV60_03380_ME"
file = "a.hca"
[[sfx]]
add = "mi_se_ok"
file = "b.wav"
[[music]]
replace = "bg00010"
file = "t.ogg"
[[music]]
add = "mi_tema"
play_in = ["map:mr01b01"]
file = "x.flac"
[[voice]]
bank = ["c01000010", "c05024610"]
cue = "gl010"
file = "g.wav"
[[voice]]
character = "Mark Evans"
technique = "Mano celestial"
file = "m.wav"
"#;
        let k = keys(t);
        for want in [
            ("bank", "evt_fwa_se"),
            ("sfx", "ev60_03380_me"),
            ("new sfx", "mi_se_ok"),
            ("music", "bg00010"),
            ("music", "map:mr01b01"),
            ("new music", "mi_tema"),
            ("voice", "c01000010/gl010"),
            ("voice", "c05024610/gl010"),
            ("voice", "Mark Evans/technique:Mano celestial"),
        ] {
            assert!(k.contains(&(want.0.to_string(), want.1.to_string())), "{want:?} in {k:?}");
        }
        assert!(keys("not toml [[").is_empty());
    }

    #[test]
    fn conflicts_between_active_mods() {
        let d = crate::temp_dir("audio_conf").unwrap();
        let root = d.join("mods");
        for (id, audio) in [
            ("a", "[[sfx]]\nreplace = \"ev60_03380_me\"\nfile = \"x.hca\"\n"),
            ("b", "[[sfx]]\nreplace = \"ev60_03380_me\"\nfile = \"y.hca\"\n[[music]]\nreplace = \"bg1\"\nfile = \"m.ogg\"\n"),
            ("c", "[[music]]\nreplace = \"bg2\"\nfile = \"m.ogg\"\n"),
        ] {
            std::fs::create_dir_all(root.join(id)).unwrap();
            std::fs::write(root.join(id).join("mod.toml"), format!("id = \"{id}\"\nname = \"{id}\"\nversion = \"1.0\"\n")).unwrap();
            std::fs::write(root.join(id).join(FILE), audio).unwrap();
        }
        std::fs::write(root.join("load_order.toml"), "order = [\"a\", \"b\", \"c\"]\n").unwrap();
        let plan = evt_modfmt::plan_root(&root);
        let c = conflicts(&plan);
        assert_eq!(c.len(), 1);
        assert_eq!(c[0].what, "ev60_03380_me");
        assert_eq!(c[0].mods, vec!["b", "a"], "load order: a is on top, so it loads last");
        let _ = std::fs::remove_dir_all(&d);
    }
}
