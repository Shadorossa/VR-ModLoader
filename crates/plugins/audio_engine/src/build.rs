//! The audio build (docs/game/media/audio-engine.md §3, §5): every active mod's `audio.toml` → per-bank cue edits,
//! new banks, `bgm_config` redirects / rows and bank registrations; then the banks are built (retail bank from the
//! player's game + the mods' plain audio or ready `.hca`), cached by an input hash, and handed back as
//! `(game path, file)` pairs to serve.
//!
//! Merge rules (per cue, across mods): different cues of one bank from different mods all go in; the same cue from
//! two mods → the one that loads later wins (WARN). A mod that ships its own built bank (`files\…`, e.g. built at
//! pack time with `audio_build` / audio_mod_build.py) is not rebuilt for its own entries, but its bank is the BASE the
//! other mods' cues of that bank are applied on (per-cue merge on top of a prebuilt bank).

use crate::decl::{AudioToml, MusicDecl, SfxDecl, VoiceDecl};
use crate::framework::{file_stamp, hash_inputs, merge_by_key, overlay_winner, Cache, GameFiles, ModDir};
use crate::index::Index;
use crate::sheet::{BankReq, Group};
use crate::source::{self, Normalize, Target, Treat};
use cri::bank::{Bank, NewCue};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// Bumped when the output of the same inputs changes (invalidates every cache entry).
pub const BUILDER_VERSION: u32 = 1;
pub const SOUND: &str = "data/common/sound_asset";
pub const BGM_CONFIG: &str = "data/common/sound/bgm_config_0.00.00.cfg.bin";
/// Retail 1-cue bank every new bank is cloned from (bgm_add_build.py route B).
pub const NEW_BANK_TEMPLATE: &str = "bgm_title";
/// Default SE category of new SE cues (fwa_sfx_build.py).
pub const DEFAULT_SE_TEMPLATE: &str = "sy0006";

/// A log note: (warning, text).
pub type Note = (bool, String);

/// One cue of a retail (or prebuilt) bank to change.
#[derive(Debug, Clone, PartialEq)]
pub struct CueEdit {
    pub file: PathBuf,
    pub treat: Treat,
}

/// A bank to rebuild.
#[derive(Debug, Clone, PartialEq)]
pub struct BankJob {
    /// Game path without extension: `data/common/sound_asset/[ja|en/]<name>`.
    pub key: String,
    pub name: String,
    /// Full cue name → (mod, edit).
    pub edits: BTreeMap<String, (String, CueEdit)>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum NewKind {
    Music,
    /// SE copying the category of this retail cue.
    Se(String),
}

#[derive(Debug, Clone, PartialEq)]
pub struct NewCueJob {
    pub name: String,
    pub edit: CueEdit,
    pub kind: NewKind,
}

/// A brand-new bank of one mod.
#[derive(Debug, Clone, PartialEq)]
pub struct NewBankJob {
    pub mod_id: String,
    pub name: String,
    pub cues: Vec<NewCueJob>,
}

/// What the mods ask for.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Plan {
    pub jobs: BTreeMap<String, BankJob>,
    pub new_banks: Vec<NewBankJob>,
    /// bgm_config id crc → (mod, crc of the cue it plays now, label).
    pub bgm_redirect: BTreeMap<u32, (String, u32, String)>,
    /// New bgm_config rows (id = cue crc).
    pub bgm_rows: Vec<(String, u32, String)>,
    /// Banks to register in sound_queue_sheet (the new banks).
    pub regs: Vec<BankReq>,
    pub notes: Vec<Note>,
}

fn safe_id(id: &str) -> String {
    id.chars().map(|c| if c.is_ascii_alphanumeric() { c.to_ascii_lowercase() } else { '_' }).collect()
}

fn valid_cue(s: &str) -> bool {
    !s.is_empty() && s.len() < 60 && s.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
}

/// The source file of an entry, relative to its mod.
fn src_path(m: &ModDir, file: &str) -> Result<PathBuf, String> {
    if file.is_empty() {
        return Err("file = \"audio/…\" missing".into());
    }
    let p = m.dir.join(file);
    if !p.is_file() {
        return Err(format!("{file} not found in the mod"));
    }
    Ok(p)
}

