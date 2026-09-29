//! Thumbnails: game icons (G4TX) decoded to small PNGs in `<index>/thumbs/`, referenced by id.
//!
//! * characters: `dx11/menu/200_icon/10_icon_chr/{face,aura_fs,aura_armed,aura_mixi,aura_soul,coach}/<name>_l.g4tx`
//! * keshin / armour / mixi max / aura: the icon of their aura character (armour: `<owner>_5000`)
//! * variants and voice banks: their character's icon; teams: their emblem's icon
//! * emblems: `dx11/menu/200_icon/01_icon_emblem/em<icon_index:04>.g4tx`

use std::collections::{BTreeMap, HashMap};
use std::path::Path;

use g4_texture::G4tx;
use rayon::prelude::*;

use crate::error::{Error, Result};
use crate::model::{Category, Val};
use crate::source::GameSource;
use crate::Index;

const CHR: &str = "data/dx11/menu/200_icon/10_icon_chr/";
const EMBLEM: &str = "data/dx11/menu/200_icon/01_icon_emblem/";
const ITEM: &str = "data/dx11/menu/200_icon/02_icon_item/";
const CHR_DIRS: [&str; 6] = ["face", "aura_fs", "aura_armed", "aura_mixi", "aura_soul", "coach"];

/// Decode texture 0 of a G4TX to a PNG whose longest side is at most `size` (a mip level when there is one).
pub fn g4tx_to_png(bytes: &[u8], size: u32) -> Result<Vec<u8>> {
    let tx = G4tx::parse(bytes).map_err(|e| Error::Format(format!("g4tx: {e}")))?;
    let t = tx.textures.first().ok_or_else(|| Error::Format("g4tx sin texturas".into()))?;
    texture_png(t, size)
}

/// One texture of a G4TX as a PNG whose longest side is at most `size`.
pub fn texture_png(t: &g4_texture::Texture, size: u32) -> Result<Vec<u8>> {
    let info = t.dds_info().map_err(|e| Error::Format(format!("dds: {e}")))?;
    let mut level = 0;
    while level + 1 < info.mip_count && (info.width.max(info.height) >> level) > size {
        level += 1;
    }
    let mut img = t.decode(level).map_err(|e| Error::Format(format!("dds: {e}")))?;
    while img.width.max(img.height) > size && img.width > 1 && img.height > 1 {
        img = img.downsample();
    }
    img.to_png_bytes().map_err(|e| Error::Format(format!("png: {e}")))
}

