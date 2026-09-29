//! ACB / AWB **editing** (port of `research/scripts/voice_bank_edit.py` `Bank` and of the new-bank builders of
//! `bgm_add_build.py` / `fwa_sfx_build.py`; same rules, so the Python and Rust tools give the same banks).
//!
//! * [`Bank::replace`] — a cue plays new audio: its waveform is overwritten when no other cue uses it, else a new
//!   waveform is added; the cue gets one fresh track → event → synth chain. Stream waveforms go to the `.awb`, memory
//!   waveforms to the ACB's own `AwbFile`.
//! * [`Bank::add`] — a new cue, rows cloned from a template cue of the bank (cue-limit id and header work counts grown).
//! * [`new_bank`] — a brand-new bank (N cues, each 1 sequence → 1 track → 1 synth → 1 streamed waveform) from a 1-cue
//!   template ACB (retail `bgm_title.acb`), with per-cue sequence commands (BGM category, or a retail SE's category).
//! * [`Bank::finish`] — the ACB (+ the `.awb`, streamed from the source archive, never loaded whole) with
//!   `StreamAwbHash` (MD5) and `StreamAwbAfs2Header` in sync.

use std::collections::{HashMap, HashSet};

use crate::awb::{Afs2, AwbPlan, Payload, ReadSeek, Written};
use crate::hca;
use crate::utf_own::{Table, Val};
use crate::{Error, Result};

const TABLES: [&str; 8] = ["CueTable", "CueNameTable", "SequenceTable", "SeqCommandTable", "TrackTable", "TrackEventTable", "SynthTable", "WaveformTable"];

fn err(s: impl Into<String>) -> Error {
    Error::Acb(s.into())
}

/// Commands of a CRI command blob: (code, value).
pub fn cmds(b: &[u8]) -> Vec<(u16, Vec<u8>)> {
    let mut out = Vec::new();
    let mut o = 0;
    while o + 3 <= b.len() {
        let c = u16::from_be_bytes([b[o], b[o + 1]]);
        let n = b[o + 2] as usize;
        out.push((c, b.get(o + 3..o + 3 + n).unwrap_or(&[]).to_vec()));
        o += 3 + n;
    }
    out
}

pub fn pack_cmds(cs: &[(u16, Vec<u8>)]) -> Vec<u8> {
    let mut v = Vec::new();
    for (c, val) in cs {
        v.extend_from_slice(&c.to_be_bytes());
        v.push(val.len() as u8);
        v.extend_from_slice(val);
    }
    v
}

fn u16s(b: &[u8]) -> Vec<usize> {
    b.chunks_exact(2).map(|c| u16::from_be_bytes([c[0], c[1]]) as usize).collect()
}

/// Format of a waveform's HCA (what a replacement is converted to).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct WaveFormat {
    pub rate: u32,
    pub channels: u16,
    pub frame_size: u16,
    pub streamed: bool,
}

/// What a replacement / addition did (for logs).
#[derive(Debug, Clone, PartialEq)]
pub struct Edit {
    pub cue: String,
    pub added: bool,
    pub waveform: usize,
    pub reused_slot: bool,
}

/// One language copy of a bank, parsed with every table the edits touch.
pub struct Bank {
    pub name: String,
    pub root: Table,
    t: HashMap<&'static str, Table>,
    ext: Option<Table>,
    /// The `.awb` (ranges into the source archive until an entry is replaced).
    pub stream: Option<AwbPlan>,
    /// The memory AWB inside the ACB (`AwbFile`) and its bytes.
    memory: Option<(AwbPlan, Vec<u8>)>,
    names: HashMap<String, usize>,
}

impl Bank {
    /// `acb` = plain ACB bytes; `stream_header` = the start of the plain `.awb` (enough for its AFS2 header), if any.
    pub fn open(name: &str, acb: &[u8], stream_header: Option<&[u8]>) -> Result<Bank> {
        let root = Table::parse(acb, true)?;
        let mut t = HashMap::new();
        for n in TABLES {
            t.insert(n, root.nested(n, 0)?);
        }
        let ext = root.nested("WaveformExtensionDataTable", 0).ok();
        let stream = match stream_header {
            Some(h) => Some(AwbPlan::from_afs2(&Afs2::parse(h)?)),
            None => None,
        };
        let memory = match root.data(0, "AwbFile") {
            Some(d) if Afs2::is_afs2(d) => Some((AwbPlan::from_afs2(&Afs2::parse(d)?), d.to_vec())),
            _ => None,
        };
        let cn = &t["CueNameTable"];
        let mut names = HashMap::new();
        for r in 0..cn.rows.len() {
            if let (Some(Val::Str(n)), Some(i)) = (cn.get(r, "CueName"), cn.int(r, "CueIndex")) {
                names.insert(n.clone(), i as usize);
            }
        }
        Ok(Bank { name: name.to_string(), root, t, ext, stream, memory, names })
    }

    pub fn cue_names(&self) -> Vec<String> {
        let mut v: Vec<String> = self.names.keys().cloned().collect();
        v.sort();
        v
    }

    pub fn has_cue(&self, cue: &str) -> bool {
        self.names.contains_key(cue)
    }

