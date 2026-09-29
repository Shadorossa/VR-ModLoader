//! Reading CRI CPK archives.
//!
//! Spec: `docs/formats/cpk.md` §1–2 and §5. A CPK is a sequence of 16-byte-headed packets (`CPK `, `TOC `, `ITOC`,
//! `ETOC`, …) each followed by an `@UTF` table, plus file contents. On PC the whole file is XOR-encrypted with
//! `crc32(<cpk file name>)` (`docs/formats/encryption.md` §1); [`CpkArchive`] wraps the reader in a
//! [`XorReader`] so only the bytes actually read are decrypted (a 4.5 GB CPK is never loaded).
//!
//! File offsets: `min(TocOffset, ContentOffset) + FileOffset`. Stored size `FileSize < ExtractSize` means the
//! entry is CRILAYLA-compressed.

use std::collections::HashMap;
use std::fs::File;
use std::io::{self, BufReader, Read, Seek, SeekFrom, Write};
use std::path::Path;

use crate::crilayla;
use crate::error::{Error, Result, check_bounds, show_magic};
use crate::utf::UtfTable;
use crate::xor::{Encryption, XorReader, basename, detect_with};

/// Upper bound for a single packet's @UTF table (sanity check against corrupt headers).
const MAX_TABLE_SIZE: u64 = 1 << 30;

/// One file inside a CPK.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CpkEntry {
    /// Directory inside the archive, without trailing `/` (e.g. `data/common/chr/_uniform/u11010010`); may be empty.
    pub dir: String,
    /// File name.
    pub name: String,
    /// Absolute byte offset of the stored data in the (decrypted) CPK.
    pub offset: u64,
    /// Stored size (`FileSize`); smaller than `extract_size` when CRILAYLA-compressed.
    pub size: u64,
    /// Real size (`ExtractSize`).
    pub extract_size: u64,
    /// `ID` column.
    pub id: u32,
    /// Row index in the TOC.
    pub toc_index: usize,
}

impl CpkEntry {
    /// `dir/name` (or `name` if `dir` is empty).
    pub fn path(&self) -> String {
        if self.dir.is_empty() { self.name.clone() } else { format!("{}/{}", self.dir, self.name) }
    }
    /// True if the entry is stored compressed (`FileSize != ExtractSize`).
    pub fn is_compressed(&self) -> bool {
        self.size != self.extract_size
    }
}

/// A packet header: 4-byte magic, u32 LE flag (`0xFF`), u64 LE table size.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PacketHeader {
    /// `CPK `, `TOC `, `ITOC`, `ETOC`, …
    pub magic: [u8; 4],
    /// Usually `0xFF`.
    pub flag: u32,
    /// Size of the @UTF table after the 16-byte header.
    pub size: u64,
}

/// An open CPK archive over any `Read + Seek` source.
#[derive(Debug)]
pub struct CpkArchive<R> {
    reader: XorReader<R>,
    len: u64,
    header: UtfTable,
    toc: Option<UtfTable>,
    entries: Vec<CpkEntry>,
    index: HashMap<String, usize>,
}

impl CpkArchive<BufReader<File>> {
    /// Open a CPK file, detecting the filename-XOR encryption from its name.
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        let file = File::open(path)?;
        let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        Self::from_reader(BufReader::with_capacity(1 << 16, file), &name)
    }
}

impl<R: Read + Seek> CpkArchive<R> {
    /// Open from a reader positioned anywhere; `file_name` is the CPK's on-disk name (for the XOR key).
    ///
    /// Tries plaintext, `crc32(name)` and `crc32(lowercase name)` (Viola's order).
    pub fn from_reader(mut inner: R, file_name: &str) -> Result<Self> {
        inner.seek(SeekFrom::Start(0))?;
        let mut head = [0u8; 4];
        inner.read_exact(&mut head).map_err(|_| Error::Cpk("file shorter than 4 bytes".into()))?;
        let enc = detect_with(file_name, &head, |h| h.starts_with(b"CPK "))
            .ok_or_else(|| Error::UnknownEncryption(basename(file_name).to_owned()))?;
        Self::with_encryption(inner, enc)
    }