fn treat(src: &crate::decl::Src, default_norm: Normalize, loop_secs: Option<(f64, f64)>) -> Result<Treat, String> {
    let normalize = match &src.normalize {
        Some(s) => Normalize::parse(s)?,
        None => default_norm,
    };
    Ok(Treat { gain_db: src.volume.unwrap_or(0.0), normalize, loop_secs })
}

/// A `.hca` goes in as it is: its own defaults (no normalize) unless the entry asks for one (then an error later).
fn treat_for(path: &Path, src: &crate::decl::Src, default_norm: Normalize, loop_secs: Option<(f64, f64)>) -> Result<Treat, String> {
    let is_hca = path.extension().is_some_and(|e| e.eq_ignore_ascii_case("hca"));
    treat(src, if is_hca { Normalize::None } else { default_norm }, if is_hca { None } else { loop_secs })
}

/// Does mod `m` ship the built bank `key` itself (its own `files\`)?
fn prebuilt(m: &ModDir, key: &str) -> bool {
    let rel: PathBuf = format!("{key}.acb").split('/').collect();
    m.dir.join("files").join(rel).is_file()
}

struct Items {
    cue_edits: Vec<(String, String, (String, String, CueEdit))>, // (mod, "key|cue", (key, name, edit))
}

fn voice_items(m: &ModDir, v: &VoiceDecl, idx: &Index, it: &mut Items, notes: &mut Vec<Note>) -> Result<(), String> {
    let file = src_path(m, &v.src.file)?;
    let (banks, charas) = match (&v.bank, &v.character) {
        (Some(b), _) => (b.list(), Vec::new()),
        (None, Some(c)) => idx.character_banks(c)?,
        _ => return Err("character = \"…\" or bank = \"c########\" missing".into()),
    };
    let kinds = [v.cue.is_some(), v.technique.is_some(), v.armour.is_some()];
    if kinds.iter().filter(|k| **k).count() != 1 {
        return Err("exactly one of cue / technique / armour".into());
    }
    let langs: Vec<&str> = match v.lang.as_deref().unwrap_or("both") {
        "both" => vec!["ja", "en"],
        "ja" => vec!["ja"],
        "en" => vec!["en"],
        other => return Err(format!("lang = \"{other}\": ja | en | both")),
    };
    let tr = treat_for(&file, &v.src, Normalize::Peak, None)?;
    for b in banks {
        let sufs = if let Some(c) = &v.cue {
            c.list()
        } else if let Some(t) = &v.technique {
            idx.technique_keys(t)?
        } else {
            let who = if charas.is_empty() { idx.characters_of_bank(&b) } else { idx.characters_of_bank(&b) };
            idx.armour_keys(v.armour.as_deref().unwrap_or(""), &who)?
        };
        let got: Vec<&str> = langs.iter().copied().filter(|l| idx.has_bank(l, &b)).collect();
        if got.is_empty() {
            notes.push((true, format!("mod {}: [[voice]] bank {b}: the game has no {} copy (skipped)", m.id, langs.join("/"))));
            continue;
        }
        for l in got {
            let key = format!("{SOUND}/{l}/{b}");
            for s in &sufs {
                let cue = if s.starts_with(&format!("{b}_")) { s.clone() } else { format!("{b}_{s}") };
                if !valid_cue(&cue) {
                    return Err(format!("cue {cue:?}: A-Z a-z 0-9 _ only"));
                }
                it.cue_edits.push((m.id.clone(), format!("{key}|{cue}"), (key.clone(), b.clone(), CueEdit { file: file.clone(), treat: tr })));
            }
        }
    }
    let _ = charas;
    Ok(())
}

