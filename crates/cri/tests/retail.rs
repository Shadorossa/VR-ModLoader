//! Read-only checks against the game's banks. Every test skips when the dump is not on this PC.
//!
//! `CRI_SOUND_ASSET` overrides the folder (default: the v7.1.2 dump's `data/common/sound_asset`).
//! `CRI_REF_DIR` (optional): folder with `<stem>.hca` + `<stem>.ref.wav` pairs decoded by another
//! decoder (`tools/hcaenc decode`, VGAudio 2.2.1); `reference_pcm_matches` compares our PCM against them.
//! VGAudio is only a valid reference for files with `min_res` ≥ 1 (v2.0 and older): it mis-parses the
//! v3.0 `min_res` 0 streams the game uses (see `v3_frames_parse_within_their_payload`), so those pairs
//! are only reported, not asserted.

use std::path::{Path, PathBuf};

use cri::acb::{Acb, Location};
use cri::awb::Afs2;
use cri::hca::{self, Header};

fn asset_dir() -> Option<PathBuf> {
    let p = std::env::var("CRI_SOUND_ASSET")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/../../assets/v7.1.2/data/common/sound_asset")));
    p.join("bgm_title.acb").is_file().then_some(p)
}

fn read(p: &Path) -> Vec<u8> {
    std::fs::read(p).unwrap_or_else(|e| panic!("{}: {e}", p.display()))
}

/// Payload bytes of a waveform of `acb` (`stem.awb` next to it for streamed ones).
fn payload(dir: &Path, stem: &str, acb_bytes: &[u8], acb: &Acb, w: usize) -> Vec<u8> {
    match acb.locate(&acb.waveforms[w]).expect("waveform located") {
        Location::Memory { start, end } => acb_bytes[start..end].to_vec(),
        Location::Stream { start, end } => {
            use std::io::{Read, Seek, SeekFrom};
            let mut f = std::fs::File::open(dir.join(format!("{stem}.awb"))).unwrap();
            f.seek(SeekFrom::Start(start)).unwrap();
            let mut buf = vec![0u8; (end - start) as usize];
            f.read_exact(&mut buf).unwrap();
            buf
        }
    }
}

#[test]
fn bgm_title_has_one_cue_that_decodes() {
    let Some(dir) = asset_dir() else { eprintln!("skip: no sound_asset dump"); return };
    let bytes = read(&dir.join("bgm_title.acb"));
    let acb = Acb::parse(&bytes).unwrap();
    assert_eq!(acb.name, "bgm_title");
    assert_eq!(acb.cues.len(), 1);
    assert_eq!(acb.cues[0].name, "bg00010");
    assert_eq!(acb.cues[0].waveforms, vec![0]);
    let w = &acb.waveforms[0];
    assert_eq!(w.sampling_rate, 48000);
    assert!(w.is_streamed());
    // The external AWB header copied into the ACB matches the .awb on disk.
    let awb_head = read(&dir.join("bgm_title.awb"));
    let afs2 = Afs2::parse(&awb_head).unwrap();
    let embedded = acb.stream_awb.as_ref().unwrap();
    assert_eq!(afs2.entries.len(), embedded.entries.len());
    assert_eq!(afs2.entries[0].start, embedded.entries[0].start);
    let hca = payload(&dir, "bgm_title", &bytes, &acb, 0);
    let header = Header::parse(&hca).unwrap();
    assert!(header.crc_ok);
    assert_eq!(header.version, 0x300);
    assert_eq!(header.sample_rate, 48000);
    assert_eq!(header.channels, w.channels);
    let pcm = hca::decode(&hca).unwrap();
    assert_eq!(pcm.sample_rate, 48000);
    assert_eq!(pcm.channels as u8, w.channels);
    assert_eq!(pcm.frame_count() as u32, w.num_samples, "ACB NumSamples == decoded frames");
    let peak = pcm.samples.iter().map(|s| (*s as i32).abs()).max().unwrap();
    assert!(peak > 1000, "silent output (peak {peak})");
}

#[test]
fn waza_stream_and_voice_bank_parse() {
    let Some(dir) = asset_dir() else { eprintln!("skip: no sound_asset dump"); return };
    let bytes = read(&dir.join("waza_stream.acb"));
    let acb = Acb::parse(&bytes).unwrap();
    assert!(acb.cues.len() > 1000, "waza_stream cues: {}", acb.cues.len());
    let me = acb.cue("ev60_00010_me").expect("ev60_00010_me");
    assert!(!me.waveforms.is_empty());
    let hca = payload(&dir, "waza_stream", &bytes, &acb, me.waveforms[0]);
    let pcm = hca::decode(&hca).unwrap();
    assert_eq!(pcm.frame_count() as u32, acb.waveforms[me.waveforms[0]].num_samples);

    // Mark Evans' Victory Road bank: generic lines + move shouts (`_whs#####` / `_whk#####`).
    let bytes = read(&dir.join("ja/c01000010.acb"));
    let acb = Acb::parse(&bytes).unwrap();
    assert_eq!(acb.name, "c01000010");
    let shouts: Vec<&str> = acb.cues.iter().filter(|c| c.name.contains("_wh")).map(|c| c.name.as_str()).collect();
    assert!(shouts.len() > 10, "move shouts in c01000010: {shouts:?}");
    let goal = acb.cue("c01000010_gl010").expect("gl010");
    let hca = payload(&dir, "ja/c01000010", &bytes, &acb, goal.waveforms[0]);
    let header = Header::parse(&hca).unwrap();
    assert_eq!(header.channels, 1);
    let pcm = hca::decode(&hca).unwrap();
    assert_eq!(pcm.frame_count() as u32, acb.waveforms[goal.waveforms[0]].num_samples);
}

