//! Integration tests against a real IEVR v7.1.2 install and a Viola dump. All `#[ignore]`; run with
//! `cargo test -p l5-cpk -- --ignored --nocapture`.
//!
//! The install is only ever **read**; everything written goes to a temp dir or memory.
//! Override locations with `IEVR_DATA` (the install's `data` folder) and `IEVR_DUMP` (the folder that
//! contains the dumped `data/` tree).

use std::fs;
use std::io::{Cursor, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use l5_cpk::{CpkArchive, CpkBuilder, CpkEntry, Encryption, UtfTable, aes_list, key_for_name};

fn install_data() -> PathBuf {
    std::env::var_os("IEVR_DATA")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(r"D:\SteamLibrary\steamapps\common\INAZUMA ELEVEN Victory Road\data"))
}

fn dump_root() -> PathBuf {
    std::env::var_os("IEVR_DUMP").map(PathBuf::from).unwrap_or_else(|| {
        PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/../../assets/v7.1.2"))
    })
}

fn sorted_cpks(dir: &Path) -> Vec<PathBuf> {
    let mut v: Vec<PathBuf> = fs::read_dir(dir)
        .unwrap()
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|e| e == "cpk"))
        .collect();
    v.sort();
    v
}

#[test]
#[ignore]
fn cpk_list_decrypts_to_t2b_and_roundtrips() {
    for path in [install_data().join("cpk_list.cfg.bin"), dump_root().join("data").join("cpk_list.cfg.bin")] {
        let enc = fs::read(&path).unwrap();
        let plain = aes_list::decrypt(&enc).unwrap();
        assert!(aes_list::has_t2b_footer(&plain), "{}", path.display());
        let entry_count = u32::from_le_bytes(plain[0..4].try_into().unwrap());
        println!("{}: {} B enc, {} B plain, header entryCount {}", path.display(), enc.len(), plain.len(), entry_count);
        assert_eq!(aes_list::encrypt(&plain), enc, "encrypt(decrypt(x)) != x");
        assert_eq!(aes_list::decode(&enc).unwrap().1, aes_list::ListEncoding::Aes);
    }
}

/// Pick up to `n` entries spread over the archive, preferring a mix of compressed and raw ones and, when the
/// archive is larger than 4 GiB, entries stored past the 4 GiB mark (exercises 64-bit XOR offsets).
fn pick(entries: &[CpkEntry], n: usize, cpk_len: u64) -> Vec<CpkEntry> {
    let mut out: Vec<CpkEntry> = Vec::new();
    let small: Vec<&CpkEntry> = entries.iter().filter(|e| e.extract_size < 64 << 20).collect();
    if cpk_len > 1 << 32 {
        out.extend(small.iter().filter(|e| e.offset > 1 << 32).take(2).map(|e| (*e).clone()));
    }
    if let Some(c) = small.iter().find(|e| e.is_compressed()) {
        out.push((*c).clone());
    }
    if let Some(r) = small.iter().find(|e| !e.is_compressed()) {
        out.push((*r).clone());
    }
    let step = (small.len() / n.max(1)).max(1);
    for e in small.iter().step_by(step) {
        if out.len() >= n {
            break;
        }
        if !out.iter().any(|o| o.toc_index == e.toc_index) {
            out.push((*e).clone());
        }
    }
    out.truncate(n);
    out
}

