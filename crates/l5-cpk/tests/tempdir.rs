//! Build a small CPK into a temp dir with the file-based API and reopen it (no game files needed).

use l5_cpk::{CpkArchive, CpkBuilder, key_for_name};

#[test]
fn build_encrypted_cpk_in_tempdir_and_reopen() {
    let tmp = tempfile::tempdir().unwrap();
    let src = tmp.path().join("voice.awb");
    let awb: Vec<u8> = b"AFS2".iter().copied().chain((0..100_000u32).map(|i| (i % 97) as u8)).collect();
    std::fs::write(&src, &awb).unwrap();
    let acb = b"@UTF".iter().copied().chain(std::iter::repeat_n(0x55, 3000)).collect::<Vec<u8>>();

    let mut b = CpkBuilder::new();
    b.add_file("data/common/sound_asset/ja/c01000120.awb", &src)
        .add_bytes("data/common/sound_asset/ja/c01000120.acb", acb.clone());
    let out = tmp.path().join("5a460c051bb0b0d02ed03ea7184347e1.cpk");
    b.write_file(&out, true).unwrap();

    let raw = std::fs::read(&out).unwrap();
    assert_ne!(&raw[..4], b"CPK ", "output must be XOR-encrypted");

    let mut cpk = CpkArchive::open(&out).unwrap();
    assert_eq!(cpk.key(), Some(key_for_name("5a460c051bb0b0d02ed03ea7184347e1.cpk")));
    assert_eq!(cpk.entries().len(), 2);
    assert_eq!(cpk.extract_path("data/common/sound_asset/ja/c01000120.awb").unwrap(), awb);
    assert_eq!(cpk.extract_path("data/common/sound_asset/ja/c01000120.acb").unwrap(), acb);
    // Streaming extraction.
    let e = cpk.find("data/common/sound_asset/ja/c01000120.awb").unwrap().clone();
    let mut sink = Vec::new();
    assert_eq!(cpk.extract_to(&e, &mut sink).unwrap(), awb.len() as u64);
    assert_eq!(sink, awb);
    assert!(cpk.extract_path("nope").is_err());
}
