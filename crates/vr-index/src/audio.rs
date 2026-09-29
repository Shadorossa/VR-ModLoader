//! Audio part of the index: BGM ids and cues, SE cues of the language-independent banks, voice banks
//! (merges what `research/scripts/audio_index_build.py` knew; docs/game/media/audio-engine.md §2).

use std::collections::{BTreeMap, HashMap, HashSet};

use l5_core::rdbn::Rdbn;
use l5_core::t2b::T2b;

use crate::model::{Category, Entity, Val};
use crate::source::GameSource;

const BGM_BANKS: [&str; 3] = ["bgm", "bgm_title", "bgm_chronicle"];

fn crc(s: &str) -> u32 {
    crc32fast::hash(s.as_bytes())
}

/// Cue names + lengths of an ACB (plain inside a CPK; XOR-encrypted when loose).
fn cues(bytes: &[u8], file_name: &str) -> Option<Vec<(String, u32)>> {
    let plain;
    let data = if bytes.starts_with(b"@UTF") {
        bytes
    } else {
        plain = cri::crypt::xor(bytes, cri::crypt::loose_key(file_name), 0);
        &plain
    };
    let acb = cri::acb::Acb::parse(data).ok()?;
    Some(
        acb.cues
            .iter()
            .map(|c| {
                let len = if c.length_ms != u32::MAX && c.length_ms != 0 {
                    c.length_ms
                } else {
                    c.waveforms.first().and_then(|&w| acb.waveforms.get(w)).map_or(0, |w| w.duration_ms())
                };
                (c.name.clone(), len)
            })
            .collect(),
    )
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn index_audio(
    src: &GameSource,
    root_banks: &[String],
    voice_banks: &BTreeMap<String, Vec<&str>>,
    bgm_cfg: Option<&Rdbn>,
    map_sound: Option<&T2b>,
    maps: &HashMap<u32, String>,
    charas: &HashSet<String>,
    warnings: &mut Vec<String>,
) -> Vec<Entity> {
    let mut out = Vec::new();
    // Every root bank header (they are small; the .awb stream archives are never read).
    let paths: Vec<String> = root_banks.iter().map(|b| format!("data/common/sound_asset/{b}.acb")).collect();
    let files = src.read_many(&paths);
    let mut bank_cues: BTreeMap<String, Vec<(String, u32)>> = BTreeMap::new();
    for b in root_banks {
        let p = format!("data/common/sound_asset/{b}.acb");
        match files.get(&p) {
            Some(Ok(bytes)) => match cues(bytes, &format!("{b}.acb")) {
                Some(c) => {
                    bank_cues.insert(b.clone(), c);
                }
                None => warnings.push(format!("{p}: ACB ilegible")),
            },
            Some(Err(e)) => warnings.push(format!("{p}: {e}")),
            None => {}
        }
    }

    // --- BGM: bgm_config ids -> cue; plus every cue of the BGM banks
    let mut bgm_names: HashMap<u32, (String, &str, u32)> = HashMap::new();
    for b in BGM_BANKS {
        for (c, len) in bank_cues.get(b).map(Vec::as_slice).unwrap_or_default() {
            bgm_names.entry(crc(c)).or_insert((c.clone(), b, *len));
        }
    }
    // map BGM (map_sound MAP_BGM_CONFIG = map hash, then MAP_BGM_CONFIG_PARAM rows = bgm ids)
    let mut bgm_maps: HashMap<u32, Vec<String>> = HashMap::new();
    if let Some(t) = map_sound {
        let mut cur: Option<u32> = None;
        for e in &t.entries {
            match e.name.as_deref() {
                Some("MAP_BGM_CONFIG") => cur = e.values.first().and_then(|v| v.as_int()).map(|v| v as u32),
                Some("MAP_BGM_CONFIG_PARAM") => {
                    if let (Some(m), Some(b)) = (cur, e.values.first().and_then(|v| v.as_int())) {
                        if b != 0 {
                            let mid = maps.get(&m).cloned().unwrap_or_else(|| format!("0x{m:08X}"));
                            let v = bgm_maps.entry(b as u32).or_default();
                            if !v.contains(&mid) {
                                v.push(mid);
                            }
                        }
                    }
                }
                _ => {}
            }
        }
    }
    let mut seen: HashSet<String> = HashSet::new();
    let mut push_bgm = |out: &mut Vec<Entity>, id: String, hash: u32, cue: Option<&(String, &str, u32)>, category: Option<i64>| {
        if !seen.insert(id.clone()) {
            return;
        }
        let mut e = Entity::new(Category::Bgm, id, hash);
        if let Some((c, bank, len)) = cue {
            e.set("cue", c.as_str());
            e.set("bank", *bank);
            if *len > 0 {
                e.set("duration_ms", *len as i64);
            }
        }
        if let Some(c) = category {
            e.set("bgm_category", c);
        }
        if let Some(m) = bgm_maps.get(&hash) {
            e.set("maps", Val::L(m.iter().map(|s| Val::S(s.clone())).collect()));
            for id in m {
                if !id.starts_with("0x") {
                    e.link("map", Category::Map, id.clone());
                }
            }
        }
        out.push(e);
    };
    if let Some(r) = bgm_cfg {
        if let Some(l) = r.lists.iter().find(|l| l.name == "m_bgmInfoList") {
            let ty = &r.types[l.type_index];
            let f = |n: &str| ty.fields.iter().position(|x| x.name == n);
            let (fi, fc, fk) = (f("bgm_id"), f("bgm"), f("category"));
            for row in &l.rows {
                let get = |i: Option<usize>| i.and_then(|i| row.get(i)).and_then(|v| v.first()).map(crate::build::r_u32).unwrap_or(0);
                let (bid, cue) = (get(fi), get(fc));
                let id = bgm_names.get(&bid).map_or_else(|| format!("0x{bid:08X}"), |x| x.0.clone());
                push_bgm(&mut out, id, bid, bgm_names.get(&cue), Some(get(fk) as i32 as i64));
            }
        }
    } else {
        warnings.push("bgm_config no encontrado".into());
    }
    let mut rest: Vec<(&u32, &(String, &str, u32))> = bgm_names.iter().collect();
    rest.sort_by(|a, b| a.1.0.cmp(&b.1.0));
    for (h, x) in rest {
        push_bgm(&mut out, x.0.clone(), *h, Some(x), None);
    }

    // --- SE: every cue of the other root banks (first bank wins)
    let mut se_seen: HashSet<String> = HashSet::new();
    for (bank, cs) in &bank_cues {
        if BGM_BANKS.contains(&bank.as_str()) {
            continue;
        }
        for (c, len) in cs {
            if !se_seen.insert(c.clone()) {
                continue;
            }
            let mut e = Entity::new(Category::Se, c.clone(), crc(c));
            e.set("bank", bank.as_str());
            if *len > 0 {
                e.set("duration_ms", *len as i64);
            }
            out.push(e);
        }
    }

    // --- voice banks (names come from the list; the banks themselves are not read)
    for (bank, langs) in voice_banks {
        let mut e = Entity::new(Category::VoiceBank, bank.clone(), crc(bank));
        e.set("langs", Val::L(langs.iter().map(|l| Val::S(l.to_string())).collect()));
        if charas.contains(bank) {
            e.link("chara", Category::Character, bank.clone());
        }
        out.push(e);
    }
    out
}