fn sfx_items(m: &ModDir, s: &SfxDecl, idx: &Index, it: &mut Items, new_se: &mut Vec<NewCueJob>) -> Result<(), String> {
    let file = src_path(m, &s.src.file)?;
    let tr = treat_for(&file, &s.src, Normalize::None, None)?;
    if let Some(name) = &s.add {
        if s.replace.is_some() || s.technique.is_some() {
            return Err("add = … cannot go with replace / technique".into());
        }
        if !valid_cue(name) {
            return Err(format!("add = {name:?}: A-Z a-z 0-9 _ only (under 60)"));
        }
        let tpl = s.category_template.clone().unwrap_or_else(|| DEFAULT_SE_TEMPLATE.into());
        new_se.push(NewCueJob { name: name.clone(), edit: CueEdit { file, treat: tr }, kind: NewKind::Se(tpl) });
        return Ok(());
    }
    let cue = match (&s.replace, &s.technique) {
        (Some(c), None) => c.clone(),
        (None, Some(t)) => idx.technique_se(t)?,
        _ => return Err("replace = \"<cue>\", technique = \"…\" or add = \"<new cue>\"".into()),
    };
    let bank = match &s.bank {
        Some(b) => b.clone(),
        None => idx.se_bank(&cue).map(str::to_string).ok_or_else(|| format!("cue {cue}: its bank is not in the index (give bank = \"…\")"))?,
    };
    let key = format!("{SOUND}/{bank}");
    it.cue_edits.push((m.id.clone(), format!("{key}|{cue}"), (key, bank, CueEdit { file, treat: tr })));
    Ok(())
}

fn music_items(m: &ModDir, mu: &MusicDecl, idx: &Index, plan_music: &mut Vec<NewCueJob>, redirects: &mut Vec<(String, String, (u32, String))>, rows: &mut Vec<(String, u32, String)>, n: usize) -> Result<(), String> {
    let file = src_path(m, &mu.src.file)?;
    let lp = mu.loop_secs()?;
    let tr = treat_for(&file, &mu.src, Normalize::None, lp)?;
    let mut targets: Vec<(String, u32)> = Vec::new();
    for t in mu.replace.as_ref().map(|r| r.list()).unwrap_or_default().iter().chain(mu.play_in.as_ref().map(|r| r.list()).unwrap_or_default().iter()) {
        targets.extend(idx.bgm_targets(t)?);
    }
    let name = match (&mu.add, targets.first()) {
        (Some(a), _) => a.clone(),
        (None, Some((label, _))) => format!("{}_{}", safe_id(&m.id), label.trim_start_matches("0x")),
        (None, None) => return Err("replace = \"bg#####\" / context, or add = \"<new track>\"".into()),
    };
    let name = if valid_cue(&name) { name } else { format!("{}_m{n}", safe_id(&m.id)) };
    if !valid_cue(&name) {
        return Err(format!("track name {name:?}: A-Z a-z 0-9 _ only"));
    }
    let crc = crc32fast::hash(name.as_bytes());
    if mu.add.is_some() {
        rows.push((m.id.clone(), crc, name.clone()));
    }
    for (label, id) in targets {
        redirects.push((m.id.clone(), format!("{id}"), (crc, format!("{label} -> {name}"))));
    }
    plan_music.push(NewCueJob { name, edit: CueEdit { file, treat: tr }, kind: NewKind::Music });
    Ok(())
}

/// Resolve every mod's `audio.toml` (load order) into a [`Plan`] (boot: a mod's own built banks are not rebuilt).
pub fn plan(mods: &[(ModDir, AudioToml)], idx: &Index) -> Plan {
    plan_with(mods, idx, true)
}

