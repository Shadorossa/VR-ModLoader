//! Building CPK archives.
//!
//! Follows Viola's `CCriCpkWriter` (`docs/formats/cpk.md` §4), whose output the game loads from
//! `data/packs_custom/`:
//!
//! ```text
//! 0x000   CPK packet (CpkHeader)
//! 0x800   content, files sorted case-insensitively by path, each aligned to 0x800
//! ...     TOC packet (aligned 0x800)
//! ...     ETOC packet (aligned 0x800); the file ends right after it
//! ```
//!
//! No ITOC, unmasked tables, then the whole file is XOR-encrypted with `crc32(<output cpk name>)`.
//!
//! Differences from Viola, both opt-in or fixes:
//! - entries copied from an existing archive ([`CpkSource::Entry`]) keep their real `ExtractSize`, so a
//!   CRILAYLA-compressed original is described correctly (Viola records `ExtractSize = stored size`);
//! - [`CpkBuilder::compress`] can CRILAYLA-compress files (Viola never does; game acceptance **untested**).

use std::borrow::Cow;
use std::fs::File;
use std::io::{self, BufWriter, Read, Seek, Write};
use std::path::{Path, PathBuf};

use crate::cpk::{CpkArchive, CpkEntry};
use crate::crilayla;
use crate::error::{Error, Result};
use crate::utf::{ColumnType, Storage, UtfTable, Value};
use crate::xor::{XorWriter, key_for_name};

/// Alignment of packets and file data.
pub const ALIGN: u64 = 0x800;
/// Offset of the `(c)CRI` tag found in every retail CPK header block.
const CRI_TAG_OFFSET: u64 = 0x7FA;
/// `UpdateDateTime` Viola writes for every ETOC row.
pub const ETOC_UPDATE_DATE_TIME: u64 = 570_276_038_271_829_760;
/// `Tvers` Viola writes.
pub const TVERS: &str = "N/A, DLL3.11.05";

/// Where a file's bytes come from.
#[derive(Debug, Clone)]
pub enum CpkSource {
    /// In-memory plain file contents.
    Bytes(Vec<u8>),
    /// A file on disk (streamed at write time; never loaded whole unless compressing).
    File(PathBuf),
    /// Already-stored bytes (e.g. a CRILAYLA blob) with the real extracted size.
    Stored {
        /// Bytes written verbatim.
        data: Vec<u8>,
        /// `ExtractSize` to record.
        extract_size: u64,
    },
    /// The stored bytes of an entry of the archive passed to [`CpkBuilder::write_with_archive`], copied raw.
    Entry(CpkEntry),
}

#[derive(Debug, Clone)]
struct Item {
    path: String,
    source: CpkSource,
}

/// Builds a new CPK from files (see module docs).
#[derive(Debug, Clone, Default)]
pub struct CpkBuilder {
    items: Vec<Item>,
    compress: bool,
}