/// Extract every thumbnail the index can use into `<out>/thumbs/` (existing PNGs are kept) and set `thumb` on the
/// entities. Returns the number of PNG files referenced.
pub fn extract(src: &GameSource, index: &mut Index, out: &Path, size: u32, progress: crate::build::Progress<'_>, warnings: &mut Vec<String>) -> Result<usize> {
    // stem -> source path, for every character icon folder (face wins over the others)
    let mut chr: HashMap<String, String> = HashMap::new();
    for d in CHR_DIRS.iter().rev() {
        for it in src.list_retail(&format!("{CHR}{d}/")) {
            let name = it.path.rsplit('/').next().unwrap_or("");
            if let Some(stem) = name.strip_suffix("_l.g4tx") {
                chr.insert(stem.to_string(), it.path.clone());
            }
        }
    }
    // job: source path -> relative output path
    let mut jobs: BTreeMap<String, String> = BTreeMap::new();
    let mut thumb_of: HashMap<(Category, String), String> = HashMap::new();
    let ents = index.entities();
    let chara_thumb = |id: &str, jobs: &mut BTreeMap<String, String>| -> Option<String> {
        let p = chr.get(id)?;
        let rel = format!("thumbs/chara/{id}.png");
        jobs.entry(p.clone()).or_insert_with(|| rel.clone());
        Some(jobs[p].clone())
    };
    for e in ents {
        let t = match e.category {
            Category::Character => chara_thumb(&e.id, &mut jobs),
            Category::Keshin | Category::Armour | Category::Miximax | Category::Aura => {
                let armed = if e.category == Category::Armour { e.links("owner").find_map(|o| chara_thumb(&format!("{}_5000", o.id), &mut jobs)) } else { None };
                armed.or_else(|| e.links("aura_chara").find_map(|a| chara_thumb(&a.id, &mut jobs)))
            }
            Category::Emblem => {
                // `<key>.g4tx` (em010003), older ones `em<icon_index:04>.g4tx`
                let n = e.field("icon_index").and_then(Val::as_i64).unwrap_or(-1);
                [e.id.clone(), format!("em{n:04}")].into_iter().find_map(|stem| {
                    let it = src.retail_item(&format!("{EMBLEM}{stem}.g4tx"))?;
                    let rel = format!("thumbs/emblem/{stem}.png");
                    jobs.entry(it.path.clone()).or_insert_with(|| rel.clone());
                    Some(jobs[&it.path].clone())
                })
            }
            _ => None,
        };
        if let Some(t) = t {
            thumb_of.insert((e.category, e.id.clone()), t);
        }
    }
    // item icons: textures named after the item key inside the icon sheets (02_icon_item/icon_item01..)
    let item_ids: std::collections::HashSet<&str> = ents.iter().filter(|e| e.category == Category::Item).map(|e| e.id.as_str()).collect();
    let sheets: Vec<String> = src.list_retail(ITEM).iter().filter(|it| it.path.ends_with(".g4tx")).map(|it| it.path.clone()).collect();
    let item_dir = out.join("thumbs/item");
    std::fs::create_dir_all(&item_dir).map_err(|e| Error::io(&item_dir, e))?;
    // The sheets are big (10 x ~25 MB): when they were already cut for this game state and size, reuse the PNGs.
    let marker = item_dir.join(".sheets");
    let stamp = format!("{} {size}", src.fingerprint());
    let reuse = std::fs::read_to_string(&marker).is_ok_and(|s| s == stamp);
    let sheet_files = if reuse { Default::default() } else { src.read_many(&sheets) };
    if reuse {
        if let Ok(rd) = std::fs::read_dir(&item_dir) {
            for e in rd.flatten() {
                let n = e.file_name().to_string_lossy().into_owned();
                if let Some(stem) = n.strip_suffix(".png") {
                    if item_ids.contains(stem) {
                        thumb_of.insert((Category::Item, stem.to_string()), format!("thumbs/item/{n}"));
                    }
                }
            }
        }
    }
    let item_done: Vec<Vec<String>> = sheet_files
        .par_iter()
        .map(|(_, r)| {
            let Ok(b) = r else { return Vec::new() };
            let Ok(tx) = G4tx::parse(b) else { return Vec::new() };
            let mut ok = Vec::new();
            for t in &tx.textures {
                if !item_ids.contains(t.name.as_str()) {
                    continue;
                }
                let rel = format!("thumbs/item/{}.png", t.name);
                if out.join(&rel).is_file() || texture_png(t, size).ok().is_some_and(|png| std::fs::write(out.join(&rel), png).is_ok()) {
                    ok.push(t.name.clone());
                }
            }
            ok
        })
        .collect();
    drop(sheet_files);
    if !reuse {
        let _ = std::fs::write(&marker, &stamp);
    }
    for name in item_done.into_iter().flatten() {
        thumb_of.insert((Category::Item, name.clone()), format!("thumbs/item/{name}.png"));
    }

    // second pass: things that borrow another entity's picture
    let mut assign: Vec<(usize, String)> = Vec::new();
    for (i, e) in ents.iter().enumerate() {
        let t = match e.category {
            Category::Variant | Category::VoiceBank => e.links("chara").find_map(|l| thumb_of.get(&(Category::Character, l.id.clone())).cloned()),
            Category::Team => e.links("emblem").find_map(|l| thumb_of.get(&(Category::Emblem, l.id.clone())).cloned()),
            c => thumb_of.get(&(c, e.id.clone())).cloned(),
        };
        if let Some(t) = t {
            assign.push((i, t));
        }
    }

    // extract (chunks keep memory bounded: a face icon is ~130 KB)
    let todo: Vec<(String, String)> = jobs.iter().filter(|(_, rel)| !out.join(rel).is_file()).map(|(a, b)| (a.clone(), b.clone())).collect();
    for d in ["thumbs/chara", "thumbs/emblem"] {
        std::fs::create_dir_all(out.join(d)).map_err(|e| Error::io(&out.join(d), e))?;
    }
    let total = todo.len();
    let mut done = 0;
    let mut failed = 0;
    for chunk in todo.chunks(384) {
        let paths: Vec<String> = chunk.iter().map(|c| c.0.clone()).collect();
        let files = src.read_many(&paths);
        let res: Vec<bool> = chunk
            .par_iter()
            .map(|(p, rel)| {
                let Some(Ok(b)) = files.get(p) else { return false };
                match g4tx_to_png(b, size) {
                    Ok(png) => std::fs::write(out.join(rel), png).is_ok(),
                    Err(_) => false,
                }
            })
            .collect();
        failed += res.iter().filter(|x| !**x).count();
        done += chunk.len();
        progress("miniaturas", &format!("{done}/{total}"));
    }
    if failed > 0 {
        warnings.push(format!("{failed} miniaturas no se pudieron extraer"));
    }
    let mut ok: std::collections::HashSet<String> = jobs.values().filter(|rel| out.join(rel).is_file()).cloned().collect();
    ok.extend(thumb_of.iter().filter(|(k, _)| k.0 == Category::Item).map(|(_, v)| v.clone()));
    let n = ok.len();
    let assign: Vec<(usize, String)> = assign.into_iter().filter(|(_, t)| ok.contains(t)).collect();
    for (i, t) in assign {
        index.set_thumb(i, t);
    }
    Ok(n)
}
