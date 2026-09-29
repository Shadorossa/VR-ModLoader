//! ACB cue sheets (docs/formats/audio-acb-awb-hca.md §2): cue names, the cue graph
//! (Cue → Sequence → Track → command stream → Synth → Waveform) and the AWB references.

use std::collections::BTreeSet;

use serde::Serialize;

use crate::awb::Afs2;
use crate::utf::Table;
use crate::{Error, Result};

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Waveform {
    /// Index in `WaveformTable`.
    pub index: usize,
    /// Id inside the memory AWB (`AwbFile`) when `streaming` is 0 or 2.
    pub memory_awb_id: u32,
    /// Id inside the external `<sheet>.awb` when `streaming` is 1 or 2.
    pub stream_awb_id: u32,
    pub stream_awb_port: u32,
    /// 2 = HCA; 6 is HCA too in this game (payload magic decides).
    pub encode_type: u8,
    /// 0 memory, 1 stream, 2 both.
    pub streaming: u8,
    pub channels: u8,
    pub sampling_rate: u32,
    pub num_samples: u32,
    /// `LoopFlag` 2 (or a loop in `WaveformExtensionDataTable`) → loop points in samples.
    pub loop_range: Option<(u32, u32)>,
}

impl Waveform {
    pub fn is_streamed(&self) -> bool {
        self.streaming != 0
    }

    pub fn duration_ms(&self) -> u32 {
        if self.sampling_rate == 0 {
            0
        } else {
            (self.num_samples as u64 * 1000 / self.sampling_rate as u64) as u32
        }
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Cue {
    /// Index in `CueTable`.
    pub index: usize,
    pub id: u32,
    pub name: String,
    /// 1 waveform, 2 synth, 3 sequence (the only type used by the game).
    pub reference_type: u8,
    pub reference_index: u32,
    /// `CueTable.Length` in ms (0xFFFFFFFF = infinite / unknown).
    pub length_ms: u32,
    /// Waveform indices reached from the cue, in play order (random / switch sequences list every candidate).
    pub waveforms: Vec<usize>,
}

/// A parsed cue sheet (only what the browser and the player need).
#[derive(Debug, Clone)]
pub struct Acb {
    pub name: String,
    pub version: u32,
    pub cues: Vec<Cue>,
    pub waveforms: Vec<Waveform>,
    /// Embedded memory AWB (`AwbFile`), when the sheet has memory waveforms.
    pub memory_awb: Option<Afs2>,
    /// Byte range of the memory AWB inside the ACB file.
    pub memory_awb_range: Option<(usize, usize)>,
    /// External AWB header copy (`StreamAwbAfs2Header`), parsed: locates streamed payloads without opening the .awb.
    pub stream_awb: Option<Afs2>,
    /// `StreamAwbHash` rows: (awb name, MD5 of the whole .awb).
    pub stream_awb_hash: Vec<(String, [u8; 16])>,
}

/// Parse a command stream (`repeated { u16 code BE, u8 len, len bytes }`) into `(code, payload)` items.
pub fn commands(mut d: &[u8]) -> Vec<(u16, &[u8])> {
    let mut out = Vec::new();
    while d.len() >= 3 {
        let code = u16::from_be_bytes([d[0], d[1]]);
        let len = d[2] as usize;
        let payload = &d[3..(3 + len).min(d.len())];
        out.push((code, payload));
        d = &d[(3 + len).min(d.len())..];
        if code == 0 && len == 0 {
            break;
        }
    }
    out
}

struct Graph<'a> {
    sequences: Option<Table<'a>>,
    tracks: Option<Table<'a>>,
    track_events: Option<Table<'a>>,
    synths: Option<Table<'a>>,
}

impl<'a> Graph<'a> {
    fn collect(&self, ref_type: u8, index: usize, out: &mut Vec<usize>, seen: &mut BTreeSet<(u8, usize)>, depth: usize) {
        if depth > 16 || !seen.insert((ref_type, index)) {
            return;
        }
        match ref_type {
            1 => out.push(index),
            2 | 5 | 7 => {
                let Some(t) = &self.synths else { return };
                let Some(items) = t.data(index, "ReferenceItems") else { return };
                for pair in items.chunks_exact(4) {
                    let ty = u16::from_be_bytes([pair[0], pair[1]]) as u8;
                    let idx = u16::from_be_bytes([pair[2], pair[3]]) as usize;
                    if ty != 0 {
                        self.collect(ty, idx, out, seen, depth + 1);
                    }
                }
            }
            3 => {
                let Some(t) = &self.sequences else { return };
                let n = t.int(index, "NumTracks").unwrap_or(0).max(0) as usize;
                let Some(idx) = t.data(index, "TrackIndex") else { return };
                for pair in idx.chunks_exact(2).take(n) {
                    let track = u16::from_be_bytes([pair[0], pair[1]]) as usize;
                    self.track(track, out, seen, depth + 1);
                }
            }
            _ => {}
        }
    }