enum Payload<'a> {
    Mem(Cow<'a, [u8]>),
    File(&'a Path),
    Entry(&'a CpkEntry),
}

struct Prepared<'a> {
    dir: String,
    name: String,
    stored: u64,
    extract: u64,
    payload: Payload<'a>,
}

fn split_path(path: &str) -> (String, String) {
    let p = path.replace('\\', "/");
    let p = p.trim_start_matches('/');
    match p.rfind('/') {
        Some(i) => (p[..i].to_owned(), p[i + 1..].to_owned()),
        None => (String::new(), p.to_owned()),
    }
}

fn align(v: u64) -> u64 {
    v.div_ceil(ALIGN) * ALIGN
}

fn packet(magic: &[u8; 4], table: &UtfTable) -> Result<Vec<u8>> {
    let utf = table.to_bytes()?;
    let mut p = Vec::with_capacity(16 + utf.len());
    p.extend_from_slice(magic);
    p.extend_from_slice(&0xFFu32.to_le_bytes());
    p.extend_from_slice(&(utf.len() as u64).to_le_bytes());
    p.extend_from_slice(&utf);
    Ok(p)
}

fn write_zeros<W: Write>(w: &mut XorWriter<W>, until: u64) -> io::Result<()> {
    const Z: [u8; 4096] = [0; 4096];
    while w.position() < until {
        let n = (until - w.position()).min(Z.len() as u64) as usize;
        w.write_all(&Z[..n])?;
    }
    Ok(())
}

/// Compress `data` if enabled and worthwhile: `(stored, extract, payload)`.
fn maybe_compress(enabled: bool, data: Cow<'_, [u8]>) -> Result<(u64, u64, Cow<'_, [u8]>)> {
    let ext = data.len() as u64;
    if enabled && data.len() >= crilayla::PREFIX_SIZE {
        let z = crilayla::compress(&data)?;
        if z.len() < data.len() {
            return Ok((z.len() as u64, ext, Cow::Owned(z)));
        }
    }
    Ok((ext, ext, data))
}

/// Case-insensitive ordering like .NET `StringComparer.OrdinalIgnoreCase` (upper-cases, then compares).
fn cmp_ignore_case(a: &str, b: &str) -> std::cmp::Ordering {
    a.chars().flat_map(char::to_uppercase).cmp(b.chars().flat_map(char::to_uppercase))
}

impl CpkBuilder {
    /// An empty builder.
    pub fn new() -> Self {
        Self::default()
    }

    /// A builder listing every entry of `archive` as a raw [`CpkSource::Entry`] (for "rebuild with some files
    /// replaced", like Viola's `CAudioCpkRebuilder`). Write it with [`CpkBuilder::write_with_archive`].
    pub fn from_archive<R: Read + Seek>(archive: &CpkArchive<R>) -> Self {
        let items = archive
            .entries()
            .iter()
            .map(|e| Item { path: e.path(), source: CpkSource::Entry(e.clone()) })
            .collect();
        Self { items, compress: false }
    }

    /// Enable CRILAYLA compression of [`CpkSource::Bytes`] / [`CpkSource::File`] items (kept only when smaller).
    pub fn compress(&mut self, on: bool) -> &mut Self {
        self.compress = on;
        self
    }

    /// Add a file at archive path `path` (`dir/name`, `/`-separated).
    pub fn add(&mut self, path: impl Into<String>, source: CpkSource) -> &mut Self {
        self.items.push(Item { path: path.into(), source });
        self
    }

    /// Add in-memory contents.
    pub fn add_bytes(&mut self, path: impl Into<String>, data: Vec<u8>) -> &mut Self {
        self.add(path, CpkSource::Bytes(data))
    }

    /// Add a file from disk.
    pub fn add_file(&mut self, path: impl Into<String>, file: impl Into<PathBuf>) -> &mut Self {
        self.add(path, CpkSource::File(file.into()))
    }

    /// Replace the source of an existing path (exact match first, else a unique case-insensitive match).
    /// Returns false if nothing matched.
    pub fn replace(&mut self, path: &str, source: CpkSource) -> bool {
        let idx = self.items.iter().position(|i| i.path == path).or_else(|| {
            let mut m = self.items.iter().enumerate().filter(|(_, i)| i.path.eq_ignore_ascii_case(path));
            match (m.next(), m.next()) {
                (Some((i, _)), None) => Some(i),
                _ => None,
            }
        });
        match idx {
            Some(i) => {
                self.items[i].source = source;
                true
            }
            None => false,
        }
    }

    /// Replace `path` if present, otherwise add it.
    pub fn upsert(&mut self, path: &str, source: CpkSource) -> &mut Self {
        if !self.replace(path, source.clone()) {
            self.add(path, source);
        }
        self
    }

    /// Paths currently in the builder.
    pub fn paths(&self) -> impl Iterator<Item = &str> {
        self.items.iter().map(|i| i.path.as_str())
    }

    /// Number of files.
    pub fn len(&self) -> usize {
        self.items.len()
    }

    /// True if no files were added.
    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    fn prepare(&self) -> Result<Vec<Prepared<'_>>> {
        let mut out = Vec::with_capacity(self.items.len());
        for it in &self.items {
            let (dir, name) = split_path(&it.path);
            if name.is_empty() {
                return Err(Error::Cpk(format!("invalid archive path {:?}", it.path)));
            }
            let (stored, extract, payload) = match &it.source {
                CpkSource::Bytes(b) => {
                    let (s, e, d) = maybe_compress(self.compress, Cow::Borrowed(b))?;
                    (s, e, Payload::Mem(d))
                }
                CpkSource::File(p) if self.compress => {
                    let (s, e, d) = maybe_compress(self.compress, Cow::Owned(std::fs::read(p)?))?;
                    (s, e, Payload::Mem(d))
                }
                CpkSource::File(p) => {
                    let len = std::fs::metadata(p)?.len();
                    (len, len, Payload::File(p))
                }
                CpkSource::Stored { data, extract_size } => {
                    (data.len() as u64, *extract_size, Payload::Mem(Cow::Borrowed(data)))
                }
                CpkSource::Entry(e) => (e.size, e.extract_size, Payload::Entry(e)),
            };
            if stored > u32::MAX as u64 || extract > u32::MAX as u64 {
                return Err(Error::OutOfRange(format!("{}: files over 4 GiB cannot be described", it.path)));
            }
            out.push(Prepared { dir, name, stored, extract, payload });
        }
        // Stable sort by full path, case-insensitive (Viola: OrderBy(RelativePath, OrdinalIgnoreCase)).
        out.sort_by(|a, b| {
            let pa = if a.dir.is_empty() { a.name.clone() } else { format!("{}/{}", a.dir, a.name) };
            let pb = if b.dir.is_empty() { b.name.clone() } else { format!("{}/{}", b.dir, b.name) };
            cmp_ignore_case(&pa, &pb)
        });
        for w in out.windows(2) {
            if w[0].dir == w[1].dir && w[0].name == w[1].name {
                return Err(Error::Cpk(format!("duplicate path {}/{}", w[0].dir, w[0].name)));
            }
        }
        Ok(out)
    }

    /// Write the CPK to `out`, XOR-encrypting it with `key` (`None` = plaintext).
    ///
    /// Fails if the builder holds [`CpkSource::Entry`] items (use [`CpkBuilder::write_with_archive`]).
    pub fn write<W: Write>(&self, out: W, key: Option<u32>) -> Result<()> {
        self.write_impl::<io::Cursor<Vec<u8>>, W>(None, out, key)
    }

    /// Write the CPK, copying [`CpkSource::Entry`] items raw from `archive`.
    pub fn write_with_archive<R: Read + Seek, W: Write>(
        &self,
        archive: &mut CpkArchive<R>,
        out: W,
        key: Option<u32>,
    ) -> Result<()> {
        self.write_impl(Some(archive), out, key)
    }

    /// Write to a file; if `encrypt`, the key is `crc32(<file name of path>)` as the game expects.
    pub fn write_file(&self, path: impl AsRef<Path>, encrypt: bool) -> Result<()> {
        self.write_file_impl::<io::Cursor<Vec<u8>>>(None, path.as_ref(), encrypt)
    }

    /// [`CpkBuilder::write_file`] with a source archive for [`CpkSource::Entry`] items.
    pub fn write_file_with_archive<R: Read + Seek>(
        &self,
        archive: &mut CpkArchive<R>,
        path: impl AsRef<Path>,
        encrypt: bool,
    ) -> Result<()> {
        self.write_file_impl(Some(archive), path.as_ref(), encrypt)
    }

    fn write_file_impl<R: Read + Seek>(&self, archive: Option<&mut CpkArchive<R>>, path: &Path, encrypt: bool) -> Result<()> {
        let key = if encrypt {
            let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            Some(key_for_name(&name))
        } else {
            None
        };
        let mut w = BufWriter::with_capacity(1 << 20, File::create(path)?);
        self.write_impl(archive, &mut w, key)?;
        w.flush()?;
        Ok(())
    }

    fn write_impl<R: Read + Seek, W: Write>(
        &self,
        mut archive: Option<&mut CpkArchive<R>>,
        out: W,
        key: Option<u32>,
    ) -> Result<()> {
        let files = self.prepare()?;

        // Content layout.
        let content_offset = ALIGN;
        let mut offsets = Vec::with_capacity(files.len());
        let mut cursor = content_offset;
        for f in &files {
            cursor = align(cursor);
            offsets.push(cursor);
            cursor += f.stored;
        }
        let toc_offset = align(cursor);

        // TOC.
        let mut toc = UtfTable::new("CpkTocInfo");
        let common_dir = files.first().map(|f| f.dir.clone()).filter(|d| files.iter().all(|f| f.dir.eq_ignore_ascii_case(d)));
        match &common_dir {
            Some(d) => toc.add_const("DirName", ColumnType::String, Value::String(d.clone())),
            None => toc.add_column("DirName", ColumnType::String, Storage::PerRow),
        };
        toc.add_column("FileName", ColumnType::String, Storage::PerRow)
            .add_column("FileSize", ColumnType::U32, Storage::PerRow)
            .add_column("ExtractSize", ColumnType::U32, Storage::PerRow)
            .add_column("FileOffset", ColumnType::U64, Storage::PerRow)
            .add_column("ID", ColumnType::U32, Storage::PerRow)
            .add_const("UserString", ColumnType::String, Value::String("<NULL>".into()));
        for (i, f) in files.iter().enumerate() {
            let mut vals = Vec::with_capacity(6);
            if common_dir.is_none() {
                vals.push(Value::String(f.dir.clone()));
            }
            vals.extend([
                Value::String(f.name.clone()),
                Value::U32(f.stored as u32),
                Value::U32(f.extract as u32),
                Value::U64(offsets[i] - content_offset),
                Value::U32(i as u32),
            ]);
            toc.push_row_values(vals)?;
        }

        // ETOC.
        let mut etoc = UtfTable::new("CpkEtocInfo");
        etoc.seed_strings.push("<NULL>".into());
        etoc.add_column("UpdateDateTime", ColumnType::U64, Storage::PerRow)
            .add_column("LocalDir", ColumnType::String, Storage::PerRow);
        for f in &files {
            etoc.push_row_values([Value::U64(ETOC_UPDATE_DATE_TIME), Value::String(f.dir.clone())])?;
        }
        etoc.push_row_values([Value::U64(0), Value::String(String::new())])?;

        let toc_packet = packet(b"TOC ", &toc)?;
        let etoc_packet = packet(b"ETOC", &etoc)?;
        let etoc_offset = align(toc_offset + toc_packet.len() as u64);
        let packed: u64 = files.iter().map(|f| f.stored).sum();
        let data_size: u64 = files.iter().map(|f| f.extract).sum();
        let n_files = u32::try_from(files.len()).map_err(|_| Error::OutOfRange("too many files".into()))?;

        let header = header_table(&HeaderValues {
            content_offset,
            content_size: toc_offset - content_offset,
            toc_offset,
            toc_size: toc_packet.len() as u64,
            etoc_offset,
            etoc_size: etoc_packet.len() as u64,
            packed,
            data_size,
            files: n_files,
        })?;
        let cpk_packet = packet(b"CPK ", &header)?;
        if cpk_packet.len() as u64 > content_offset {
            return Err(Error::Cpk("CPK header packet larger than 0x800".into()));
        }

        // Emit.
        let mut w = XorWriter::new(out, key, 0);
        w.write_all(&cpk_packet)?;
        // Every retail CPK carries the CRI copyright tag right before the content (docs/formats/cpk.md).
        if cpk_packet.len() as u64 <= CRI_TAG_OFFSET && content_offset >= CRI_TAG_OFFSET + 6 {
            write_zeros(&mut w, CRI_TAG_OFFSET)?;
            w.write_all(b"(c)CRI")?;
        }
        for (f, &off) in files.iter().zip(&offsets) {
            write_zeros(&mut w, off)?;
            let written = match &f.payload {
                Payload::Mem(d) => {
                    w.write_all(d)?;
                    d.len() as u64
                }
                Payload::File(p) => io::copy(&mut File::open(p)?.take(f.stored), &mut w)?,
                Payload::Entry(e) => {
                    let a = archive
                        .as_deref_mut()
                        .ok_or_else(|| Error::Cpk("CpkSource::Entry needs write_with_archive".into()))?;
                    a.copy_raw_to(e, &mut w)?
                }
            };
            if written != f.stored {
                return Err(Error::Cpk(format!("{}/{}: size changed while writing", f.dir, f.name)));
            }
        }
        write_zeros(&mut w, toc_offset)?;
        w.write_all(&toc_packet)?;
        write_zeros(&mut w, etoc_offset)?;
        w.write_all(&etoc_packet)?;
        w.flush()?;
        Ok(())
    }
}