    pub fn cue_index(&self, cue: &str) -> Result<usize> {
        self.names.get(cue).copied().ok_or_else(|| err(format!("cue {cue} is not in {}", self.name)))
    }

    fn tb(&self, n: &str) -> &Table {
        &self.t[n]
    }

    fn tbm(&mut self, n: &str) -> &mut Table {
        self.t.get_mut(n).expect("table")
    }

    /// (track, event, synth, waveforms) of every track of the cue's sequence.
    fn chains(&self, ci: usize) -> Result<Vec<(usize, usize, usize, Vec<usize>)>> {
        let c = self.tb("CueTable");
        if c.int(ci, "ReferenceType") != Some(3) {
            return Err(err(format!("cue {ci}: not a sequence cue")));
        }
        let si = c.int(ci, "ReferenceIndex").unwrap_or(0) as usize;
        let s = self.tb("SequenceTable");
        let nt = s.int(si, "NumTracks").unwrap_or(0) as usize;
        let mut out = Vec::new();
        for ti in u16s(s.data(si, "TrackIndex").unwrap_or(&[])).into_iter().take(nt) {
            let ei = self.tb("TrackTable").int(ti, "EventIndex").unwrap_or(0xFFFF) as usize;
            if ei == 0xFFFF {
                continue;
            }
            for (code, v) in cmds(self.tb("TrackEventTable").data(ei, "Command").unwrap_or(&[])) {
                if code == 0x07D0 && v.len() >= 4 {
                    let (rt, ri) = (u16::from_be_bytes([v[0], v[1]]), u16::from_be_bytes([v[2], v[3]]) as usize);
                    if rt != 2 {
                        return Err(err(format!("track {ti}: note-on of type {rt} (synth only)")));
                    }
                    let items = self.tb("SynthTable").data(ri, "ReferenceItems").unwrap_or(&[]).to_vec();
                    let mut ws = Vec::new();
                    for k in items.chunks_exact(4) {
                        if u16::from_be_bytes([k[0], k[1]]) != 1 {
                            return Err(err(format!("synth {ri}: reference that is not a waveform")));
                        }
                        ws.push(u16::from_be_bytes([k[2], k[3]]) as usize);
                    }
                    out.push((ti, ei, ri, ws));
                }
            }
        }
        Ok(out)
    }

    fn waveform_users(&self) -> HashMap<usize, HashSet<usize>> {
        let mut u: HashMap<usize, HashSet<usize>> = HashMap::new();
        for ci in 0..self.tb("CueTable").rows.len() {
            if let Ok(ch) = self.chains(ci) {
                for (_, _, _, ws) in ch {
                    for w in ws {
                        u.entry(w).or_default().insert(ci);
                    }
                }
            }
        }
        u
    }

    /// First waveform of a cue.
    pub fn cue_waveform(&self, ci: usize) -> Result<usize> {
        self.chains(ci)?.into_iter().flat_map(|c| c.3).next().ok_or_else(|| err("the cue plays no waveform"))
    }

    fn streamed(&self, wi: usize) -> bool {
        self.tb("WaveformTable").int(wi, "Streaming").unwrap_or(1) != 0
    }

    /// HCA header bytes of waveform `wi` (stream: read from `src` = the source `.awb`).
    fn wave_head(&self, wi: usize, src: Option<&mut dyn ReadSeek>) -> Result<Vec<u8>> {
        let w = self.tb("WaveformTable");
        let (plan, id, from_mem) = if self.streamed(wi) {
            (self.stream.as_ref().ok_or_else(|| err("streamed waveform but no .awb"))?, w.int(wi, "StreamAwbId").unwrap_or(0), false)
        } else {
            (&self.memory.as_ref().ok_or_else(|| err("memory waveform but no AwbFile"))?.0, w.int(wi, "MemoryAwbId").unwrap_or(0), true)
        };
        let p = plan.entries.iter().find(|e| e.0 == id as u32).map(|e| e.1.clone()).ok_or_else(|| err(format!("AWB id {id} missing")))?;
        match p {
            Payload::Bytes(b) => Ok(b[..b.len().min(4096)].to_vec()),
            Payload::Range(a, e) if from_mem => Ok(self.memory.as_ref().unwrap().1[a as usize..e.min(a + 4096) as usize].to_vec()),
            Payload::Range(a, e) => {
                let r = src.ok_or_else(|| err("no .awb source to read the waveform from"))?;
                let n = (e - a).min(4096) as usize;
                let mut b = vec![0u8; n];
                r.seek(std::io::SeekFrom::Start(a)).and_then(|_| r.read_exact(&mut b)).map_err(|e| err(format!("awb read: {e}")))?;
                Ok(b)
            }
        }
    }

    /// Format a replacement of `cue` must have (the first waveform's HCA: rate, channels, frame size).
    pub fn cue_format(&self, cue: &str, src: Option<&mut dyn ReadSeek>) -> Result<WaveFormat> {
        let wi = self.cue_waveform(self.cue_index(cue)?)?;
        self.wave_format(wi, src)
    }

    /// Format of cue index `ci` (e.g. a template picked by [`Self::pick_template`]).
    pub fn cue_format_at(&self, ci: usize, src: Option<&mut dyn ReadSeek>) -> Result<WaveFormat> {
        let wi = self.cue_waveform(ci)?;
        self.wave_format(wi, src)
    }

