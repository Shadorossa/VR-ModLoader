//! `cargo run -p cri --example bank_replace -- <bank> in.acb in.awb <cue> new.hca out.acb out.awb` (plain files): the
//! Rust bank editor (`cri::bank`), for byte comparisons with research/scripts/voice_bank_edit.py (`Bank.replace`,
//! `Bank.add` when the cue is new).

fn main() {
    let a: Vec<String> = std::env::args().collect();
    if a.len() < 8 {
        eprintln!("usage: bank_replace <bank> in.acb in.awb <cue> new.hca out.acb out.awb");
        std::process::exit(2);
    }
    let acb = std::fs::read(&a[2]).unwrap();
    let awb = std::fs::read(&a[3]).unwrap();
    let hca = std::fs::read(&a[5]).unwrap();
    let mut b = cri::bank::Bank::open(&a[1], &acb, Some(&awb)).unwrap();
    let e = if b.has_cue(&a[4]) {
        b.replace(&a[4], hca).unwrap()
    } else {
        let t = b.pick_template(&a[4]).unwrap();
        b.add(&a[4], hca, t).unwrap()
    };
    let mut out = std::fs::File::create(&a[7]).unwrap();
    let (acb2, _) = b.finish(Some(&mut std::io::Cursor::new(awb)), Some(&mut out), None).unwrap();
    std::fs::write(&a[6], acb2).unwrap();
    println!("{e:?}");
}
