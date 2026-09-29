//! Extract one cue's audio to a WAV file, reading only the needed AWB byte range (no repo files modified).
//! `cargo run -p cri --example cueextract -- <bank.acb> <bank.awb> <cue_name|@wN> <out.wav>`
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};

use cri::acb::{Acb, Location};
use cri::hca;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let (acb_path, awb_path, cue_sel, out_path) = match &args[..] {
        [a, b, c, d] => (a, b, c, d),
        _ => {
            eprintln!("usage: cueextract <bank.acb> <bank.awb> <cue_name|@wN> <out.wav>");
            std::process::exit(1);
        }
    };
    let acb_bytes = std::fs::read(acb_path).expect("read acb");
    let acb = Acb::parse(&acb_bytes).expect("parse acb");

    let waveform_idx = if let Some(n) = cue_sel.strip_prefix("@w") {
        n.parse::<usize>().expect("waveform index")
    } else {
        let c = acb.cue(cue_sel).unwrap_or_else(|| panic!("cue {cue_sel} not found"));
        *c.waveforms.first().expect("cue has no waveform (control cue)")
    };
    let w = &acb.waveforms[waveform_idx];
    let loc = acb.locate(w).expect("locate waveform");
    let hca_bytes = match loc {
        Location::Memory { start, end } => acb_bytes[start..end].to_vec(),
        Location::Stream { start, end } => {
            let mut f = File::open(awb_path).expect("open awb");
            f.seek(SeekFrom::Start(start)).expect("seek");
            let mut buf = vec![0u8; (end - start) as usize];
            f.read_exact(&mut buf).expect("read range");
            buf
        }
    };
    assert!(hca::Header::is_hca(&hca_bytes), "waveform {waveform_idx} is not HCA (encode_type {})", w.encode_type);
    let mut pcm = hca::decode(&hca_bytes).expect("decode hca");
    if let Some((s, e)) = w.loop_range {
        pcm.loop_range = Some((s as u64, e as u64));
    }
    std::fs::write(out_path, cri::wav::write(&pcm)).expect("write wav");
    println!("wrote {out_path}: {} samples @ {} Hz, {} ch, loop {:?}", pcm.frame_count(), pcm.sample_rate, pcm.channels, pcm.loop_range);
}