    /// Open with a known encryption (plain or a specific XOR key).
    pub fn with_encryption(mut inner: R, enc: Encryption) -> Result<Self> {
        let len = inner.seek(SeekFrom::End(0))?;
        inner.seek(SeekFrom::Start(0))?;
        let reader = XorReader::new(inner, enc.key())?;
        let mut me = Self { reader, len, header: UtfTable::default(), toc: None, entries: Vec::new(), index: HashMap::new() };
        let (h, header) = me.read_packet(0)?;
        if &h.magic != b"CPK " {
            return Err(Error::BadMagic { what: "CPK header", expected: "CPK ", found: show_magic(&h.magic) });
        }
        if header.rows.is_empty() {
            return Err(Error::Cpk("CpkHeader table has no rows".into()));
        }
        me.header = header;
        me.load_toc()?;
        Ok(me)
    }

    fn header_u64(&self, name: &str) -> Option<u64> {
        self.header.get_u64(0, name)
    }

    fn load_toc(&mut self) -> Result<()> {
        let toc_off = match self.header_u64("TocOffset") {
            Some(o) if o != 0 && o != u64::MAX => o,
            _ => return Err(Error::Cpk("CPK has no TOC (ITOC-only archives are not supported)".into())),
        };
        let (h, toc) = self.read_packet(toc_off)?;
        if &h.magic != b"TOC " {
            return Err(Error::BadMagic { what: "TOC packet", expected: "TOC ", found: show_magic(&h.magic) });
        }
        let content_off = self.header_u64("ContentOffset").unwrap_or(toc_off);
        let base = toc_off.min(content_off);
        let col = |n: &str| toc.column_index(n);
        let (c_dir, c_name, c_size, c_ext, c_off, c_id) =
            (col("DirName"), col("FileName"), col("FileSize"), col("ExtractSize"), col("FileOffset"), col("ID"));
        let (Some(c_name), Some(c_size), Some(c_off)) = (c_name, c_size, c_off) else {
            return Err(Error::Cpk("TOC lacks FileName/FileSize/FileOffset".into()));
        };
        let mut entries = Vec::with_capacity(toc.rows.len());
        for r in 0..toc.rows.len() {
            let s = |c: Option<usize>| c.and_then(|c| toc.get_at(r, c)).and_then(|v| v.as_str()).unwrap_or("").to_owned();
            let u = |c: Option<usize>| c.and_then(|c| toc.get_at(r, c)).and_then(|v| v.as_u64());
            let size = u(Some(c_size)).ok_or_else(|| Error::Cpk(format!("TOC row {r}: bad FileSize")))?;
            let rel = u(Some(c_off)).ok_or_else(|| Error::Cpk(format!("TOC row {r}: bad FileOffset")))?;
            let offset = base.checked_add(rel).ok_or_else(|| Error::Cpk(format!("TOC row {r}: offset overflow")))?;
            entries.push(CpkEntry {
                dir: s(c_dir),
                name: s(Some(c_name)),
                offset,
                size,
                extract_size: u(c_ext).unwrap_or(size),
                id: u(c_id).and_then(|v| u32::try_from(v).ok()).unwrap_or(r as u32),
                toc_index: r,
            });
        }
        let mut index = HashMap::with_capacity(entries.len());
        for (i, e) in entries.iter().enumerate() {
            index.entry(e.path()).or_insert(i);
        }
        self.index = index;
        self.entries = entries;
        self.toc = Some(toc);
        Ok(())
    }

    /// Read `len` decrypted bytes at absolute offset `offset`.
    pub fn read_at(&mut self, offset: u64, len: u64) -> Result<Vec<u8>> {
        check_bounds("CPK read", offset, len, self.len)?;
        let n = usize::try_from(len).map_err(|_| Error::OutOfRange("read larger than address space".into()))?;
        self.reader.seek(SeekFrom::Start(offset))?;
        let mut buf = vec![0u8; n];
        self.reader.read_exact(&mut buf)?;
        Ok(buf)
    }

    /// Read and parse the packet at `offset` (16-byte header + @UTF table).
    pub fn read_packet(&mut self, offset: u64) -> Result<(PacketHeader, UtfTable)> {
        let h = self.read_at(offset, 16)?;
        let header = PacketHeader {
            magic: [h[0], h[1], h[2], h[3]],
            flag: u32::from_le_bytes([h[4], h[5], h[6], h[7]]),
            size: u64::from_le_bytes([h[8], h[9], h[10], h[11], h[12], h[13], h[14], h[15]]),
        };
        if header.size > MAX_TABLE_SIZE {
            return Err(Error::Cpk(format!("packet at {offset:#x} claims a {} byte table", header.size)));
        }
        let body = self.read_at(offset + 16, header.size)?;
        Ok((header, UtfTable::parse(&body)?))
    }