    fn wave_format(&self, wi: usize, src: Option<&mut dyn ReadSeek>) -> Result<WaveFormat> {
        let h = hca::Header::parse(&self.wave_head(wi, src)?)?;
        Ok(WaveFormat { rate: h.sample_rate, channels: h.channels as u16, frame_size: h.frame_size, streamed: self.streamed(wi) })
    }

    fn simple(&self, ci: usize) -> bool {
        matches!(self.chains(ci), Ok(ch) if ch.len() == 1 && ch[0].3.len() == 1 && self.streamed(ch[0].3[0]))
    }

    fn suffix_of(&self, ci: usize) -> String {
        self.names.iter().find(|(_, &i)| i == ci).map(|(n, _)| n.strip_prefix(&format!("{}_", self.name)).unwrap_or(n).to_string()).unwrap_or_default()
    }

    /// Template cue for a new cue (voice_bank_edit.py `pick_template`): a simple cue of the same suffix family.
    pub fn pick_template(&self, cue: &str) -> Result<usize> {
        let mut simple: Vec<usize> = self.names.values().copied().collect::<HashSet<_>>().into_iter().filter(|&ci| self.simple(ci)).collect();
        simple.sort();
        if simple.is_empty() {
            return Err(err(format!("{} has no simple cue to copy", self.name)));
        }
        let suf = cue.strip_prefix(&format!("{}_", self.name)).unwrap_or(cue).to_string();
        let is_wh = |s: &str| s.len() == 8 && s.starts_with("wh") && s[3..].bytes().all(|b| b.is_ascii_digit());
        let is_inc = |s: &str| s.len() == 11 && s.starts_with('k') && s.ends_with("_inc");
        let prefs: Vec<Box<dyn Fn(&str) -> bool>> = if suf.len() >= 8 && suf.starts_with("wa") && suf[3..8].bytes().all(|b| b.is_ascii_digit()) {
            vec![Box::new(|s: &str| s == "armed"), Box::new(is_wh)]
        } else if is_wh(&suf) || is_inc(&suf) {
            vec![Box::new(is_wh), Box::new(is_inc), Box::new(|s: &str| s.starts_with("wh"))]
        } else {
            let lead: String = suf.chars().take_while(|c| c.is_ascii_lowercase()).collect();
            if lead.is_empty() {
                vec![]
            } else {
                vec![Box::new(move |s: &str| s.starts_with(&lead) && s.len() > lead.len() && s[lead.len()..].bytes().all(|b| b.is_ascii_digit()))]
            }
        };
        for p in &prefs {
            if let Some(&ci) = simple.iter().find(|&&ci| p(&self.suffix_of(ci))) {
                return Ok(ci);
            }
        }
        Ok(simple[0])
    }

    fn set_waveform(&mut self, wi: usize, hca_bytes: Vec<u8>) -> Result<()> {
        let h = hca::Header::parse(&hca_bytes)?;
        let streamed = self.streamed(wi);
        let w = self.tbm("WaveformTable");
        w.set(wi, "EncodeType", Val::U8(2))?;
        w.set(wi, "NumSamples", Val::I64(h.sample_count() as i64))?;
        w.set(wi, "NumChannels", Val::I64(h.channels as i64))?;
        w.set(wi, "SamplingRate", Val::I64(h.sample_rate as i64))?;
        let lp = h.loop_samples();
        w.set(wi, "LoopFlag", Val::I64(if lp.is_some() { 2 } else { 1 }))?;
        let ext_index = match (lp, self.ext.as_mut()) {
            (Some((s, e)), Some(ext)) if !ext.rows.is_empty() => {
                let i = ext.push_copy(0);
                ext.set(i, "LoopStart", Val::I64(s as i64))?;
                ext.set(i, "LoopEnd", Val::I64(e as i64))?;
                i as i64
            }
            _ => 0xFFFF,
        };
        self.tbm("WaveformTable").set(wi, "ExtensionData", Val::I64(ext_index))?;
        let w = self.tb("WaveformTable");
        if streamed {
            let id = w.int(wi, "StreamAwbId").unwrap_or(0) as u32;
            self.stream.as_mut().ok_or_else(|| err("no .awb"))?.put(id, Payload::Bytes(hca_bytes));
        } else {
            let id = w.int(wi, "MemoryAwbId").unwrap_or(0) as u32;
            self.memory.as_mut().ok_or_else(|| err("no AwbFile"))?.0.put(id, Payload::Bytes(hca_bytes));
        }
        Ok(())
    }

    fn new_waveform(&mut self, tpl_wi: usize, hca_bytes: Vec<u8>) -> Result<usize> {
        let streamed = self.streamed(tpl_wi);
        let (col, plan) = if streamed { ("StreamAwbId", self.stream.as_ref()) } else { ("MemoryAwbId", self.memory.as_ref().map(|m| &m.0)) };
        let new_id = plan.ok_or_else(|| err("no AWB"))?.max_id().map_or(0, |m| m + 1);
        if new_id > 0xFFFF {
            return Err(err("AWB full"));
        }
        let w = self.tbm("WaveformTable");
        let wi = w.push_copy(tpl_wi);
        w.set(wi, col, Val::I64(new_id as i64))?;
        self.set_waveform(wi, hca_bytes)?;
        Ok(wi)
    }