    fn track(&self, track: usize, out: &mut Vec<usize>, seen: &mut BTreeSet<(u8, usize)>, depth: usize) {
        let (Some(tracks), Some(events)) = (&self.tracks, &self.track_events) else { return };
        let Some(ev) = tracks.int(track, "EventIndex") else { return };
        if ev < 0 || ev == 0xFFFF {
            return;
        }
        let Some(cmd) = events.data(ev as usize, "Command") else { return };
        for (code, payload) in commands(cmd) {
            // 0x07D0 "note on": (u16 refType, u16 refIndex). 0x004F carries the same pair in sequence commands.
            if matches!(code, 0x07D0 | 0x004F) && payload.len() >= 4 {
                let ty = u16::from_be_bytes([payload[0], payload[1]]) as u8;
                let idx = u16::from_be_bytes([payload[2], payload[3]]) as usize;
                if ty != 0 {
                    self.collect(ty, idx, out, seen, depth + 1);
                }
            }
        }
    }
}

impl Acb {
    pub fn parse(data: &[u8]) -> Result<Acb> {
        let root = Table::parse(data)?;
        if root.row_count == 0 {
            return Err(Error::Acb("empty header".into()));
        }
        let name = root.str(0, "Name").unwrap_or_default();
        let version = root.int(0, "Version").unwrap_or(0) as u32;

        // Waveforms + loop points.
        let ext = root.table(0, "WaveformExtensionDataTable");
        let mut waveforms = Vec::new();
        if let Some(wt) = root.table(0, "WaveformTable") {
            let has_split_ids = wt.column_index("StreamAwbId").is_some();
            for r in 0..wt.row_count {
                let streaming = wt.int(r, "Streaming").unwrap_or(0) as u8;
                let (memory_awb_id, stream_awb_id) = if has_split_ids {
                    (wt.int(r, "MemoryAwbId").unwrap_or(0xFFFF) as u32, wt.int(r, "StreamAwbId").unwrap_or(0xFFFF) as u32)
                } else {
                    // ACB < 1.30: one `Id` column for both.
                    let id = wt.int(r, "Id").unwrap_or(0xFFFF) as u32;
                    (id, id)
                };
                let loop_flag = wt.int(r, "LoopFlag").unwrap_or(0);
                let ext_idx = wt.int(r, "ExtensionData").unwrap_or(0xFFFF);
                let mut loop_range = None;
                if let (Some(et), true) = (&ext, ext_idx >= 0 && ext_idx != 0xFFFF) {
                    if let (Some(s), Some(e)) = (et.int(ext_idx as usize, "LoopStart"), et.int(ext_idx as usize, "LoopEnd")) {
                        if e > s && (loop_flag == 2 || loop_flag == 0) {
                            loop_range = Some((s as u32, e as u32));
                        }
                    }
                }
                waveforms.push(Waveform {
                    index: r,
                    memory_awb_id,
                    stream_awb_id,
                    stream_awb_port: wt.int(r, "StreamAwbPortNo").unwrap_or(0) as u32,
                    encode_type: wt.int(r, "EncodeType").unwrap_or(0) as u8,
                    streaming,
                    channels: wt.int(r, "NumChannels").unwrap_or(0) as u8,
                    sampling_rate: wt.int(r, "SamplingRate").unwrap_or(0) as u32,
                    num_samples: wt.int(r, "NumSamples").unwrap_or(0) as u32,
                    loop_range,
                });
            }
        }

        // Cues.
        let graph = Graph {
            sequences: root.table(0, "SequenceTable"),
            tracks: root.table(0, "TrackTable"),
            track_events: root.table(0, "TrackEventTable"),
            synths: root.table(0, "SynthTable"),
        };
        let cue_table = root.table(0, "CueTable");
        let mut cues: Vec<Cue> = Vec::new();
        if let Some(ct) = &cue_table {
            for r in 0..ct.row_count {
                let reference_type = ct.int(r, "ReferenceType").unwrap_or(0) as u8;
                let reference_index = ct.int(r, "ReferenceIndex").unwrap_or(0) as u32;
                let mut wf = Vec::new();
                let mut seen = BTreeSet::new();
                graph.collect(reference_type, reference_index as usize, &mut wf, &mut seen, 0);
                wf.retain(|w| *w < waveforms.len());
                cues.push(Cue {
                    index: r,
                    id: ct.int(r, "CueId").unwrap_or(0) as u32,
                    name: String::new(),
                    reference_type,
                    reference_index,
                    length_ms: ct.int(r, "Length").unwrap_or(0) as u32,
                    waveforms: wf,
                });
            }
        }
        if let Some(nt) = root.table(0, "CueNameTable") {
            for r in 0..nt.row_count {
                let idx = nt.int(r, "CueIndex").unwrap_or(-1);
                if let (Some(name), Some(cue)) = (nt.str(r, "CueName"), usize::try_from(idx).ok().and_then(|i| cues.get_mut(i))) {
                    if cue.name.is_empty() {
                        cue.name = name;
                    }
                }
            }
        }
        for c in &mut cues {
            if c.name.is_empty() {
                c.name = format!("cue_{}", c.id);
            }
        }

        // AWBs.
        let (memory_awb, memory_awb_range) = match root.data(0, "AwbFile") {
            Some(d) if Afs2::is_afs2(d) => {
                let start = d.as_ptr() as usize - data.as_ptr() as usize;
                (Afs2::parse(d).ok(), Some((start, start + d.len())))
            }
            _ => (None, None),
        };
        let stream_awb = root.table(0, "StreamAwbAfs2Header").and_then(|t| (t.row_count > 0).then(|| t.data(0, "Header")).flatten()).and_then(|h| Afs2::parse(h).ok());
        let mut stream_awb_hash = Vec::new();
        if let Some(ht) = root.table(0, "StreamAwbHash") {
            for r in 0..ht.row_count {
                let n = ht.str(r, "Name").unwrap_or_default();
                let mut md5 = [0u8; 16];
                if let Some(h) = ht.data(r, "Hash") {
                    if h.len() == 16 {
                        md5.copy_from_slice(h);
                    }
                }
                stream_awb_hash.push((n, md5));
            }
        }
        Ok(Acb { name, version, cues, waveforms, memory_awb, memory_awb_range, stream_awb, stream_awb_hash })
    }

    pub fn cue(&self, name: &str) -> Option<&Cue> {
        self.cues.iter().find(|c| c.name == name).or_else(|| self.cues.iter().find(|c| c.name.eq_ignore_ascii_case(name)))
    }

    /// Where the payload of a waveform lives.
    pub fn locate(&self, w: &Waveform) -> Option<Location> {
        if w.is_streamed() {
            let e = self.stream_awb.as_ref()?.entry(w.stream_awb_id)?;
            Some(Location::Stream { start: e.start, end: e.end })
        } else {
            let e = self.memory_awb.as_ref()?.entry(w.memory_awb_id)?;
            let (base, _) = self.memory_awb_range?;
            Some(Location::Memory { start: base + e.start as usize, end: base + e.end as usize })
        }
    }
}

/// Byte range of a waveform payload: inside the ACB file (memory) or the external `.awb` (stream).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Location {
    Memory { start: usize, end: usize },
    Stream { start: u64, end: u64 },
}