#[test]
fn memory_awb_and_loop_points() {
    let Some(dir) = asset_dir() else { eprintln!("skip: no sound_asset dump"); return };
    // `common.acb` keeps some waveforms in its memory AWB.
    let bytes = read(&dir.join("common.acb"));
    let acb = Acb::parse(&bytes).unwrap();
    assert!(acb.memory_awb.is_some(), "common.acb has no memory AWB");
    let mem = acb.waveforms.iter().find(|w| !w.is_streamed()).expect("a memory waveform");
    let hca = payload(&dir, "common", &bytes, &acb, mem.index);
    assert!(Header::is_hca(&hca));
    let pcm = hca::decode(&hca).unwrap();
    assert_eq!(pcm.frame_count() as u32, mem.num_samples);

    // `bgm.acb`: looping BGM carries loop points in both the ACB and the HCA.
    let bytes = read(&dir.join("bgm.acb"));
    let acb = Acb::parse(&bytes).unwrap();
    let looped = acb.waveforms.iter().find(|w| w.loop_range.is_some()).expect("a looping bgm waveform");
    let hca = payload(&dir, "bgm", &bytes, &acb, looped.index);
    let header = Header::parse(&hca).unwrap();
    let (s, e) = header.loop_samples().expect("HCA loop chunk");
    let (acb_s, acb_e) = looped.loop_range.unwrap();
    assert!(s <= acb_s as u64 + 1024 && e + 1024 >= acb_e as u64, "loop mismatch: hca {s}-{e}, acb {acb_s}-{acb_e}");
}

#[test]
fn every_global_bank_parses_and_first_waveform_decodes() {
    let Some(dir) = asset_dir() else { eprintln!("skip: no sound_asset dump"); return };
    let mut report = Vec::new();
    for entry in std::fs::read_dir(&dir).unwrap().flatten() {
        let p = entry.path();
        if p.extension().is_none_or(|e| e != "acb") {
            continue;
        }
        let stem = p.file_stem().unwrap().to_string_lossy().to_string();
        let bytes = read(&p);
        let acb = Acb::parse(&bytes).unwrap_or_else(|e| panic!("{stem}: {e}"));
        let Some(w) = acb.waveforms.first() else { continue };
        let hca = payload(&dir, &stem, &bytes, &acb, w.index);
        let header = Header::parse(&hca).unwrap_or_else(|e| panic!("{stem}: {e}"));
        let pcm = hca::decode(&hca).unwrap_or_else(|e| panic!("{stem}: {e}"));
        report.push(format!("{stem}: v{:x} {} cues, {} ch {} Hz, {} samples (acb {}), ath {} loop {}", header.version, acb.cues.len(), header.channels, header.sample_rate, pcm.frame_count(), w.num_samples, header.ath_type, header.loop_info.is_some()));
        // Looping waveforms: the ACB counts up to the loop end (the tail after it never plays).
        let expect = header.loop_samples().map_or(pcm.frame_count() as u64, |(_, e)| e);
        assert_eq!(expect as u32, w.num_samples, "{stem}");
    }
    for l in &report {
        println!("{l}");
    }
    assert!(report.len() >= 10);
}

#[test]
fn reference_pcm_matches() {
    let Some(dir) = std::env::var("CRI_REF_DIR").ok().map(PathBuf::from).filter(|p| p.is_dir()) else { eprintln!("skip: CRI_REF_DIR not set"); return };
    let mut checked = 0;
    for entry in std::fs::read_dir(&dir).unwrap().flatten() {
        let p = entry.path();
        if p.extension().is_none_or(|e| e != "hca") {
            continue;
        }
        let stem = p.file_stem().unwrap().to_string_lossy().to_string();
        let refp = dir.join(format!("{stem}.ref.wav"));
        if !refp.is_file() {
            continue;
        }
        let bytes = read(&p);
        let reference_valid = Header::parse(&bytes).unwrap_or_else(|e| panic!("{stem}: {e}")).min_resolution >= 1;
        let pcm = hca::decode(&bytes).unwrap_or_else(|e| panic!("{stem}: {e}"));
        let (ch, rate, samples) = cri::wav::read(&read(&refp)).unwrap();
        assert_eq!((ch, rate), (pcm.channels, pcm.sample_rate), "{stem}");
        // The reference decoder cuts looping files at the loop end; we keep the whole payload.
        assert!(samples.len() <= pcm.samples.len(), "{stem}: reference has more samples ({} > {})", samples.len(), pcm.samples.len());
        let mut max_diff = 0i32;
        let mut over2 = 0usize;
        for (a, b) in samples.iter().zip(&pcm.samples) {
            let d = (*a as i32 - *b as i32).abs();
            max_diff = max_diff.max(d);
            if d > 2 {
                over2 += 1;
            }
        }
        println!("{stem}: {} samples, max diff {max_diff}, >2 LSB: {over2} ({:.4}%){}", samples.len(), 100.0 * over2 as f64 / samples.len().max(1) as f64, if reference_valid { "" } else { " [min_res 0: reference not valid, not asserted]" });
        if !reference_valid {
            continue;
        }
        assert!(over2 as f64 / (samples.len().max(1) as f64) < 0.001, "{stem}: too many differing samples");
        checked += 1;
    }
    assert!(checked > 0, "no reference pairs in {}", dir.display());
}

