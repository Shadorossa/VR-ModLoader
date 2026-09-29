//! `cargo run -p cri --example hca_encode -- in.wav out.hca [frame_size] [loop_start loop_end]`: the Rust HCA encoder
//! (`cri::hca_enc`) on a 16-bit PCM WAV, for checks against other decoders (tools/hcaenc `decode`, vgmstream).

fn main() {
    let a: Vec<String> = std::env::args().collect();
    if a.len() < 3 {
        eprintln!("usage: hca_encode in.wav out.hca [frame_size] [loop_start loop_end]");
        std::process::exit(2);
    }
    let d = std::fs::read(&a[1]).expect("read wav");
    let (ch, rate, pcm) = cri::wav::read(&d).expect("16-bit PCM WAV");
    let frame_size = a.get(3).and_then(|s| s.parse().ok()).unwrap_or(0);
    let loop_range = match (a.get(4).and_then(|s| s.parse().ok()), a.get(5).and_then(|s| s.parse().ok())) {
        (Some(s), Some(e)) => Some((s, e)),
        _ => None,
    };
    let t = std::time::Instant::now();
    let out = cri::hca_enc::encode(&pcm, ch, rate, cri::hca_enc::Options { frame_size, loop_range }).expect("encode");
    std::fs::write(&a[2], &out).expect("write");
    println!("{} ch {} Hz {} samples -> {} B in {} ms", ch, rate, pcm.len() / ch as usize, out.len(), t.elapsed().as_millis());
}