#[test]
#[ignore]
fn open_list_extract_and_compare_with_dump() {
    let packs = sorted_cpks(&install_data().join("packs"));
    assert!(packs.len() > 100);
    // The documented sample, the two largest (> 4 GiB and ~4 GB), and two spread across the list.
    let mut chosen: Vec<PathBuf> = vec![install_data().join("packs").join("e832856918ebb97cb4430f715e6bd525.cpk")];
    let mut by_size = packs.clone();
    by_size.sort_by_key(|p| std::cmp::Reverse(fs::metadata(p).map(|m| m.len()).unwrap_or(0)));
    chosen.extend(by_size.iter().take(2).cloned());
    chosen.push(packs[packs.len() / 3].clone());
    chosen.push(packs[2 * packs.len() / 3].clone());

    let (mut compared, mut compressed, mut missing, mut total_files, mut ranges, mut beyond_4g) = (0, 0, 0, 0, 0, 0);
    for path in &chosen {
        let t = std::time::Instant::now();
        let mut cpk = CpkArchive::open(path).unwrap();
        let name = path.file_name().unwrap().to_string_lossy().into_owned();
        assert_eq!(cpk.key(), Some(key_for_name(&name)));
        let entries = cpk.entries().to_vec();
        total_files += entries.len();
        let itoc = cpk.itoc().unwrap().map(|t| t.rows.len());
        let etoc = cpk.etoc().unwrap().map(|t| t.rows.len());
        println!(
            "{name}: {} B, {} files, ITOC rows {itoc:?}, ETOC rows {etoc:?}, opened in {:?}",
            cpk.len(),
            entries.len(),
            t.elapsed()
        );
        let mut ids: Vec<u32> = entries.iter().map(|e| e.id).collect();
        ids.sort_unstable();
        assert!(ids.iter().enumerate().all(|(i, &id)| id as usize == i), "IDs are a permutation of 0..n");
        for e in &entries {
            assert!(e.offset + e.size <= cpk.len());
        }
        // Huge raw entries (multi-GB AWB/USM): compare 1 MiB at the start and at the end, straight from the
        // encrypted file, so offsets past 4 GiB are exercised without extracting gigabytes.
        for e in entries.iter().filter(|e| e.size >= 64 << 20 && !e.is_compressed()) {
            let Ok(mut f) = fs::File::open(dump_root().join(e.path())) else { continue };
            for rel in [0, e.size - (1 << 20)] {
                let got = cpk.read_at(e.offset + rel, 1 << 20).unwrap();
                let mut want = vec![0u8; 1 << 20];
                f.seek(SeekFrom::Start(rel)).unwrap();
                f.read_exact(&mut want).unwrap();
                assert!(got == want, "{} @+{rel:#x} differs", e.path());
            }
            ranges += 1;
            beyond_4g += (e.offset + e.size > 1 << 32) as usize;
            println!("   ok {} (1 MiB head+tail of {} B, stored @{:#x}..{:#x})", e.path(), e.size, e.offset, e.offset + e.size);
        }
        for e in pick(&entries, 8, cpk.len()) {
            let data = cpk.extract(&e).unwrap();
            assert_eq!(data.len() as u64, e.extract_size);
            let loose = dump_root().join(e.path());
            match fs::read(&loose) {
                Ok(expected) => {
                    assert!(data == expected, "{} differs from the dump", e.path());
                    compared += 1;
                    compressed += e.is_compressed() as usize;
                    println!(
                        "   ok {} ({} -> {} B{}, @{:#x})",
                        e.path(),
                        e.size,
                        e.extract_size,
                        if e.is_compressed() { ", CRILAYLA" } else { "" },
                        e.offset
                    );
                }
                Err(_) => {
                    missing += 1;
                    println!("   (not in dump) {}", e.path());
                }
            }
        }
    }
    println!(
        "{total_files} files listed; {compared} extracted files byte-identical to the dump ({compressed} CRILAYLA), \
         {missing} not in dump; {ranges} huge entries range-checked ({beyond_4g} ending past 4 GiB)"
    );
    assert!(beyond_4g >= 1, "no entry past 4 GiB was checked");
    assert!(compared >= 25, "only {compared} files compared");
    assert!(compressed >= 5);
}

/// Decompress every CRILAYLA entry of the sample CPK, and check the TOC's @UTF table survives a write/read.
#[test]
#[ignore]
fn sample_cpk_full_decompress_and_utf_roundtrip() {
    let path = install_data().join("packs").join("e832856918ebb97cb4430f715e6bd525.cpk");
    let mut cpk = CpkArchive::open(&path).unwrap();
    let entries = cpk.entries().to_vec();
    let mut n = 0;
    for e in entries.iter().filter(|e| e.is_compressed()) {
        let out = cpk.extract(e).unwrap();
        assert_eq!(out.len() as u64, e.extract_size);
        n += 1;
    }
    println!("{n}/{} entries CRILAYLA-decompressed with matching sizes", entries.len());

    // Our compressor on real data: round-trips, and compare the size with CRI's.
    let (mut ours, mut theirs, mut raw_total) = (0u64, 0u64, 0u64);
    for e in entries.iter().filter(|e| e.is_compressed()).step_by(20) {
        let plain = cpk.extract(e).unwrap();
        let z = l5_cpk::crilayla::compress(&plain).unwrap();
        assert_eq!(l5_cpk::crilayla::decompress(&z).unwrap(), plain, "{}", e.path());
        ours += z.len() as u64;
        theirs += e.size;
        raw_total += e.extract_size;
    }
    println!("compressor: {raw_total} B -> ours {ours} B vs CRI {theirs} B");

    // Every retail table survives parse -> write -> parse byte-identically.
    let header = cpk.header().clone();
    for (col, magic) in [(None, *b"CPK "), (Some("TocOffset"), *b"TOC "), (Some("ItocOffset"), *b"ITOC"), (Some("EtocOffset"), *b"ETOC")] {
        let off = col.map_or(0, |c| header.get_u64(0, c).unwrap());
        let (h, table) = cpk.read_packet(off).unwrap();
        assert_eq!(h.magic, magic);
        let bytes = table.to_bytes().unwrap();
        assert_eq!(UtfTable::parse(&bytes).unwrap(), table);
        let raw = cpk.read_at(off + 16, h.size).unwrap();
        assert_eq!(table.seed_strings, ["<NULL>"]);
        let identical = bytes == raw;
        println!("{} ({}, {} rows, {} B): byte-identical re-serialisation: {identical}", String::from_utf8_lossy(&magic), table.name, table.rows.len(), raw.len());
        assert!(identical);
    }
}