/// [`plan`]; `honor_prebuilt = false` (the pack-time tool) rebuilds a mod's banks even when its `files\` has them.
pub fn plan_with(mods: &[(ModDir, AudioToml)], idx: &Index, honor_prebuilt: bool) -> Plan {
    let prebuilt = |m: &ModDir, key: &str| honor_prebuilt && prebuilt(m, key);
    let mut p = Plan::default();
    let mut it = Items { cue_edits: Vec::new() };
    let mut redirects = Vec::new();
    let mut ordered: Vec<&(ModDir, AudioToml)> = mods.iter().collect();
    ordered.sort_by_key(|(m, _)| m.load_index);
    for (m, t) in ordered {
        let mut new_se = Vec::new();
        let mut music = Vec::new();
        for (i, v) in t.voice.iter().enumerate() {
            if let Err(e) = voice_items(m, v, idx, &mut it, &mut p.notes) {
                p.notes.push((true, format!("mod {}: [[voice]] nº {}: {e} (skipped)", m.id, i + 1)));
            }
        }
        for (i, s) in t.sfx.iter().enumerate() {
            if let Err(e) = sfx_items(m, s, idx, &mut it, &mut new_se) {
                p.notes.push((true, format!("mod {}: [[sfx]] nº {}: {e} (skipped)", m.id, i + 1)));
            }
        }
        for (i, mu) in t.music.iter().enumerate() {
            if let Err(e) = music_items(m, mu, idx, &mut music, &mut redirects, &mut p.bgm_rows, i + 1) {
                p.notes.push((true, format!("mod {}: [[music]] nº {}: {e} (skipped)", m.id, i + 1)));
            }
        }
        for (suffix, cues) in [("se", new_se), ("bgm", music)] {
            if cues.is_empty() {
                continue;
            }
            let name = format!("evt_{suffix}_{}", safe_id(&m.id));
            let name: String = name.chars().take(60).collect();
            p.regs.push(BankReq { name: name.clone(), group: Group::Global, voice: Some(false), source: m.id.clone() });
            if prebuilt(m, &format!("{SOUND}/{name}")) {
                p.notes.push((false, format!("mod {}: {name} ships built (files\\): not rebuilt", m.id)));
                continue;
            }
            p.new_banks.push(NewBankJob { mod_id: m.id.clone(), name, cues });
        }
    }
    // per-cue merge across mods
    let (merged, conflicts) = merge_by_key(&it.cue_edits);
    for c in conflicts {
        p.notes.push((true, format!("audio conflict: cue {}: {} (winner: {}, loads later)", c.key.replace('|', " "), c.losers.join(", "), c.winner)));
    }
    let owner: BTreeMap<&str, &ModDir> = mods.iter().map(|(m, _)| (m.id.as_str(), m)).collect();
    for (k, (mod_id, (key, name, edit))) in merged {
        if owner.get(mod_id.as_str()).is_some_and(|m| prebuilt(m, &key)) {
            continue; // the mod's own built bank already has it
        }
        let cue = k.split('|').nth(1).unwrap_or_default().to_string();
        p.jobs.entry(key.clone()).or_insert_with(|| BankJob { key: key.clone(), name: name.clone(), edits: BTreeMap::new() }).edits.insert(cue, (mod_id, edit));
    }
    let items: Vec<(String, String, (u32, String))> = redirects;
    let (red, conflicts) = merge_by_key(&items);
    for c in conflicts {
        p.notes.push((true, format!("audio conflict: BGM id 0x{:08X}: {} (winner: {})", c.key.parse::<u32>().unwrap_or(0), c.losers.join(", "), c.winner)));
    }
    for (id, (m, (crc, label))) in red {
        p.bgm_redirect.insert(id.parse().unwrap_or(0), (m, crc, label));
    }
    p
}

// ---------------------------------------------------------------- execution

/// Where outputs go and how they are named.
pub struct Env<'a> {
    pub game: &'a dyn GameFiles,
    /// Every active mod (overlay winners = bases).
    pub mods: &'a [ModDir],
    pub cache: &'a Cache,
}

/// Built outputs: game path → file (to serve), plus notes.
#[derive(Debug, Default)]
pub struct Built {
    pub files: Vec<(String, PathBuf)>,
    pub notes: Vec<Note>,
    pub rebuilt: usize,
    pub cached: usize,
}

fn read_plain(p: &Path, magic: &[u8; 4]) -> Result<(Vec<u8>, bool), String> {
    let b = std::fs::read(p).map_err(|e| format!("{}: {e}", p.display()))?;
    if b.starts_with(magic) {
        return Ok((b, false));
    }
    let key = cri::crypt::loose_key(&p.file_name().unwrap_or_default().to_string_lossy());
    let d = cri::crypt::xor(&b, key, 0);
    if d.starts_with(magic) {
        Ok((d, true))
    } else {
        Err(format!("{}: neither plain nor XOR {}", p.display(), String::from_utf8_lossy(magic)))
    }
}

/// Base file of `key` (+ext): the overlay winner among the mods, else the game's own.
fn base_of(env: &Env, key: &str) -> Option<PathBuf> {
    overlay_winner(env.mods, key).map(|(_, p)| p).or_else(|| env.game.path(key))
}

