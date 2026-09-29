# l5-cpk

CRI CPK archives and the Level-5 encryption layers around them, as used by *Inazuma Eleven: Victory Road* (PC).
The format specs are not published yet; the source is the reference.

| Module | Contents |
|---|---|
| `xor` | Filename-XOR stream cipher (`key = crc32(basename)`), seekable: `crypt_in_place` / `decrypt_range(key, offset, buf)`, `key_for_name`, `detect` / `detect_with`, `XorReader` (Read+Seek) and `XorWriter` |
| `aes_list` | `cpk_list.cfg.bin` AES-256-CBC/PKCS#7: `decrypt`, `encrypt`, `decode`/`encode` (plain / AES / legacy XOR), `has_t2b_footer`, de-obfuscated `aes_key`/`aes_iv`. Returns T2B bytes only (parsing is another crate's job). |
| `utf` | `@UTF` tables: `UtfTable::parse` (all 12 types, zero/constant/per-row storage, string + data pools, masked tables) and `UtfTable::to_bytes` |
| `crilayla` | `decompress`, `compress` (greedy hash-chain; round-trips; within 0.2% of CRI's sizes) |
| `cpk` | `CpkArchive::open(path)` / `from_reader(r, name)` / `with_encryption(r, enc)`; `entries()` (`dir, name, offset, size, extract_size, id`), `find`, `extract`, `extract_to` (streams large raw files), `read_raw`, `copy_raw_to`, `header`/`toc`/`itoc`/`etoc`, `read_packet` |
| `writer` | `CpkBuilder`: `add_bytes` / `add_file` / `add(path, CpkSource)`, `from_archive` + `replace` (rebuild with replacements, like Viola's audio re-pack), optional `compress`, `write(out, key)`, `write_file(path, encrypt)`, `write_with_archive` |

```rust
use l5_cpk::{CpkArchive, CpkBuilder, aes_list};

// Read (the file is XOR-decrypted on the fly; a 4.5 GB CPK is never loaded).
let mut cpk = CpkArchive::open(r"...\data\packs\e832856918ebb97cb4430f715e6bd525.cpk")?;
let e = cpk.find("data/common/chr/_uniform/u11010010/u11010010_p200.g4pk").unwrap().clone();
let bytes = cpk.extract(&e)?;                       // CRILAYLA-decompressed

// Build an encrypted packs_custom CPK (key = crc32 of the output file name).
let mut b = CpkBuilder::new();
b.add_file("data/common/sound_asset/ja/c01000120.acb", "edited.acb");
b.write_file(r"out\data\packs_custom\5a460c051bb0b0d02ed03ea7184347e1.cpk", true)?;

// cpk_list
let plain = aes_list::decrypt(&std::fs::read("cpk_list.cfg.bin")?)?;
```

## Writer layout

Identical to Viola's `CCriCpkWriter`: CPK packet, content at 0x800 (sorted case-insensitively, 0x800-aligned), TOC,
ETOC, no ITOC, `Tvers = "N/A, DLL3.11.05"`, whole file XOR-encrypted. Two deliberate differences: entries copied raw
from an existing archive keep their real `ExtractSize` (Viola writes the stored size), and CRILAYLA compression is
available but off by default.

## Tests

```
cargo test -p l5-cpk                                   # unit + temp-dir tests, no game files needed
cargo test -p l5-cpk --release -- --ignored --nocapture # real install, read-only
```

Ignored tests read `D:\SteamLibrary\steamapps\common\INAZUMA ELEVEN Victory Road\data` and the dump in
`<repo>ssets7.1.2` (your own extracted copy; override with `IEVR_DATA` / `IEVR_DUMP`). They check: both cpk_lists decrypt to T2B
and `encrypt(decrypt(x)) == x`; 5 CPKs (incl. the 4.58 GB one) list and extract byte-identically to the dump,
including reads past 4 GiB; all 1,412 CRILAYLA entries of the sample CPK decompress; retail CPK/TOC/ITOC/ETOC tables
re-serialise byte-identically; the 3 Viola-written `packs_custom` CPKs are rebuilt byte-identically; loose
`L5logo.usm` is detected and decrypted.

## Gaps

- ITOC-only CPKs (no `TOC`, no file names) are rejected; IEVR always has a TOC.
- `GTOC`/`HTOC`/`HGTOC` packets are not parsed (not used by IEVR); `read_packet` can read them raw.
- Game acceptance of self-compressed CRILAYLA entries is untested (Viola and retail audio CPKs store raw).
- CRC columns (`TocCrc`, `EnableFileCrc`, ...) are neither checked nor written (all zero in IEVR).
