//! Dump a cue sheet or an HCA header: `cargo run -p cri --example cridump -- <file.acb | file.hca | file.awb> [out.wav]`.
//! With an `.hca` and an output path, decodes the file to WAV.

use std::path::Path;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let Some(path) = args.first() else {
        eprintln!("usage: cridump <file.acb|file.hca|file.awb> [out.wav]");
        return;
    };
    let bytes = std::fs::read(path).expect("read");
    let ext = Path::new(path).extension().and_then(|e| e.to_str()).unwrap_or("").to_lowercase();
    if ext == "acb" || cri::utf::Table::is_utf(&bytes) {
        let acb = cri::acb::Acb::parse(&bytes).expect("acb");
        println!("sheet {} version 0x{:X}: {} cues, {} waveforms, memory awb {}, stream awb entries {}", acb.name, acb.version, acb.cues.len(), acb.waveforms.len(), acb.memory_awb.is_some(), acb.stream_awb.as_ref().map_or(0, |a| a.entries.len()));
        for (name, md5) in &acb.stream_awb_hash {
            println!("  stream awb {name} md5 {}", md5.iter().map(|b| format!("{b:02x}")).collect::<String>());
        }
        for c in &acb.cues {
            let w: Vec<String> = c
                .waveforms
                .iter()
                .map(|&i| {
                    let w = &acb.waveforms[i];
                    format!("w{i}(id {} {} {}ch {}Hz {} smp{})", if w.is_streamed() { w.stream_awb_id } else { w.memory_awb_id }, if w.is_streamed() { "stream" } else { "memory" }, w.channels, w.sampling_rate, w.num_samples, w.loop_range.map(|(s, e)| format!(" loop {s}-{e}")).unwrap_or_default())
                })
                .collect();
            println!("  cue {:4} {:<32} id {:<5} type {} len {:>7} ms  {}", c.index, c.name, c.id, c.reference_type, c.length_ms, w.join(" "));
        }
    } else if ext == "awb" || cri::awb::Afs2::is_afs2(&bytes) {
        let a = cri::awb::Afs2::parse(&bytes).expect("afs2");
        println!("AFS2 v{} offset {} id {} align {} subkey {} entries {} header {} B total {}", a.version, a.offset_size, a.id_size, a.alignment, a.subkey, a.entries.len(), a.header_len, a.total_len);
        for e in a.entries.iter().take(20) {
            println!("  id {:5} {:>10}..{:<10} ({} B)", e.id, e.start, e.end, e.end - e.start);
        }
    } else {
        let h = cri::hca::Header::parse(&bytes).expect("hca header");
        println!("{h:#?}");
        println!("samples {} loop {:?} hfr groups {}", h.sample_count(), h.loop_samples(), h.hfr_group_count());
        if let Some(out) = args.get(1) {
            let pcm = cri::hca::decode(&bytes).expect("decode");
            std::fs::write(out, cri::wav::write(&pcm)).expect("write");
            println!("wrote {out}: {} frames", pcm.frame_count());
        }
    }
}