    fn clone_chain(&mut self, chain: &(usize, usize, usize, Vec<usize>), wi: usize) -> Result<usize> {
        let (ti0, ei0, yi0, _) = chain;
        let s = self.tbm("SynthTable");
        let yi = s.push_copy(*yi0);
        let mut items = 1u16.to_be_bytes().to_vec();
        items.extend_from_slice(&(wi as u16).to_be_bytes());
        s.set(yi, "ReferenceItems", Val::Data(items))?;
        for c in ["ControlWorkArea1", "ControlWorkArea2"] {
            if s.col(c).is_some() && s.int(*yi0, c) == Some(*yi0 as i64) {
                s.set(yi, c, Val::I64(yi as i64))?;
            }
        }
        let ev: Vec<(u16, Vec<u8>)> = cmds(self.tb("TrackEventTable").data(*ei0, "Command").unwrap_or(&[]))
            .into_iter()
            .map(|(c, v)| {
                if c == 0x07D0 && v.len() >= 4 {
                    let mut nv = 2u16.to_be_bytes().to_vec();
                    nv.extend_from_slice(&(yi as u16).to_be_bytes());
                    nv.extend_from_slice(&v[4..]);
                    (c, nv)
                } else {
                    (c, v)
                }
            })
            .collect();
        let e = self.tbm("TrackEventTable");
        let ei = e.push_copy(*ei0);
        e.set(ei, "Command", Val::Data(pack_cmds(&ev)))?;
        let t = self.tbm("TrackTable");
        let ti = t.push_copy(*ti0);
        t.set(ti, "EventIndex", Val::I64(ei as i64))?;
        Ok(ti)
    }

    /// `cue` plays `hca_bytes` from now on (same format as [`Self::cue_format`] expected).
    pub fn replace(&mut self, cue: &str, hca_bytes: Vec<u8>) -> Result<Edit> {
        let h = hca::Header::parse(&hca_bytes)?;
        let ci = self.cue_index(cue)?;
        let chains = self.chains(ci)?;
        if chains.is_empty() {
            return Err(err(format!("cue {cue} plays no waveform")));
        }
        let mut old: Vec<usize> = chains.iter().flat_map(|c| c.3.clone()).collect::<HashSet<_>>().into_iter().collect();
        old.sort();
        let users = self.waveform_users();
        let reused = old.len() == 1 && users.get(&old[0]).is_some_and(|u| u.len() == 1 && u.contains(&ci));
        let wi = if reused {
            self.set_waveform(old[0], hca_bytes)?;
            old[0]
        } else {
            self.new_waveform(old[0], hca_bytes)?
        };
        let ti = self.clone_chain(&chains[0], wi)?;
        let si = self.tb("CueTable").int(ci, "ReferenceIndex").unwrap_or(0) as usize;
        let s = self.tbm("SequenceTable");
        s.set(si, "NumTracks", Val::I64(1))?;
        s.set(si, "TrackIndex", Val::Data((ti as u16).to_be_bytes().to_vec()))?;
        if let Some(tv) = s.data(si, "TrackValues").map(|d| d.to_vec()).filter(|d| !d.is_empty()) {
            s.set(si, "TrackValues", Val::Data(tv[..2.min(tv.len())].to_vec()))?;
        }
        let len = if h.loop_samples().is_some() { 0xFFFF_FFFF } else { h.sample_count() * 1000 / h.sample_rate.max(1) as u64 };
        let c = self.tbm("CueTable");
        c.set(ci, "Length", Val::I64(len as i64))?;
        c.set(ci, "NumRelatedWaveforms", Val::I64(1))?;
        Ok(Edit { cue: cue.to_string(), added: false, waveform: wi, reused_slot: reused })
    }

    fn header_int(&self, n: &str) -> i64 {
        self.root.int(0, n).unwrap_or(0)
    }