fn open_awb(p: &Path) -> Result<Box<dyn cri::awb::ReadSeek>, String> {
    use std::io::Read;
    let mut f = std::fs::File::open(p).map_err(|e| format!("{}: {e}", p.display()))?;
    let mut m = [0u8; 4];
    f.read_exact(&mut m).map_err(|e| e.to_string())?;
    let f = std::fs::File::open(p).map_err(|e| e.to_string())?;
    let f = std::io::BufReader::with_capacity(1 << 20, f);
    if &m == b"AFS2" {
        Ok(Box::new(f))
    } else {
        Ok(Box::new(cri::crypt::XorReader::new(f, cri::crypt::loose_key(&p.file_name().unwrap_or_default().to_string_lossy()))))
    }
}

fn awb_header(r: &mut dyn cri::awb::ReadSeek) -> Result<Vec<u8>, String> {
    let mut h = vec![0u8; 16];
    r.seek(std::io::SeekFrom::Start(0)).and_then(|_| r.read_exact(&mut h)).map_err(|e| format!("awb: {e}"))?;
    let n = cri::awb::Afs2::header_len_of(&h).ok_or("not an AFS2 archive")?;
    let mut full = vec![0u8; n];
    r.seek(std::io::SeekFrom::Start(0)).and_then(|_| r.read_exact(&mut full)).map_err(|e| format!("awb: {e}"))?;
    Ok(full)
}

fn write_xor(p: &Path, plain: &[u8]) -> Result<(), String> {
    std::fs::create_dir_all(p.parent().unwrap()).map_err(|e| e.to_string())?;
    let key = cri::crypt::loose_key(&p.file_name().unwrap_or_default().to_string_lossy());
    std::fs::write(p, cri::crypt::xor(plain, key, 0)).map_err(|e| format!("{}: {e}", p.display()))
}

fn edit_stamp(mod_id: &str, cue: &str, e: &CueEdit) -> String {
    format!("{mod_id}|{cue}|{}|{:?}", file_stamp(&e.file), e.treat)
}

fn build_job(env: &Env, j: &BankJob, out: &mut Built) -> Result<(), String> {
    let acb_key = format!("{}.acb", j.key);
    let awb_key = format!("{}.awb", j.key);
    let base_acb = base_of(env, &acb_key).ok_or_else(|| format!("{acb_key}: not in the game"))?;
    let base_awb = base_of(env, &awb_key);
    let mut parts = vec![format!("v{BUILDER_VERSION}"), file_stamp(&base_acb)];
    if let Some(a) = &base_awb {
        parts.push(file_stamp(a));
    }
    parts.extend(j.edits.iter().map(|(c, (m, e))| edit_stamp(m, c, e)));
    let hash = hash_inputs(&parts);
    let mut rels = vec![acb_key.clone()];
    if base_awb.is_some() {
        rels.push(awb_key.clone());
    }
    let rel_refs: Vec<&str> = rels.iter().map(String::as_str).collect();
    if env.cache.fresh(&rel_refs, &hash) {
        out.cached += 1;
    } else {
        let (acb, _) = read_plain(&base_acb, b"@UTF")?;
        let mut src = match &base_awb {
            Some(p) => Some(open_awb(p)?),
            None => None,
        };
        let head = match src.as_mut() {
            Some(r) => Some(awb_header(r.as_mut())?),
            None => None,
        };
        let mut b = Bank::open(&j.name, &acb, head.as_deref()).map_err(|e| e.to_string())?;
        for (cue, (m, e)) in &j.edits {
            let fmt_ci = if b.has_cue(cue) { b.cue_index(cue) } else { b.pick_template(cue) }.map_err(|e| e.to_string())?;
            let f = b.cue_format_at(fmt_ci, src.as_deref_mut().map(|r| r as &mut dyn cri::awb::ReadSeek)).map_err(|e| format!("{cue}: {e}"))?;
            let hca = source::to_hca(&e.file, &Target { rate: f.rate, channels: f.channels, frame_size: f.frame_size }, &e.treat).map_err(|x| format!("mod {m}: {cue}: {x}"))?;
            let r = if b.has_cue(cue) { b.replace(cue, hca) } else { b.add(cue, hca, fmt_ci) };
            let r = r.map_err(|x| format!("mod {m}: {cue}: {x}"))?;
            out.notes.push((false, format!("{}: {} {cue} from mod {m} ({})", j.name, if r.added { "added" } else { "replaced" }, e.file.file_name().unwrap_or_default().to_string_lossy())));
        }
        let awb_path = env.cache.path(&awb_key);
        let (acb2, written) = if base_awb.is_some() {
            std::fs::create_dir_all(awb_path.parent().unwrap()).map_err(|e| e.to_string())?;
            let tmp = awb_path.with_extension("awb.tmp");
            let mut f = std::io::BufWriter::with_capacity(1 << 20, std::fs::File::create(&tmp).map_err(|e| e.to_string())?);
            let key = cri::crypt::loose_key(&format!("{}.awb", j.name));
            let r = b.finish(src.as_deref_mut().map(|r| r as &mut dyn cri::awb::ReadSeek), Some(&mut f), Some(key)).map_err(|e| e.to_string())?;
            use std::io::Write;
            f.flush().map_err(|e| e.to_string())?;
            drop(f);
            std::fs::rename(&tmp, &awb_path).map_err(|e| e.to_string())?;
            r
        } else {
            b.finish(None, None, None).map_err(|e| e.to_string())?
        };
        write_xor(&env.cache.path(&acb_key), &acb2)?;
        env.cache.seal(&rel_refs, &hash).map_err(|e| e.to_string())?;
        out.rebuilt += 1;
        out.notes.push((false, format!("{}: built ({} cue(s); AWB {} B)", j.name, j.edits.len(), written.map_or(0, |w| w.size))));
    }
    for r in rels {
        let p = env.cache.path(&r);
        out.files.push((r, p));
    }
    Ok(())
}

