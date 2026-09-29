# l5-core

Level-5 core formats for *Inazuma Eleven: Victory Road* (PC): CRC-32 name hashes and
byte-exact **T2B** and **RDBN** `cfg.bin` read/write. Rust port of the project's reference Python reader / writer.

The format specs are not published yet; the source is the reference.

## API

| Module | Main items |
|---|---|
| `hash` | `crc32`, `crc32_str`, `crc32_i32`, `crc32_const`, `jamcrc`, `to_i32`/`to_u32`, `INVALID`/`NONE` (+ `_I32`) |
| `t2b` | `T2b::parse` / `to_bytes`, `Entry { name, hash, values }`, `Value::{Int, Float, String}`, `build_tree` / `T2b::tree` (`Node`, `TreeMode`), `sorted_lists`, `rebuild_sort_indexes[_with]`, `new_entry`, `rename_entry`, `key_table` |
| `rdbn` | `Rdbn::parse` / `to_bytes`, `Type` / `Field` / `List` / `Row`, `Value` (typed per field type), `layout`, `Rdbn::sort_index`, `Rdbn::table` / `tables` (named JSON view) |
| `detect` | `detect(&[u8]) -> Option<FileKind>`, `CfgBin::{T2b, Rdbn}` with `parse` / `to_bytes` |
| `text` | `TextEncoding::{ShiftJis, Utf8}` lossless `decode` / `encode` |

```rust
let data = std::fs::read(path)?;
let doc = l5_core::CfgBin::parse(&data)?;
assert_eq!(doc.to_bytes()?, data);            // byte-exact for game files
let json = serde_json::to_string(&doc)?;      // for the frontend
```

Parsers never panic on malformed input; every problem is an `l5_core::Error`.

## JSON shapes (serde, camelCase)

* `CfgBin`: the variant object plus `"format": "t2b" | "rdbn"`.
* T2B: `{entries: [{name, hash, values: [{type: "int"|"float"|"string", value}]}], footer, stringDedup, hashKind, info?}`.
  Tree nodes: `{entry, children?, end?, sortIndex?}` — indices into `entries`.
* RDBN (lossless model): `{version, types: [{name, hash, unkHash, fields: [{name, hash, type, category, size, count}]}],
  lists: [{name, hash, typeIndex, indexed, keyField, rows: [[[{type, value}]]]}]}` — `rows[row][field][element]`.
* RDBN view (display): `Rdbn::table(i)` → `{name, typeName, indexed, columns: [{key, type, count, …}],
  rows: [{fieldName: plainValue}]}`; arrays are JSON arrays, embedded records are objects.

Floats are stored as `f32` (bit-exact in binary); JSON cannot carry NaN.

## Notes and deviations from the Python reference

* Undecodable string bytes are mapped to `U+10FF00 + byte` (like Python's `surrogateescape`) so round trips stay exact.
* Entries/fields/lists keep their stored name hash; the writer writes it verbatim (use `T2b::rename_entry` when renaming).
* T2B 8-byte value width (other Level-5 games) is rejected with `Error::UnsupportedValueWidth`.
* RDBN keeps unreferenced string-pool tails (`trailing_strings`), so `soccer_common_text.cfg.bin` round-trips too.
* Sort keys (T2B `__SORT_INDEX` and RDBN indexes) are detected per list from the stored index; a field whose stable
  argsort reproduces it exactly is preferred (same result as Python on all game files).
* The counted tree also structures `X_LIST_BEG … X_LIST_END` blocks as rows when their counts fill the block.

## Tests

```text
cargo test -p l5-core                                   # unit tests (synthetic files, malformed-input fuzz)
set VR_GAME_DATA=<...>\Extracted\data                   # optional, defaults to <repo>ssets7.1.2\data (your own extracted copy)
cargo test -p l5-core --release --test game_data -- --ignored --nocapture
```

The game test is read-only. Last run (v7.1.2): T2B 84 400 / 84 400 (70 798 `.cfg.bin` + 13 602 other extensions),
RDBN 302 / 302, JSON round trip 84 702 / 84 702, `__SORT_INDEX` regeneration unchanged in every file (68 lists).