    /// A new cue `cue` playing `hca_bytes`, rows cloned from template cue index `tpl`.
    pub fn add(&mut self, cue: &str, hca_bytes: Vec<u8>, tpl: usize) -> Result<Edit> {
        if self.has_cue(cue) {
            return Err(err(format!("cue {cue} already exists in {}", self.name)));
        }
        let h = hca::Header::parse(&hca_bytes)?;
        let chain = self.chains(tpl)?.into_iter().next().ok_or_else(|| err("template cue without waveform"))?;
        let wi = self.new_waveform(chain.3[0], hca_bytes)?;
        let ti = self.clone_chain(&chain, wi)?;
        let si0 = self.tb("CueTable").int(tpl, "ReferenceIndex").unwrap_or(0) as usize;
        let ci = self.tb("CueTable").rows.len();
        let si = self.tb("SequenceTable").rows.len();
        let limit = self.header_int("NumCueLimitListWorks");
        let ci0 = self.tb("SequenceTable").int(si0, "CommandIndex").unwrap_or(0) as usize;
        let sc: Vec<(u16, Vec<u8>)> = cmds(self.tb("SeqCommandTable").data(ci0, "Command").unwrap_or(&[]))
            .into_iter()
            .map(|(c, mut v)| {
                if c == 0x004F && v.len() >= 4 {
                    v[2..4].copy_from_slice(&(limit as u16).to_be_bytes());
                }
                (c, v)
            })
            .collect();
        let sct = self.tbm("SeqCommandTable");
        let sci = sct.push_copy(ci0);
        sct.set(sci, "Command", Val::Data(pack_cmds(&sc)))?;
        let s = self.tbm("SequenceTable");
        let new_si = s.push_copy(si0);
        s.set(new_si, "NumTracks", Val::I64(1))?;
        s.set(new_si, "TrackIndex", Val::Data((ti as u16).to_be_bytes().to_vec()))?;
        s.set(new_si, "CommandIndex", Val::I64(sci as i64))?;
        if s.col("ControlWorkArea1").is_some() && s.int(si0, "ControlWorkArea1") == Some(si0 as i64) {
            s.set(new_si, "ControlWorkArea1", Val::I64(si as i64))?;
        }
        if let Some(tv) = s.data(si0, "TrackValues").map(|d| d.to_vec()).filter(|d| !d.is_empty()) {
            s.set(new_si, "TrackValues", Val::Data(tv[..2.min(tv.len())].to_vec()))?;
        }
        if let Some(hist) = s.data(si0, "NumPlaybackTrackNoHistories").map(|d| d.to_vec()) {
            if hist.len() == 2 && u16s(&hist)[0] == si0 {
                s.set(new_si, "NumPlaybackTrackNoHistories", Val::Data((si as u16).to_be_bytes().to_vec()))?;
            }
        }
        let c = self.tbm("CueTable");
        let cue_id = (0..c.rows.len()).filter_map(|r| c.int(r, "CueId")).max().unwrap_or(0) + 1;
        let new_ci = c.push_copy(tpl);
        c.set(new_ci, "CueId", Val::I64(cue_id))?;
        c.set(new_ci, "ReferenceIndex", Val::I64(si as i64))?;
        let len = if h.loop_samples().is_some() { 0xFFFF_FFFF } else { h.sample_count() * 1000 / h.sample_rate.max(1) as u64 };
        c.set(new_ci, "Length", Val::I64(len as i64))?;
        c.set(new_ci, "NumRelatedWaveforms", Val::I64(1))?;
        debug_assert_eq!(new_ci, ci);
        // CueNameTable: sorted by name bytes
        let n = self.tbm("CueNameTable");
        let r = n.push_copy(0);
        n.set(r, "CueName", Val::Str(cue.to_string()))?;
        n.set(r, "CueIndex", Val::I64(ci as i64))?;
        let (nc, ic) = (n.col("CueName").unwrap(), n.col("CueIndex").unwrap());
        let _ = ic;
        n.rows.sort_by(|a, b| a[nc].as_str().unwrap_or("").as_bytes().cmp(b[nc].as_str().unwrap_or("").as_bytes()));
        self.names.insert(cue.to_string(), ci);
        self.root.set(0, "NumCueLimitListWorks", Val::I64(limit + 1))?;
        let nw = self.header_int("NumCueLimitNodeWorks");
        self.root.set(0, "NumCueLimitNodeWorks", Val::I64(nw + 1))?;
        let need = 64 * (limit as usize + 1) + 8;
        let mut clw = self.root.data(0, "CueLimitWorkTable").unwrap_or(&[]).to_vec();
        if clw.len() < need {
            clw.resize(need, 0);
            self.root.set(0, "CueLimitWorkTable", Val::Data(clw))?;
        }
        Ok(Edit { cue: cue.to_string(), added: true, waveform: wi, reused_slot: false })
    }

    /// Write the edited bank: the ACB bytes (plain) and, if the bank streams, the `.awb` to `awb_out` (reading the
    /// untouched entries from `src`, optionally XOR-encrypted with `awb_xor`).
    pub fn finish(mut self, src: Option<&mut dyn ReadSeek>, awb_out: Option<&mut dyn std::io::Write>, awb_xor: Option<u32>) -> Result<(Vec<u8>, Option<Written>)> {
        for n in TABLES {
            let tb = self.t.remove(n).unwrap();
            self.root.set_nested(n, 0, tb)?;
        }
        if let Some(ext) = self.ext.take() {
            self.root.set_nested("WaveformExtensionDataTable", 0, ext)?;
        }
        if let Some((plan, old)) = self.memory.take() {
            let mut v = Vec::new();
            plan.write(Some(&mut std::io::Cursor::new(&old)), &mut v, None).map_err(|e| err(format!("memory AWB: {e}")))?;
            self.root.set(0, "AwbFile", Val::Data(v))?;
        }
        let mut written = None;
        if let (Some(plan), Some(out)) = (self.stream.as_ref(), awb_out) {
            let w = plan.write(src, out, awb_xor).map_err(|e| err(format!("AWB write: {e}")))?;
            let mut h = self.root.nested("StreamAwbHash", 0)?;
            h.rows.truncate(1);
            h.set(0, "Name", Val::Str(self.name.clone()))?;
            h.set(0, "Hash", Val::Data(w.md5.to_vec()))?;
            self.root.set_nested("StreamAwbHash", 0, h)?;
            let mut hd = self.root.nested("StreamAwbAfs2Header", 0)?;
            hd.rows.truncate(1);
            hd.set(0, "Header", Val::Data(w.header.clone()))?;
            self.root.set_nested("StreamAwbAfs2Header", 0, hd)?;
            written = Some(w);
        }
        Ok((self.root.build()?, written))
    }
}