/// Viola wrote the CPKs in `data/packs_custom`; rebuilding them from their own entries with our writer must give the
/// exact same encrypted bytes.
#[test]
#[ignore]
fn viola_packs_custom_rebuild_is_byte_identical() {
    let dir = install_data().join("packs_custom");
    let cpks = sorted_cpks(&dir);
    assert!(!cpks.is_empty());
    for path in cpks {
        let original = fs::read(&path).unwrap();
        let name = path.file_name().unwrap().to_string_lossy().into_owned();
        let mut cpk = CpkArchive::from_reader(Cursor::new(original.clone()), &name).unwrap();
        let builder = CpkBuilder::from_archive(&cpk);
        let mut rebuilt = Vec::new();
        builder.write_with_archive(&mut cpk, &mut rebuilt, Some(key_for_name(&name))).unwrap();
        println!("{name}: {} files, {} B, identical: {}", cpk.entries().len(), original.len(), rebuilt == original);
        assert!(rebuilt == original, "{name}: rebuilt CPK differs");
    }
}

/// Build a small encrypted CPK from real extracted files into a temp dir and reopen it.
#[test]
#[ignore]
fn build_small_cpk_from_real_files_and_reopen() {
    let mut src = CpkArchive::open(install_data().join("packs").join("e832856918ebb97cb4430f715e6bd525.cpk")).unwrap();
    let picks: Vec<CpkEntry> = src.entries().iter().step_by(200).take(6).cloned().collect();
    let tmp = tempfile::tempdir().unwrap();
    let mut b = CpkBuilder::new();
    let mut expected = Vec::new();
    for e in &picks {
        let data = src.extract(e).unwrap();
        b.add_bytes(e.path(), data.clone());
        expected.push((e.path(), data));
    }
    for compress in [false, true] {
        let out = tmp.path().join(if compress { "packed_z.cpk" } else { "packed.cpk" });
        b.compress(compress);
        b.write_file(&out, true).unwrap();
        let mut back = CpkArchive::open(&out).unwrap();
        assert!(matches!(back.key(), Some(k) if k == key_for_name(out.file_name().unwrap().to_str().unwrap())));
        for (p, d) in &expected {
            assert_eq!(&back.extract_path(p).unwrap(), d, "{p}");
        }
        println!("{}: {} B, {} files, compressed entries {}", out.display(), back.len(), back.entries().len(),
            back.entries().iter().filter(|e| e.is_compressed()).count());
        // Plain open must fail on an encrypted file when told it is plain.
        assert!(CpkArchive::with_encryption(fs::File::open(&out).unwrap(), Encryption::Plain).is_err());
    }
}

/// Retail loose USMs are XOR-encrypted with their own name; detection finds the key and yields `CRID`.
#[test]
#[ignore]
fn loose_usm_detects_and_decrypts() {
    let path = install_data().join("dx11").join("movie").join("L5logo.usm");
    let mut data = fs::read(&path).unwrap();
    let enc = l5_cpk::xor::detect("L5logo.usm", &data[..16]).unwrap();
    assert_eq!(enc, Encryption::Xor(key_for_name("L5logo.usm")));
    let t = std::time::Instant::now();
    l5_cpk::xor::decrypt_range(enc.key().unwrap(), 0, &mut data);
    let dt = t.elapsed();
    assert_eq!(&data[..4], b"CRID");
    println!("L5logo.usm: {} B decrypted in {dt:?} ({:.0} MB/s)", data.len(), data.len() as f64 / dt.as_secs_f64() / 1e6);
}