struct HeaderValues {
    content_offset: u64,
    content_size: u64,
    toc_offset: u64,
    toc_size: u64,
    etoc_offset: u64,
    etoc_size: u64,
    packed: u64,
    data_size: u64,
    files: u32,
}

/// `CpkHeader` exactly as Viola lays it out (column order, types, zero columns).
fn header_table(v: &HeaderValues) -> Result<UtfTable> {
    use ColumnType::*;
    let cols: &[(&str, ColumnType, bool)] = &[
        ("UpdateDateTime", U64, true),
        ("FileSize", U64, false),
        ("ContentOffset", U64, true),
        ("ContentSize", U64, true),
        ("TocOffset", U64, true),
        ("TocSize", U64, true),
        ("TocCrc", U32, false),
        ("EtocOffset", U64, true),
        ("EtocSize", U64, true),
        ("ItocOffset", U64, false),
        ("ItocSize", U64, false),
        ("ItocCrc", U32, false),
        ("GtocOffset", U64, false),
        ("GtocSize", U64, false),
        ("GtocCrc", U32, false),
        ("EnabledPackedSize", U64, true),
        ("EnabledDataSize", U64, true),
        ("TotalDataSize", U64, false),
        ("Tocs", U32, false),
        ("Files", U32, true),
        ("Groups", U32, true),
        ("Attrs", U32, true),
        ("TotalFiles", U32, false),
        ("Directories", U32, false),
        ("Updates", U32, false),
        ("Version", U16, true),
        ("Revision", U16, true),
        ("Align", U16, true),
        ("Sorted", U16, true),
        ("EID", U16, false),
        ("CpkMode", U32, true),
        ("Tvers", String, true),
        ("Comment", String, false),
        ("Codec", U32, true),
        ("DpkItoc", U32, true),
    ];
    let mut t = UtfTable::new("CpkHeader");
    for &(name, ty, per_row) in cols {
        t.add_column(name, ty, if per_row { Storage::PerRow } else { Storage::Zero });
    }
    t.push_row_values([
        Value::U64(1),
        Value::U64(v.content_offset),
        Value::U64(v.content_size),
        Value::U64(v.toc_offset),
        Value::U64(v.toc_size),
        Value::U64(v.etoc_offset),
        Value::U64(v.etoc_size),
        Value::U64(v.packed),
        Value::U64(v.data_size),
        Value::U32(v.files),
        Value::U32(0),
        Value::U32(0),
        Value::U16(7),
        Value::U16(2),
        Value::U16(ALIGN as u16),
        Value::U16(1),
        Value::U32(1),
        Value::String(TVERS.into()),
        Value::U32(0),
        Value::U32(0),
    ])?;
    Ok(t)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::xor::Encryption;

    fn build(compress: bool, key: Option<u32>) -> (Vec<u8>, Vec<(String, Vec<u8>)>) {
        let files: Vec<(String, Vec<u8>)> = vec![
            ("data/common/sound_asset/ja/b.awb".into(), (0..5000u32).map(|i| (i * 13) as u8).collect()),
            ("data/common/sound_asset/ja/A.acb".into(), b"@UTF fake acb ".repeat(300)),
            ("data/common/x/c.bin".into(), vec![]),
            ("root.txt".into(), b"hi".to_vec()),
        ];
        let mut b = CpkBuilder::new();
        b.compress(compress);
        for (p, d) in &files {
            b.add_bytes(p.clone(), d.clone());
        }
        let mut out = Vec::new();
        b.write(&mut out, key).unwrap();
        (out, files)
    }

    #[test]
    fn build_and_reopen() {
        for (compress, key) in [(false, None), (false, Some(0x1234_5678)), (true, Some(key_for_name("t.cpk")))] {
            let (bytes, files) = build(compress, key);
            let enc = key.map_or(Encryption::Plain, Encryption::Xor);
            let mut a = CpkArchive::with_encryption(io::Cursor::new(bytes), enc).unwrap();
            assert_eq!(a.entries().len(), files.len());
            // Sorted case-insensitively: A.acb before b.awb.
            let names: Vec<_> = a.entries().iter().map(|e| e.path()).collect();
            assert_eq!(
                names,
                ["data/common/sound_asset/ja/A.acb", "data/common/sound_asset/ja/b.awb", "data/common/x/c.bin", "root.txt"]
            );
            for (p, d) in &files {
                let e = a.find(p).unwrap().clone();
                assert_eq!(e.offset % ALIGN, 0);
                assert_eq!(&a.extract(&e).unwrap(), d, "{p}");
            }
            assert!(a.etoc().unwrap().is_some());
            assert!(a.itoc().unwrap().is_none());
            if compress {
                assert!(a.find("data/common/sound_asset/ja/A.acb").unwrap().is_compressed());
            }
        }
    }

    #[test]
    fn common_dir_is_constant() {
        let mut b = CpkBuilder::new();
        b.add_bytes("d/x.acb", vec![1; 10]).add_bytes("d/y.awb", vec![2; 10]);
        let mut out = Vec::new();
        b.write(&mut out, None).unwrap();
        let a = CpkArchive::with_encryption(io::Cursor::new(out), Encryption::Plain).unwrap();
        let toc = a.toc().unwrap();
        assert_eq!(toc.columns[0].storage, Storage::Constant);
        assert_eq!(a.entries()[1].path(), "d/y.awb");
    }

    #[test]
    fn rebuild_with_replacement() {
        let (bytes, _) = build(true, None);
        let mut src = CpkArchive::with_encryption(io::Cursor::new(bytes), Encryption::Plain).unwrap();
        let mut b = CpkBuilder::from_archive(&src);
        assert!(b.replace("data/common/sound_asset/ja/b.awb", CpkSource::Bytes(b"new".to_vec())));
        assert!(b.replace("DATA/common/x/C.BIN", CpkSource::Bytes(b"ci".to_vec())));
        let mut out = Vec::new();
        b.write_with_archive(&mut src, &mut out, None).unwrap();
        let mut a = CpkArchive::with_encryption(io::Cursor::new(out), Encryption::Plain).unwrap();
        assert_eq!(a.extract_path("data/common/sound_asset/ja/b.awb").unwrap(), b"new");
        assert_eq!(a.extract_path("data/common/x/c.bin").unwrap(), b"ci");
        // Compressed original copied raw, still described correctly.
        let acb = a.find("data/common/sound_asset/ja/A.acb").unwrap().clone();
        assert!(acb.is_compressed());
        assert_eq!(a.extract(&acb).unwrap(), b"@UTF fake acb ".repeat(300));
        // Entry sources without an archive are an error, not a panic.
        assert!(b.write(Vec::new(), None).is_err());
    }

    #[test]
    fn duplicates_rejected() {
        let mut b = CpkBuilder::new();
        b.add_bytes("a/b", vec![]).add_bytes("a/b", vec![1]);
        assert!(b.write(Vec::new(), None).is_err());
    }
}