/// One cue of a [`new_bank`].
#[derive(Debug, Clone)]
pub struct NewCue {
    pub name: String,
    pub hca: Vec<u8>,
    /// Sequence command of the cue (category, volume…): `None` = the template's (BGM). Its cue-limit id (command
    /// 0x004F) is set to the cue's index.
    pub seq_command: Option<Vec<u8>>,
}

/// The sequence command of cue `cue` of `acb` (e.g. retail `common.acb` `sy0006`: the SE category).
pub fn seq_command_of(acb: &[u8], cue: &str) -> Result<Vec<u8>> {
    let b = Bank::open("x", acb, None)?;
    let ci = b.cue_index(cue)?;
    let si = b.tb("CueTable").int(ci, "ReferenceIndex").unwrap_or(0) as usize;
    let ci = b.tb("SequenceTable").int(si, "CommandIndex").unwrap_or(0) as usize;
    Ok(b.tb("SeqCommandTable").data(ci, "Command").unwrap_or(&[]).to_vec())
}

fn set_limit_id(cmd: &[u8], id: usize) -> Vec<u8> {
    let cs: Vec<(u16, Vec<u8>)> = cmds(cmd)
        .into_iter()
        .map(|(c, mut v)| {
            if c == 0x004F && v.len() >= 4 {
                v[2..4].copy_from_slice(&(id as u16).to_be_bytes());
            }
            (c, v)
        })
        .collect();
    pack_cmds(&cs)
}