/// Regression for "every cue sounds garbled" (docs/game/media/audio-browser.md §5): the game's HCA 3.0
/// streams (`min_res` 0) have bands whose resolution curve position is past the table (> 65 in clHCA
/// terms). Those carry no coded value (resolution 0 → noise band); the old decoder clamped them to
/// resolution 1 and read 1–2 phantom bits per band, so from the first such band on the frame was parsed
/// out of sync (2959 of 2962 frames of `bg00010` read past their payload). A correct parse ends within a
/// few bits of the frame's CRC, never past it.
#[test]
fn v3_frames_parse_within_their_payload() {
    let Some(dir) = asset_dir() else { eprintln!("skip: no sound_asset dump"); return };
    // (bank, cue or "" for the first waveform): stereo BGM with loop, mono voice, technique SE, stereo
    // `channel_config` 0, the only HFR layout of the game (`ch`, base 96 bands + 8 HFR groups) and the v2.0 bank.
    let cases = [("bgm_title", "bg00010"), ("ja/c01000010", "c01000010_gl010"), ("waza_stream", "ev60_00010_me"), ("anime_stream", ""), ("ch", ""), ("d00010000", "")];
    for (bank, cue) in cases {
        let bytes = read(&dir.join(format!("{bank}.acb")));
        let acb = Acb::parse(&bytes).unwrap();
        let w = if cue.is_empty() { 0 } else { acb.cue(cue).unwrap_or_else(|| panic!("{bank}/{cue}")).waveforms[0] };
        let hca = payload(&dir, bank, &bytes, &acb, w);
        let header = Header::parse(&hca).unwrap();
        assert!(header.crc_ok, "{bank}: header CRC");
        assert_eq!(header.cipher_type, 0, "{bank}: the game ships no encrypted HCA");
        let mut dec = hca::Decoder::new(header.clone()).unwrap();
        let fs = header.frame_size as usize;
        let mut p = header.header_size as usize;
        let (mut sum_sq, mut n, mut peak, mut clipped) = (0f64, 0usize, 0f32, 0usize);
        for f in 0..header.frame_count {
            let frame = &hca[p..p + fs];
            p += fs;
            for wave in dec.decode_frame(frame).unwrap_or_else(|e| panic!("{bank} frame {f}: {e}")) {
                for &s in wave {
                    assert!(s.is_finite(), "{bank} frame {f}: non-finite sample");
                    sum_sq += (s as f64) * (s as f64);
                    peak = peak.max(s.abs());
                    clipped += usize::from(s.abs() >= 1.0);
                    n += 1;
                }
            }
        }
        let rms = (sum_sq / n.max(1) as f64).sqrt() * 32767.0;
        println!(
            "{bank}/{}: v{:x} {} ch {} Hz min_res {} bands {}+{}+hfr {}x{}: {} frames, overrun {}, bad crc {}, rms {rms:.0}, peak {:.0}, clipped {clipped}",
            if cue.is_empty() { "w0" } else { cue },
            header.version, header.channels, header.sample_rate, header.min_resolution, header.base_band_count, header.stereo_band_count, header.hfr_group_count(), header.bands_per_hfr_group,
            header.frame_count, dec.overrun_frames, dec.bad_crc_frames, peak * 32767.0
        );
        assert_eq!(dec.bad_crc_frames, 0, "{bank}: frame CRC");
        assert_eq!(dec.overrun_frames, 0, "{bank}: frames parsed past their payload (desynchronized bitstream)");
        assert!((20.0..20000.0).contains(&rms), "{bank}: rms {rms}");
        assert!(peak > 0.02 && peak < 2.0, "{bank}: peak {peak}");
        assert!((clipped as f64) < n as f64 * 1e-3, "{bank}: {clipped} of {n} samples clip");
        let pcm = hca::decode(&hca).unwrap();
        assert_eq!(pcm.frame_count() as u64, header.sample_count(), "{bank}: decoded frames == header's");
        assert_eq!(pcm.channels as u8, header.channels);
    }
}