fn build_new(env: &Env, j: &NewBankJob, out: &mut Built) -> Result<(), String> {
    let key = format!("{SOUND}/{}", j.name);
    let (acb_key, awb_key) = (format!("{key}.acb"), format!("{key}.awb"));
    let tpl_path = env.game.path(&format!("{SOUND}/{NEW_BANK_TEMPLATE}.acb")).ok_or("retail bgm_title.acb not found")?;
    let mut parts = vec![format!("v{BUILDER_VERSION}-new"), file_stamp(&tpl_path)];
    for c in &j.cues {
        parts.push(format!("{}|{:?}|{}", c.name, c.kind, edit_stamp(&j.mod_id, &c.name, &c.edit)));
    }
    let hash = hash_inputs(&parts);
    let rels = [acb_key.as_str(), awb_key.as_str()];
    if env.cache.fresh(&rels, &hash) {
        out.cached += 1;
    } else {
        let (tpl, _) = read_plain(&tpl_path, b"@UTF")?;
        let mut cues = Vec::new();
        let mut cmd_cache: BTreeMap<String, Vec<u8>> = BTreeMap::new();
        for c in &j.cues {
            let (target, cmd) = match &c.kind {
                NewKind::Music => (Target { rate: 48000, channels: 2, frame_size: 682 }, None),
                NewKind::Se(t) => {
                    let cmd = match cmd_cache.get(t) {
                        Some(v) => v.clone(),
                        None => {
                            let bank = ["common", "ch", "effect", "ob"].iter().find_map(|b| {
                                let p = env.game.path(&format!("{SOUND}/{b}.acb"))?;
                                let (a, _) = read_plain(&p, b"@UTF").ok()?;
                                cri::bank::seq_command_of(&a, t).ok()
                            });
                            let v = bank.ok_or_else(|| format!("category_template {t}: not a cue of common / ch / effect / ob"))?;
                            cmd_cache.insert(t.clone(), v.clone());
                            v
                        }
                    };
                    (Target { rate: 48000, channels: 1, frame_size: 341 }, Some(cmd))
                }
            };
            let hca = source::to_hca(&c.edit.file, &target, &c.edit.treat).map_err(|e| format!("mod {}: {}: {e}", j.mod_id, c.name))?;
            cues.push(NewCue { name: c.name.clone(), hca, seq_command: cmd });
        }
        let (acb, awb) = cri::bank::new_bank(&tpl, &j.name, &cues).map_err(|e| e.to_string())?;
        write_xor(&env.cache.path(&acb_key), &acb)?;
        write_xor(&env.cache.path(&awb_key), &awb)?;
        env.cache.seal(&rels, &hash).map_err(|e| e.to_string())?;
        out.rebuilt += 1;
        out.notes.push((false, format!("{}: new bank of mod {} built ({} cue(s): {})", j.name, j.mod_id, cues.len(), j.cues.iter().map(|c| c.name.as_str()).collect::<Vec<_>>().join(", "))));
    }
    out.files.push((acb_key.clone(), env.cache.path(&acb_key)));
    out.files.push((awb_key.clone(), env.cache.path(&awb_key)));
    Ok(())
}