    /// Total length of the CPK file.
    pub fn len(&self) -> u64 {
        self.len
    }

    /// True if the file is empty (never, for a valid CPK).
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// The XOR key used (`None` if plaintext).
    pub fn key(&self) -> Option<u32> {
        self.reader.key()
    }

    /// The `CpkHeader` table (row 0 holds the values).
    pub fn header(&self) -> &UtfTable {
        &self.header
    }

    /// The raw `CpkTocInfo` table.
    pub fn toc(&self) -> Option<&UtfTable> {
        self.toc.as_ref()
    }

    /// Parse the `ITOC` packet (`CpkExtendId`), if any.
    pub fn itoc(&mut self) -> Result<Option<UtfTable>> {
        self.optional_packet("ItocOffset", b"ITOC")
    }

    /// Parse the `ETOC` packet (`CpkEtocInfo`), if any.
    pub fn etoc(&mut self) -> Result<Option<UtfTable>> {
        self.optional_packet("EtocOffset", b"ETOC")
    }

    fn optional_packet(&mut self, col: &str, magic: &[u8; 4]) -> Result<Option<UtfTable>> {
        match self.header_u64(col) {
            Some(o) if o != 0 && o != u64::MAX => {
                let (h, t) = self.read_packet(o)?;
                if &h.magic != magic {
                    return Err(Error::Cpk(format!("{col} points at a {:?} packet", show_magic(&h.magic))));
                }
                Ok(Some(t))
            }
            _ => Ok(None),
        }
    }

    /// All files, in TOC order.
    pub fn entries(&self) -> &[CpkEntry] {
        &self.entries
    }

    /// Find a file by `dir/name` (exact, case-sensitive match; a leading `/` is ignored).
    pub fn find(&self, path: &str) -> Option<&CpkEntry> {
        self.index.get(path.trim_start_matches('/')).map(|&i| &self.entries[i])
    }

    /// The stored bytes of an entry, without decompression.
    pub fn read_raw(&mut self, entry: &CpkEntry) -> Result<Vec<u8>> {
        self.read_at(entry.offset, entry.size)
    }

    /// Extract an entry into memory, decompressing CRILAYLA when `FileSize != ExtractSize`.
    pub fn extract(&mut self, entry: &CpkEntry) -> Result<Vec<u8>> {
        let raw = self.read_raw(entry)?;
        if !entry.is_compressed() {
            return Ok(raw);
        }
        if !crilayla::is_crilayla(&raw) {
            return Err(Error::Cpk(format!(
                "{}: FileSize {} != ExtractSize {} but data is not CRILAYLA",
                entry.path(),
                entry.size,
                entry.extract_size
            )));
        }
        let out = crilayla::decompress(&raw)?;
        if out.len() as u64 != entry.extract_size {
            return Err(Error::Cpk(format!(
                "{}: decompressed {} bytes, TOC says {}",
                entry.path(),
                out.len(),
                entry.extract_size
            )));
        }
        Ok(out)
    }

    /// Extract an entry by path.
    pub fn extract_path(&mut self, path: &str) -> Result<Vec<u8>> {
        let e = self.find(path).cloned().ok_or_else(|| Error::NotFound(path.to_owned()))?;
        self.extract(&e)
    }

    /// Stream an entry to `out` (uncompressed entries are copied in chunks, so multi-GB files never sit in RAM).
    /// Returns the number of bytes written.
    pub fn extract_to<W: Write>(&mut self, entry: &CpkEntry, out: &mut W) -> Result<u64> {
        if entry.is_compressed() {
            let data = self.extract(entry)?;
            out.write_all(&data)?;
            return Ok(data.len() as u64);
        }
        self.copy_raw_to(entry, out)
    }

    /// Stream the stored bytes of an entry (no decompression) to `out`.
    pub fn copy_raw_to<W: Write>(&mut self, entry: &CpkEntry, out: &mut W) -> Result<u64> {
        check_bounds("CPK entry", entry.offset, entry.size, self.len)?;
        self.reader.seek(SeekFrom::Start(entry.offset))?;
        let n = io::copy(&mut (&mut self.reader).take(entry.size), out)?;
        if n != entry.size {
            return Err(Error::Truncated { what: "CPK entry", offset: entry.offset, needed: entry.size, available: n });
        }
        Ok(n)
    }

    /// Unwrap the underlying reader.
    pub fn into_inner(self) -> R {
        self.reader.into_inner()
    }
}