/// All rows of `t` = copies of row 0, `n` of them; then `f(i)` sets row `i`.
fn set_rows(t: &mut Table, n: usize, f: impl Fn(usize) -> Vec<(&'static str, Val)>) -> Result<()> {
    let base = t.rows.first().cloned().ok_or_else(|| err(format!("{}: template has no row", t.name)))?;
    t.rows = vec![base; n];
    for i in 0..n {
        for (c, v) in f(i) {
            t.set(i, c, v)?;
        }
    }
    Ok(())
}

/// A new bank `name` with `cues` (each streamed from the new `.awb`, ids 0..n), built on a 1-cue template ACB (plain
/// bytes of retail `bgm_title.acb`). Returns (plain ACB, plain AWB).
pub fn new_bank(template: &[u8], name: &str, cues: &[NewCue]) -> Result<(Vec<u8>, Vec<u8>)> {
    let n = cues.len();
    if n == 0 || n > 0xFFFF {
        return Err(err("a new bank needs 1..65535 cues"));
    }
    let heads: Vec<hca::Header> = cues.iter().map(|c| hca::Header::parse(&c.hca)).collect::<Result<_>>()?;
    let mut b = Bank::open(name, template, None)?;
    let tpl_cmd = {
        let ci = b.tb("SequenceTable").int(0, "CommandIndex").unwrap_or(0) as usize;
        b.tb("SeqCommandTable").data(ci, "Command").unwrap_or(&[]).to_vec()
    };
    b.root.set(0, "Name", Val::Str(name.to_string()))?;
    let guid: [u8; 16] = {
        use md5::Digest;
        md5::Md5::digest(format!("evt-bank:{name}").as_bytes()).into()
    };
    b.root.set(0, "AcbGuid", Val::Data(guid.to_vec()))?;
    let ms = |h: &hca::Header| if h.loop_samples().is_some() { 0xFFFF_FFFFi64 } else { (h.sample_count() * 1000 / h.sample_rate.max(1) as u64) as i64 };
    set_rows(b.tbm("CueTable"), n, |i| {
        vec![("CueId", Val::I64(i as i64)), ("ReferenceType", Val::I64(3)), ("ReferenceIndex", Val::I64(i as i64)), ("NumRelatedWaveforms", Val::I64(1)), ("Length", Val::I64(ms(&heads[i])))]
    })?;
    let mut order: Vec<usize> = (0..n).collect();
    order.sort_by(|&a, &c| cues[a].name.as_bytes().cmp(cues[c].name.as_bytes()));
    set_rows(b.tbm("CueNameTable"), n, |k| vec![("CueName", Val::Str(cues[order[k]].name.clone())), ("CueIndex", Val::I64(order[k] as i64))])?;
    set_rows(b.tbm("SequenceTable"), n, |i| {
        vec![("NumTracks", Val::I64(1)), ("TrackIndex", Val::Data((i as u16).to_be_bytes().to_vec())), ("CommandIndex", Val::I64(i as i64))]
    })?;
    set_rows(b.tbm("SeqCommandTable"), n, |i| vec![("Command", Val::Data(set_limit_id(cues[i].seq_command.as_deref().unwrap_or(&tpl_cmd), i)))])?;
    set_rows(b.tbm("TrackTable"), n, |i| vec![("EventIndex", Val::I64(i as i64))])?;
    set_rows(b.tbm("TrackEventTable"), n, |i| {
        let mut c = vec![0x07, 0xD0, 0x04, 0x00, 0x02];
        c.extend_from_slice(&(i as u16).to_be_bytes());
        c.extend_from_slice(&[0, 0, 0]);
        vec![("Command", Val::Data(c))]
    })?;
    set_rows(b.tbm("SynthTable"), n, |i| {
        let mut r = vec![0, 1];
        r.extend_from_slice(&(i as u16).to_be_bytes());
        vec![("ReferenceItems", Val::Data(r))]
    })?;
    // loops: one WaveformExtensionDataTable row per looping cue
    let mut ext_rows: Vec<(u64, u64)> = Vec::new();
    let mut ext_of = vec![0xFFFFi64; n];
    for (i, h) in heads.iter().enumerate() {
        if let Some(l) = h.loop_samples() {
            ext_of[i] = ext_rows.len() as i64;
            ext_rows.push(l);
        }
    }
    set_rows(b.tbm("WaveformTable"), n, |i| {
        vec![
            ("MemoryAwbId", Val::I64(0xFFFF)),
            ("EncodeType", Val::I64(2)),
            ("Streaming", Val::I64(1)),
            ("NumChannels", Val::I64(heads[i].channels as i64)),
            ("LoopFlag", Val::I64(if ext_of[i] != 0xFFFF { 2 } else { 1 })),
            ("SamplingRate", Val::I64(heads[i].sample_rate as i64)),
            ("NumSamples", Val::I64(heads[i].sample_count() as i64)),
            ("ExtensionData", Val::I64(ext_of[i])),
            ("StreamAwbPortNo", Val::I64(0)),
            ("StreamAwbId", Val::I64(i as i64)),
        ]
    })?;
    if let Some(ext) = b.ext.as_mut() {
        if !ext_rows.is_empty() {
            set_rows(ext, ext_rows.len(), |k| vec![("LoopStart", Val::I64(ext_rows[k].0 as i64)), ("LoopEnd", Val::I64(ext_rows[k].1 as i64))])?;
        }
    }
    b.root.set(0, "NumCueLimitListWorks", Val::I64(n as i64))?;
    b.root.set(0, "NumCueLimitNodeWorks", Val::I64(n as i64))?;
    let need = 64 * n + 8;
    if b.root.data(0, "CueLimitWorkTable").is_some_and(|d| d.len() < need) {
        b.root.set(0, "CueLimitWorkTable", Val::Data(vec![0; need]))?;
    }
    b.memory = None;
    b.stream = Some(AwbPlan { version: 2, offset_size: 4, id_size: 2, alignment: 0x20, subkey: 0, entries: cues.iter().enumerate().map(|(i, c)| (i as u32, Payload::Bytes(c.hca.clone()))).collect() });
    let mut awb = Vec::new();
    let (acb, _) = b.finish(None, Some(&mut awb), None)?;
    Ok((acb, awb))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hca_enc;

    fn dump(rel: &str) -> Option<Vec<u8>> {
        let p = std::path::Path::new(crate::DUMP_SOUND_ASSET).join(rel);
        let b = std::fs::read(&p).ok()?;
        if &b[..4] == b"@UTF" || &b[..4] == b"AFS2" {
            return Some(b);
        }
        Some(crate::crypt::xor(&b, crate::crypt::loose_key(p.file_name()?.to_str()?), 0))
    }

    fn tone(n: usize, ch: usize, f: f64, rate: u32) -> Vec<i16> {
        (0..n * ch).map(|i| (9000.0 * (2.0 * std::f64::consts::PI * f * (i / ch) as f64 / rate as f64).sin()) as i16).collect()
    }

    /// Decode cue `cue` of an (acb, awb) pair back to PCM.
    fn decode_cue(acb: &[u8], awb: &[u8], name: &str, cue: &str) -> crate::hca::Pcm {
        let b = Bank::open(name, acb, Some(awb)).unwrap();
        let wi = b.cue_waveform(b.cue_index(cue).unwrap()).unwrap();
        let w = b.tb("WaveformTable");
        let a = Afs2::parse(awb).unwrap();
        let id = w.int(wi, "StreamAwbId").unwrap() as u32;
        crate::hca::decode(a.payload(awb, id).unwrap()).unwrap()
    }

    #[test]
    fn replace_and_add_in_a_retail_voice_bank() {
        let (Some(acb), Some(awb)) = (dump("ja/c01000010.acb"), dump("ja/c01000010.awb")) else { return };
        let mut b = Bank::open("c01000010", &acb, Some(&awb[..4096.min(awb.len())])).unwrap();
        let before = b.cue_names();
        let mut src = std::io::Cursor::new(awb.clone());
        let f = b.cue_format("c01000010_gl010", Some(&mut src)).unwrap();
        assert!(f.streamed);
        let hca = hca_enc::encode(&tone(f.rate as usize, f.channels as usize, 440.0, f.rate), f.channels, f.rate, hca_enc::Options { frame_size: f.frame_size, loop_range: None }).unwrap();
        let e = b.replace("c01000010_gl010", hca.clone()).unwrap();
        assert!(!e.added);
        let tpl = b.pick_template("c01000010_whd00020").unwrap();
        b.add("c01000010_whd00020", hca, tpl).unwrap();
        let mut out = Vec::new();
        let (acb2, w) = b.finish(Some(&mut src), Some(&mut out), None).unwrap();
        let w = w.unwrap();
        use md5::Digest;
        assert_eq!(w.md5, <[u8; 16]>::from(md5::Md5::digest(&out)));
        let b2 = Bank::open("c01000010", &acb2, Some(&out)).unwrap();
        let mut after = before.clone();
        after.push("c01000010_whd00020".into());
        after.sort();
        assert_eq!(b2.cue_names(), after);
        // both cues decode to 1 s of tone; an untouched cue is byte-identical to retail
        for cue in ["c01000010_gl010", "c01000010_whd00020"] {
            let p = decode_cue(&acb2, &out, "c01000010", cue);
            assert_eq!(p.frame_count(), f.rate as usize, "{cue}");
        }
        let a1 = Afs2::parse(&awb).unwrap();
        let a2 = Afs2::parse(&out).unwrap();
        let bank1 = Bank::open("c01000010", &acb, Some(&awb)).unwrap();
        let other = before.iter().find(|c| !c.ends_with("gl010")).unwrap();
        let wi = bank1.cue_waveform(bank1.cue_index(other).unwrap()).unwrap();
        let id = bank1.tb("WaveformTable").int(wi, "StreamAwbId").unwrap() as u32;
        assert_eq!(a1.payload(&awb, id), a2.payload(&out, id));
        // stream hash / header of the ACB match the new AWB
        let root = Table::parse(&acb2, true).unwrap();
        assert_eq!(root.nested("StreamAwbHash", 0).unwrap().data(0, "Hash").unwrap(), &w.md5);
        assert!(out.starts_with(root.nested("StreamAwbAfs2Header", 0).unwrap().data(0, "Header").unwrap()));
    }

    #[test]
    fn replace_a_memory_se() {
        let (Some(acb), Some(awb)) = (dump("common.acb"), dump("common.awb")) else { return };
        let mut b = Bank::open("common", &acb, Some(&awb[..65536.min(awb.len())])).unwrap();
        let mut src = std::io::Cursor::new(awb.clone());
        let f = b.cue_format("sy0006", Some(&mut src)).unwrap();
        let hca = hca_enc::encode(&tone(4800, f.channels as usize, 1000.0, f.rate), f.channels, f.rate, hca_enc::Options { frame_size: f.frame_size, loop_range: None }).unwrap();
        b.replace("sy0006", hca).unwrap();
        let n_before = b.names.len();
        let mut out = Vec::new();
        let (acb2, _) = b.finish(Some(&mut src), Some(&mut out), None).unwrap();
        let b2 = Bank::open("common", &acb2, Some(&out)).unwrap();
        let wi = b2.cue_waveform(b2.cue_index("sy0006").unwrap()).unwrap();
        let f2 = b2.wave_format(wi, Some(&mut std::io::Cursor::new(out.clone()))).unwrap();
        assert_eq!(f2.streamed, f.streamed);
        assert_eq!(b2.cue_names().len(), n_before);
    }

    #[test]
    fn new_bank_from_bgm_title() {
        let Some(tpl) = dump("bgm_title.acb") else { return };
        let music = hca_enc::encode(&tone(96_000, 2, 330.0, 48000), 2, 48000, hca_enc::Options { frame_size: 0, loop_range: Some((1000, 90_000)) }).unwrap();
        let se = hca_enc::encode(&tone(4800, 1, 880.0, 48000), 1, 48000, hca_enc::Options::default()).unwrap();
        let se_cmd = dump("common.acb").and_then(|c| seq_command_of(&c, "sy0006").ok());
        let (acb, awb) = new_bank(&tpl, "evt_bgm_test", &[
            NewCue { name: "evt_bgm_test_b".into(), hca: music, seq_command: None },
            NewCue { name: "evt_bgm_test_a".into(), hca: se, seq_command: se_cmd },
        ])
        .unwrap();
        let b = Bank::open("evt_bgm_test", &acb, Some(&awb)).unwrap();
        assert_eq!(b.cue_names(), ["evt_bgm_test_a", "evt_bgm_test_b"]);
        assert_eq!(b.root.get(0, "Name"), Some(&Val::Str("evt_bgm_test".into())));
        let p = decode_cue(&acb, &awb, "evt_bgm_test", "evt_bgm_test_b");
        assert_eq!(p.loop_range, Some((1000, 90_000)));
        let w = b.tb("WaveformTable");
        assert_eq!((w.int(0, "LoopFlag"), w.int(1, "LoopFlag")), (Some(2), Some(1)));
        let ext = b.ext.as_ref().unwrap();
        assert_eq!((ext.int(0, "LoopStart"), ext.int(0, "LoopEnd")), (Some(1000), Some(90_000)));
        // cue-limit ids 0, 1
        let sc = b.tb("SeqCommandTable");
        for i in 0..2 {
            let lim = cmds(sc.data(i, "Command").unwrap()).into_iter().find(|c| c.0 == 0x004F).unwrap().1;
            assert_eq!(u16::from_be_bytes([lim[2], lim[3]]) as usize, i);
        }
        use md5::Digest;
        let root = Table::parse(&acb, true).unwrap();
        assert_eq!(root.nested("StreamAwbHash", 0).unwrap().data(0, "Hash").unwrap(), &<[u8; 16]>::from(md5::Md5::digest(&awb)));
    }
}