/// Build (or take from the cache) every bank of `plan`. A bank that fails is logged and left retail. `prune`: delete
/// cached outputs nobody asks for any more (boot cache only, never a mod's `files\`).
pub fn execute(plan: &Plan, env: &Env, prune: bool) -> Built {
    let mut out = Built::default();
    for j in plan.jobs.values() {
        if let Err(e) = build_job(env, j, &mut out) {
            out.notes.push((true, format!("{}: NOT built ({e}): the game keeps its bank", j.name)));
        }
    }
    for j in &plan.new_banks {
        if let Err(e) = build_new(env, j, &mut out) {
            out.notes.push((true, format!("{}: NOT built ({e})", j.name)));
        }
    }
    if !prune {
        return out;
    }
    let mut keep: Vec<String> = out.files.iter().map(|f| f.0.clone()).collect();
    keep.push(BGM_CONFIG.to_string());
    keep.push(crate::sheet::GAME_PATH.to_string());
    for g in env.cache.prune(&keep) {
        if !g.to_string_lossy().ends_with(".stamp") {
            out.notes.push((false, format!("cache: removed {} (no longer asked for)", g.display())));
        }
    }
    out
}

// ---------------------------------------------------------------- bgm_config

/// Apply the plan's BGM rows / redirects to a `bgm_config` (RDBN `m_bgmInfoList`: bgm_id, bgm, …). New rows are
/// copies of the `bg00010` row (category 0) with `bgm_id = bgm = crc32(cue)`; a redirect sets `bgm` of every row
/// whose `bgm_id` is the target.
pub fn bgm_config(base: &[u8], plan: &Plan) -> Result<(Vec<u8>, Vec<Note>), String> {
    use l5_core::rdbn::{Rdbn, Value};
    let mut doc = Rdbn::parse(base).map_err(|e| format!("bgm_config: {e}"))?;
    let li = doc.lists.iter().position(|l| l.name == "m_bgmInfoList").ok_or("m_bgmInfoList not found")?;
    let ty = doc.list_type(&doc.lists[li]).ok_or("no type")?.clone();
    let fi = |n: &str| ty.fields.iter().position(|f| f.name == n).ok_or_else(|| format!("field {n}"));
    let (f_id, f_bgm) = (fi("bgm_id")?, fi("bgm")?);
    let hash = |v: &Vec<Value>| match v.first() {
        Some(Value::Hash(h)) => *h,
        Some(Value::Int(i)) => *i as u32,
        _ => 0,
    };
    let mut notes = Vec::new();
    let rows = &mut doc.lists[li].rows;
    let tpl = rows.iter().find(|r| hash(&r[f_id]) == 22097913).or(rows.first()).cloned().ok_or("empty bgm_config")?;
    for (m, crc, name) in &plan.bgm_rows {
        if rows.iter().any(|r| hash(&r[f_id]) == *crc) {
            notes.push((true, format!("bgm_config: {name} (mod {m}) is already an id: row not added")));
            continue;
        }
        let mut r = tpl.clone();
        r[f_id] = vec![Value::Hash(*crc)];
        r[f_bgm] = vec![Value::Hash(*crc)];
        rows.push(r);
        notes.push((false, format!("bgm_config: new id {name} (mod {m})")));
    }
    for (id, (m, crc, label)) in &plan.bgm_redirect {
        let mut hit = 0;
        for r in rows.iter_mut().filter(|r| hash(&r[f_id]) == *id) {
            r[f_bgm] = vec![Value::Hash(*crc)];
            hit += 1;
        }
        notes.push((hit == 0, if hit == 0 { format!("bgm_config: {label} (mod {m}): id not in the table") } else { format!("bgm_config: {label} (mod {m})") }));
    }
    Ok((doc.to_bytes().map_err(|e| format!("bgm_config write: {e}"))?, notes))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wav(dir: &Path, name: &str, ch: u16, secs: f32) -> PathBuf {
        let rate = 44100;
        let n = (rate as f32 * secs) as usize;
        let pcm: Vec<i16> = (0..n * ch as usize).map(|i| (7000.0 * (2.0 * std::f32::consts::PI * 523.0 * (i / ch as usize) as f32 / rate as f32).sin()) as i16).collect();
        let p = dir.join("audio").join(name);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(&p, cri::wav::write(&cri::hca::Pcm { channels: ch, sample_rate: rate, samples: pcm, loop_range: None })).unwrap();
        p
    }

    fn md(id: &str, dir: &Path, i: u32) -> ModDir {
        ModDir { id: id.into(), dir: dir.to_path_buf(), load_index: i }
    }

    #[test]
    fn plan_merges_per_cue_and_resolves_names() {
        let root = std::env::temp_dir().join(format!("evt-ae-plan-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let (a, b) = (root.join("a"), root.join("b"));
        wav(&a, "x.wav", 1, 0.2);
        wav(&b, "x.wav", 1, 0.2);
        let ta = crate::decl::parse(
            "[[sfx]]\nreplace = \"ev60_03380_me\"\nfile = \"audio/x.wav\"\n[[voice]]\ncharacter = \"Beta\"\narmour = \"Atenea\"\nfile = \"audio/x.wav\"\n[[music]]\nreplace = \"title\"\nfile = \"audio/x.wav\"\n",
        )
        .unwrap();
        let tb = crate::decl::parse(
            "[[sfx]]\ntechnique = \"Ruptura relámpago CG\"\nfile = \"audio/x.wav\"\n[[sfx]]\nreplace = \"sy0006\"\nfile = \"audio/x.wav\"\n[[sfx]]\nadd = \"b_ok\"\nfile = \"audio/x.wav\"\n[[music]]\nadd = \"b_theme\"\nplay_in = \"bg00010\"\nfile = \"audio/x.wav\"\n[[voice]]\nbank = \"c01000010\"\ncue = \"gl010\"\nfile = \"audio/nope.wav\"\n",
        )
        .unwrap();
        let p = plan(&[(md("a", &a, 0), ta), (md("b", &b, 1), tb)], &crate::index::sample());
        // same waza_stream cue from both mods: b (loads later) wins, with a warning
        let w = &p.jobs[&format!("{SOUND}/waza_stream")];
        assert_eq!(w.edits["ev60_03380_me"].0, "b");
        assert!(p.notes.iter().any(|(warn, t)| *warn && t.contains("audio conflict: cue") && t.contains("winner: b")));
        assert_eq!(p.jobs[&format!("{SOUND}/common")].edits.len(), 1);
        // armour per bank: c05020700 -> was00630, c11905050 -> was00631
        assert!(p.jobs[&format!("{SOUND}/ja/c05020700")].edits.contains_key("c05020700_was00630"));
        assert!(p.jobs[&format!("{SOUND}/ja/c11905050")].edits.contains_key("c11905050_was00631"));
        // the missing file is a warning, not a failure
        assert!(p.notes.iter().any(|(w, t)| *w && t.contains("nope.wav")));
        // new banks + registration; BGM: title redirected twice -> b wins
        assert_eq!(p.new_banks.iter().map(|n| n.name.as_str()).collect::<Vec<_>>(), ["evt_bgm_a", "evt_se_b", "evt_bgm_b"]);
        assert_eq!(p.regs.len(), 3);
        assert_eq!(p.bgm_redirect[&22097913].0, "b");
        assert_eq!(p.bgm_rows, vec![("b".to_string(), crc32fast::hash(b"b_theme"), "b_theme".to_string())]);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn prebuilt_banks_are_bases_not_rebuilt() {
        let root = std::env::temp_dir().join(format!("evt-ae-pre-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let a = root.join("a");
        wav(&a, "x.wav", 1, 0.2);
        let built = a.join("files/data/common/sound_asset/waza_stream.acb");
        std::fs::create_dir_all(built.parent().unwrap()).unwrap();
        std::fs::write(&built, b"x").unwrap();
        let t = crate::decl::parse("[[sfx]]\nreplace = \"ev60_03380_me\"\nfile = \"audio/x.wav\"\n").unwrap();
        let p = plan(&[(md("a", &a, 0), t)], &crate::index::sample());
        assert!(p.jobs.is_empty());
        let _ = std::fs::remove_dir_all(&root);
    }

}
