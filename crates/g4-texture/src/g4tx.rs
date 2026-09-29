//! G4TX — Level-5 G4 texture container (spec: `docs/formats/g4tx.md`).
//!
//! ```text
//! +0x00  header (0x60)
//! +0x60  texture records     N × 0x30
//!        sprite records      M × 0x18      (sub-textures / atlas regions)
//!        align 16
//!        name-hash table     (N+M) × u32   CRC-32 of each name
//!        id table            (N+M) × u8    argsort of the hashes
//!        align 4
//!        name-offset table   (N+M) × u16   relative to this table; then 4 zero bytes
//!        names               NUL-terminated, entry order
//!        align 4             <- 0x60 + tableSize
//!        align 16            (padding; retail files hold uninitialised bytes here)
//! payload base:
//!        payload[i]          one complete DDS per texture, 16-aligned
//! ```
//!
//! [`G4tx::parse`] + [`G4tx::to_bytes`] reproduce every retail menu/font file byte for byte:
//! the original id-table tie order and the padding garbage are kept while still valid.

use crate::dds::{self, DdsFormat, DdsInfo, EncodeOptions};
use crate::error::{Error, Result, invalid};
use crate::hash::{name_hash, sorted_index_preserving};
use crate::header::{G4Header, offset_to_ptr};
use crate::pixels::RgbaImage;
use crate::util::{Reader, align, check_name, fit, name_from_bytes, put_i16, put_u16, put_u32};

/// `"G4TX"`.
pub const G4TX_MAGIC: &[u8; 4] = b"G4TX";
/// Header size of every G4TX.
pub const G4TX_HEADER_SIZE: usize = 0x60;
/// Byte size of a texture record.
pub const TEXTURE_RECORD_SIZE: usize = 0x30;
/// Byte size of a sprite (sub-texture) record.
pub const SPRITE_RECORD_SIZE: usize = 0x18;
/// File type tag at +0x06.
pub const G4TX_FILE_TYPE: u16 = 0x65;
/// Swizzle word of almost every texture (identity RGBA mapping).
pub const SWIZZLE_DEFAULT: u32 = 0xAAE4;
/// Swizzle word seen on every uncompressed R8G8B8A8 payload (636/636 in menu+font).
pub const SWIZZLE_RGBA8: u32 = 0xAA6C;

/// Format-code bit 0x20: set for NPOT and for uncompressed textures; record `row_pitch`
/// then holds `width * 4`.
pub const FORMAT_PITCH_BIT: u8 = 0x20;

/// One texture record plus its DDS payload.
#[derive(Clone, PartialEq, Eq)]
pub struct Texture {
    /// Entry name (hashed with CRC-32; consumers bind by hash).
    pub name: String,
    /// Complete DDS file.
    pub dds: Vec<u8>,
    /// +0x00, always 0 in retail files.
    pub unk00: u32,
    /// +0x0C: mip-0 byte size for BCn; `width*4` or 0 for uncompressed.
    pub data_size: u32,
    /// +0x10: `(flags << 24) | (kind << 16) | (mipCount << 8) | formatByte`.
    pub format_code: u32,
    /// +0x14: component swizzle (0xAAE4, or 0xAA6C for RGBA8).
    pub swizzle: u32,
    /// +0x18: width in pixels.
    pub width: u16,
    /// +0x1A: height in pixels.
    pub height: u16,
    /// +0x1C: array/depth count (1).
    pub depth: u32,
    /// +0x20: `width * 4` when format bit 0x20 is set, else 0.
    pub row_pitch: u32,
    /// +0x24..0x30, always 0 in retail files.
    pub reserved: [u8; 12],
}

impl std::fmt::Debug for Texture {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Texture")
            .field("name", &self.name)
            .field("width", &self.width)
            .field("height", &self.height)
            .field("format_code", &format_args!("0x{:08X}", self.format_code))
            .field("swizzle", &format_args!("0x{:X}", self.swizzle))
            .field("data_size", &self.data_size)
            .field("row_pitch", &self.row_pitch)
            .field("dds_len", &self.dds.len())
            .finish()
    }
}

/// Low 5 bits of the format byte used for a DDS format in retail files.
fn codec_bits(format: DdsFormat) -> Option<u8> {
    Some(match format {
        // BC4: the retail map cut-out masks (dx11/map/ar/tr/tr.g4tx «tr_01_a.a») carry 0x05 too
        DdsFormat::Bc7 | DdsFormat::Bc4 | DdsFormat::Rgba8 | DdsFormat::Bgra8 | DdsFormat::Bgrx8 => 0x05,
        DdsFormat::Bc3 => 0x08,
        DdsFormat::Bc1 => 0x06,
        _ => return None,
    })
}

impl Texture {
    /// Build a texture record around a DDS blob, with the field conventions of the retail
    /// files (kind 3, or 2 for BGRA8 like the fonts; swizzle 0xAA6C for RGBA8).
    pub fn from_dds(name: impl Into<String>, dds: Vec<u8>) -> Result<Self> {
        let name = name.into();
        check_name("G4TX texture", &name)?;
        let info = DdsInfo::parse(&dds)?;
        let codec = codec_bits(info.format).ok_or_else(|| {
            Error::Unsupported(format!("no known G4TX format code for {:?}", info.format))
        })?;
        let kind: u32 = if info.format == DdsFormat::Bgra8 {
            2
        } else {
            3
        };
        let mut t = Texture {
            name,
            dds: Vec::new(),
            unk00: 0,
            data_size: 0,
            format_code: (kind << 16) | 0x80 | codec as u32,
            swizzle: if info.format == DdsFormat::Rgba8 {
                SWIZZLE_RGBA8
            } else {
                SWIZZLE_DEFAULT
            },
            width: 0,
            height: 0,
            depth: 1,
            row_pitch: 0,
            reserved: [0; 12],
        };
        // Kind-3 raw textures store width*4 in data_size; kind 2 stores 0.
        if !info.format.is_block_compressed() && kind == 3 {
            t.data_size = 1;
        }
        t.set_dds(dds)?;
        Ok(t)
    }

    /// Mip count from the format code.
    pub fn mip_count(&self) -> u8 {
        (self.format_code >> 8) as u8
    }

    /// "kind" byte (3 for almost everything, 2 for some uncompressed / cube maps).
    pub fn kind(&self) -> u8 {
        (self.format_code >> 16) as u8
    }

    /// Format byte `0x80 | pitchBit(0x20) | codec`.
    pub fn format_byte(&self) -> u8 {
        self.format_code as u8
    }

    /// Top byte of the format code (1 for cube maps).
    pub fn format_flags(&self) -> u8 {
        (self.format_code >> 24) as u8
    }

    /// Parse the payload's DDS header.
    pub fn dds_info(&self) -> Result<DdsInfo> {
        DdsInfo::parse(&self.dds)
    }

    /// Decode one mip level to RGBA8.
    pub fn decode(&self, level: u32) -> Result<RgbaImage> {
        dds::decode(&self.dds, level)
    }

    /// Replace the payload with `dds` and update width, height, format code (mip count,
    /// pitch bit, codec), row pitch and data size to stay consistent with it
    /// (rules in `docs/formats/g4tx.md` §3 and §7).
    pub fn set_dds(&mut self, dds: Vec<u8>) -> Result<()> {
        let info = DdsInfo::parse(&dds)?;
        if dds.len() < info.expected_len() {
            return Err(Error::Truncated {
                what: "DDS payload",
                offset: 0,
                len: info.expected_len(),
                size: dds.len(),
            });
        }
        let width: u16 = fit(info.width as usize, "texture width")?;
        let height: u16 = fit(info.height as usize, "texture height")?;
        let mips: u8 = fit(info.mip_count as usize, "mip count")?;
        let old_format = DdsInfo::parse(&self.dds).ok().map(|i| i.format);
        let codec = if old_format == Some(info.format) {
            self.format_code as u8 & 0x1F
        } else {
            let c = codec_bits(info.format).ok_or_else(|| {
                Error::Unsupported(format!("no known G4TX format code for {:?}", info.format))
            })?;
            if old_format.is_some() {
                self.swizzle = if info.format == DdsFormat::Rgba8 {
                    SWIZZLE_RGBA8
                } else {
                    SWIZZLE_DEFAULT
                };
                if info.format == DdsFormat::Bc7 {
                    self.format_code = (self.format_code & 0xFF00_FFFF) | (3 << 16);
                }
            }
            c
        };
        let bc = info.format.is_block_compressed();
        let npot = !(info.width.is_power_of_two() && info.height.is_power_of_two());
        let pitch_bit = npot || !bc;
        let fmt_byte = 0x80 | if pitch_bit { FORMAT_PITCH_BIT } else { 0 } | codec;
        self.format_code =
            (self.format_code & 0xFFFF_0000) | ((mips as u32) << 8) | fmt_byte as u32;
        self.row_pitch = if pitch_bit { info.width * 4 } else { 0 };
        self.data_size = if bc {
            fit(info.mip0_size(), "mip-0 size")?
        } else if self.data_size == 0 {
            0
        } else {
            info.width * 4
        };
        self.width = width;
        self.height = height;
        self.dds = dds;
        Ok(())
    }
}

/// A named rectangle inside a texture (sub-texture / atlas sprite).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Sprite {
    /// Entry name (UI materials bind to CRC-32 of it).
    pub name: String,
    /// Index of the parent texture.
    pub parent: u16,
    /// Bit 0 = nine-slice data present; bits 1–2 unknown.
    pub flags: u16,
    /// X in the parent (px).
    pub x: i16,
    /// Y in the parent (px).
    pub y: i16,
    /// Width (px).
    pub width: i16,
    /// Height (px).
    pub height: i16,
    /// Nine-slice guides left, right, top, bottom (sprite-local px) when `flags & 1`.
    pub nine_slice: [i16; 4],
    /// Original/design size (w, h) when `flags & 1`, often the rect size or (0, 0).
    pub design_size: [i16; 2],
}

impl Sprite {
    /// Plain (non nine-slice) sprite.
    pub fn new(
        name: impl Into<String>,
        parent: u16,
        x: i16,
        y: i16,
        width: i16,
        height: i16,
    ) -> Self {
        Self {
            name: name.into(),
            parent,
            flags: 0,
            x,
            y,
            width,
            height,
            nine_slice: [0; 4],
            design_size: [0; 2],
        }
    }

    /// True if flag bit 0 (nine-slice data present) is set.
    pub fn has_nine_slice(&self) -> bool {
        self.flags & 1 != 0
    }

    /// Set (`Some((guides, design_size))`) or clear the nine-slice data and flag bit 0.
    pub fn set_nine_slice(&mut self, data: Option<([i16; 4], [i16; 2])>) {
        match data {
            Some((g, s)) => {
                self.flags |= 1;
                self.nine_slice = g;
                self.design_size = s;
            }
            None => {
                self.flags &= !1;
                self.nine_slice = [0; 4];
                self.design_size = [0; 2];
            }
        }
    }

    /// Crop this sprite out of its decoded parent texture (mip 0).
    pub fn crop_from(&self, atlas: &RgbaImage) -> RgbaImage {
        atlas.crop(
            self.x as i64,
            self.y as i64,
            self.width as i64,
            self.height as i64,
        )
    }
}

/// Result of a hash lookup.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntryRef {
    /// Index into [`G4tx::textures`].
    Texture(usize),
    /// Index into [`G4tx::sprites`].
    Sprite(usize),
}

/// A parsed G4TX file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct G4tx {
    /// Textures (entries `0..N`).
    pub textures: Vec<Texture>,
    /// Sprites / sub-textures (entries `N..N+M`).
    pub sprites: Vec<Sprite>,
    /// Raw header; unknown fields are written back unchanged.
    header_raw: [u8; G4TX_HEADER_SIZE],
    /// Original id table, reused while still a valid argsort (keeps retail tie order).
    sort_ids: Option<Vec<usize>>,
    /// Bytes between the table end and the payload base (garbage in retail files).
    table_padding: Vec<u8>,
}

impl Default for G4tx {
    fn default() -> Self {
        Self::new()
    }
}

impl G4tx {
    /// Empty container with the retail header constants.
    pub fn new() -> Self {
        let mut header_raw = [0u8; G4TX_HEADER_SIZE];
        G4Header {
            magic: *G4TX_MAGIC,
            header_size: G4TX_HEADER_SIZE as u16,
            file_type: G4TX_FILE_TYPE,
            reserved: 0,
            header_words: (G4TX_HEADER_SIZE / 4) as u16,
            size_field: 0,
        }
        .write_into(&mut header_raw);
        Self {
            textures: Vec::new(),
            sprites: Vec::new(),
            header_raw,
            sort_ids: None,
            table_padding: Vec::new(),
        }
    }

    /// The common G4 header prefix.
    pub fn header(&self) -> G4Header {
        // header_raw always holds at least 0x10 valid bytes.
        G4Header::parse(&self.header_raw, None).unwrap_or(G4Header {
            magic: *G4TX_MAGIC,
            header_size: G4TX_HEADER_SIZE as u16,
            file_type: G4TX_FILE_TYPE,
            reserved: 0,
            header_words: 0x18,
            size_field: 0,
        })
    }

    /// Parse a G4TX file.
    pub fn parse(data: &[u8]) -> Result<Self> {
        let h = G4Header::parse(data, Some("G4TX"))?;
        let hs = h.header_size as usize;
        if hs != G4TX_HEADER_SIZE {
            return Err(Error::Unsupported(format!(
                "G4TX header size 0x{hs:X} (expected 0x60)"
            )));
        }
        let r = Reader::new(data, "G4TX");
        let n = r.u16(0x20)? as usize;
        let total = r.u16(0x22)? as usize;
        let m = r.u8(0x25)? as usize;
        if total != n + m {
            return Err(invalid(
                "G4TX header",
                format!("total {total} != textures {n} + sprites {m}"),
            ));
        }
        let ptr = |off: usize| -> Result<usize> { Ok(h.resolve(r.u16(off)?)) };
        let hash_off = ptr(0x38)?;
        let tex_off = ptr(0x3A)?;
        let id_off = ptr(0x40)?;
        let sub_off = ptr(0x42)?;
        let names_off = ptr(0x46)?;
        let table_end = hs + h.size_field as usize;
        let base = align(table_end, 16);
        if base > data.len() {
            return Err(Error::Truncated {
                what: "G4TX table",
                offset: hs,
                len: base - hs,
                size: data.len(),
            });
        }

        let mut names = Vec::with_capacity(total);
        for k in 0..total {
            let rel = r.u16(names_off + 2 * k)? as usize;
            names.push(name_from_bytes("G4TX name", r.cstr(names_off + rel)?)?);
        }
        let ids = (0..total)
            .map(|k| r.u8(id_off + k).map(|v| v as usize))
            .collect::<Result<Vec<_>>>()?;
        r.bytes(hash_off, 4 * total)?;

        let mut textures = Vec::with_capacity(n);
        for (i, name) in names.iter().take(n).enumerate() {
            let o = tex_off + i * TEXTURE_RECORD_SIZE;
            r.bytes(o, TEXTURE_RECORD_SIZE)?;
            let poff = r.u32(o + 4)? as usize;
            let psize = r.u32(o + 8)? as usize;
            let dds = Reader::new(data, "G4TX payload")
                .bytes(base.saturating_add(poff), psize)?
                .to_vec();
            let mut reserved = [0u8; 12];
            reserved.copy_from_slice(r.bytes(o + 0x24, 12)?);
            textures.push(Texture {
                name: name.clone(),
                dds,
                unk00: r.u32(o)?,
                data_size: r.u32(o + 0x0C)?,
                format_code: r.u32(o + 0x10)?,
                swizzle: r.u32(o + 0x14)?,
                width: r.u16(o + 0x18)?,
                height: r.u16(o + 0x1A)?,
                depth: r.u32(o + 0x1C)?,
                row_pitch: r.u32(o + 0x20)?,
                reserved,
            });
        }
        let mut sprites = Vec::with_capacity(m);
        for (j, name) in names.iter().skip(n).enumerate() {
            let o = sub_off + j * SPRITE_RECORD_SIZE;
            r.bytes(o, SPRITE_RECORD_SIZE)?;
            let parent = r.u16(o)?;
            if parent as usize >= n {
                return Err(invalid(
                    "G4TX sprite",
                    format!("{name:?} parent {parent} >= {n} textures"),
                ));
            }
            sprites.push(Sprite {
                name: name.clone(),
                parent,
                flags: r.u16(o + 2)?,
                x: r.i16(o + 4)?,
                y: r.i16(o + 6)?,
                width: r.i16(o + 8)?,
                height: r.i16(o + 0x0A)?,
                nine_slice: [
                    r.i16(o + 0x0C)?,
                    r.i16(o + 0x0E)?,
                    r.i16(o + 0x10)?,
                    r.i16(o + 0x12)?,
                ],
                design_size: [r.i16(o + 0x14)?, r.i16(o + 0x16)?],
            });
        }
        let mut header_raw = [0u8; G4TX_HEADER_SIZE];
        header_raw.copy_from_slice(&data[..G4TX_HEADER_SIZE]);
        Ok(Self {
            textures,
            sprites,
            header_raw,
            sort_ids: Some(ids),
            table_padding: data
                .get(table_end..base)
                .map(<[u8]>::to_vec)
                .unwrap_or_default(),
        })
    }

    /// Read and parse a file.
    pub fn read(path: impl AsRef<std::path::Path>) -> Result<Self> {
        Self::parse(&std::fs::read(path)?)
    }

    /// Check the invariants the writer relies on.
    pub fn validate(&self) -> Result<()> {
        let (n, m) = (self.textures.len(), self.sprites.len());
        if m > 255 {
            return Err(Error::Limit(format!("{m} sprites (max 255, u8 count)")));
        }
        if n + m > 256 {
            return Err(Error::Limit(format!(
                "{} entries (max 256, u8 id table)",
                n + m
            )));
        }
        for t in &self.textures {
            check_name("G4TX texture", &t.name)?;
        }
        for s in &self.sprites {
            check_name("G4TX sprite", &s.name)?;
            if s.parent as usize >= n {
                return Err(invalid(
                    "G4TX sprite",
                    format!("{:?} parent {} >= {} textures", s.name, s.parent, n),
                ));
            }
        }
        Ok(())
    }

    /// Serialise. Recomputes hashes, the id table (keeping the original tie order when
    /// still valid), every pointer, `tableSize` and the payload size.
    pub fn to_bytes(&self) -> Result<Vec<u8>> {
        self.validate()?;
        let hs = G4TX_HEADER_SIZE;
        let (n, m) = (self.textures.len(), self.sprites.len());
        let total = n + m;
        let names: Vec<&str> = self
            .textures
            .iter()
            .map(|t| t.name.as_str())
            .chain(self.sprites.iter().map(|s| s.name.as_str()))
            .collect();
        let hashes: Vec<u32> = names.iter().map(|s| name_hash(s)).collect();
        let ids = sorted_index_preserving(&hashes, self.sort_ids.as_deref());

        let rec_end = hs + n * TEXTURE_RECORD_SIZE + m * SPRITE_RECORD_SIZE;
        let hash_off = align(rec_end, 16);
        let id_off = hash_off + 4 * total;
        let str_off = align(id_off + total, 4);
        let first = align(2 * total, 4) + 4;
        let mut name_offs = Vec::with_capacity(total);
        let mut cursor = first;
        for s in &names {
            name_offs.push(fit::<u16>(cursor, "G4TX name offset")?);
            cursor += s.len() + 1;
        }
        let table_end = align(str_off + cursor, 4);
        let base = align(table_end, 16);
        let mut payload_offs = Vec::with_capacity(n);
        let mut cursor = 0usize;
        for t in &self.textures {
            cursor = align(cursor, 16);
            payload_offs.push(fit::<u32>(cursor, "G4TX payload offset")?);
            cursor += t.dds.len();
        }
        let payload_len = align(cursor, 16);

        let mut out = vec![0u8; base + payload_len];
        out[..hs].copy_from_slice(&self.header_raw);
        out[0..4].copy_from_slice(G4TX_MAGIC);
        put_u16(&mut out, 4, hs as u16);
        put_u32(&mut out, 0x0C, fit(table_end - hs, "G4TX table size")?);
        put_u16(&mut out, 0x20, n as u16);
        put_u16(&mut out, 0x22, total as u16);
        out[0x25] = m as u8;
        put_u32(&mut out, 0x2C, fit(payload_len, "G4TX payload size")?);
        let p = |abs| offset_to_ptr(hs, abs);
        let ptrs = [
            p(hash_off)?,
            0,
            p(hash_off)?,
            p(base)?,
            p(id_off)?,
            p(hs + n * TEXTURE_RECORD_SIZE)?,
            p(hash_off)?,
            p(str_off)?,
        ];
        for (i, v) in ptrs.iter().enumerate() {
            put_u16(&mut out, 0x38 + 2 * i, *v);
        }

        for (i, t) in self.textures.iter().enumerate() {
            let o = hs + i * TEXTURE_RECORD_SIZE;
            put_u32(&mut out, o, t.unk00);
            put_u32(&mut out, o + 4, payload_offs[i]);
            put_u32(&mut out, o + 8, fit(t.dds.len(), "DDS size")?);
            put_u32(&mut out, o + 0x0C, t.data_size);
            put_u32(&mut out, o + 0x10, t.format_code);
            put_u32(&mut out, o + 0x14, t.swizzle);
            put_u16(&mut out, o + 0x18, t.width);
            put_u16(&mut out, o + 0x1A, t.height);
            put_u32(&mut out, o + 0x1C, t.depth);
            put_u32(&mut out, o + 0x20, t.row_pitch);
            out[o + 0x24..o + 0x30].copy_from_slice(&t.reserved);
        }
        for (j, s) in self.sprites.iter().enumerate() {
            let o = hs + n * TEXTURE_RECORD_SIZE + j * SPRITE_RECORD_SIZE;
            put_u16(&mut out, o, s.parent);
            put_u16(&mut out, o + 2, s.flags);
            for (k, v) in [s.x, s.y, s.width, s.height]
                .into_iter()
                .chain(s.nine_slice)
                .chain(s.design_size)
                .enumerate()
            {
                put_i16(&mut out, o + 4 + 2 * k, v);
            }
        }
        for k in 0..total {
            put_u32(&mut out, hash_off + 4 * k, hashes[k]);
            out[id_off + k] = ids[k] as u8;
            put_u16(&mut out, str_off + 2 * k, name_offs[k]);
            let s = str_off + name_offs[k] as usize;
            out[s..s + names[k].len()].copy_from_slice(names[k].as_bytes());
        }
        if self.table_padding.len() == base - table_end {
            out[table_end..base].copy_from_slice(&self.table_padding);
        }
        for (t, &o) in self.textures.iter().zip(&payload_offs) {
            let s = base + o as usize;
            out[s..s + t.dds.len()].copy_from_slice(&t.dds);
        }
        Ok(out)
    }

    /// Serialise to a file.
    pub fn write(&self, path: impl AsRef<std::path::Path>) -> Result<()> {
        std::fs::write(path, self.to_bytes()?)?;
        Ok(())
    }

    // --------------------------------------------------------------------------------
    // Lookup
    // --------------------------------------------------------------------------------

    /// Index of the first texture with this name.
    pub fn texture_index(&self, name: &str) -> Option<usize> {
        self.textures.iter().position(|t| t.name == name)
    }

    /// Index of the first sprite with this name.
    pub fn sprite_index(&self, name: &str) -> Option<usize> {
        self.sprites.iter().position(|s| s.name == name)
    }

    /// Find an entry by CRC-32 name hash (what G4MD materials store).
    pub fn find_hash(&self, hash: u32) -> Option<EntryRef> {
        self.textures
            .iter()
            .position(|t| name_hash(&t.name) == hash)
            .map(EntryRef::Texture)
            .or_else(|| {
                self.sprites
                    .iter()
                    .position(|s| name_hash(&s.name) == hash)
                    .map(EntryRef::Sprite)
            })
    }

    /// Sprites whose parent is texture `tex`.
    pub fn sprites_of(&self, tex: usize) -> impl Iterator<Item = (usize, &Sprite)> {
        self.sprites
            .iter()
            .enumerate()
            .filter(move |(_, s)| s.parent as usize == tex)
    }

    fn tex(&self, i: usize) -> Result<&Texture> {
        self.textures
            .get(i)
            .ok_or_else(|| Error::NotFound(format!("texture index {i}")))
    }

    fn tex_mut(&mut self, i: usize) -> Result<&mut Texture> {
        self.textures
            .get_mut(i)
            .ok_or_else(|| Error::NotFound(format!("texture index {i}")))
    }

    fn sprite_mut(&mut self, j: usize) -> Result<&mut Sprite> {
        self.sprites
            .get_mut(j)
            .ok_or_else(|| Error::NotFound(format!("sprite index {j}")))
    }

    // --------------------------------------------------------------------------------
    // Pixels
    // --------------------------------------------------------------------------------

    /// Decode mip `level` of texture `i` to RGBA8.
    pub fn decode_texture(&self, i: usize, level: u32) -> Result<RgbaImage> {
        self.tex(i)?.decode(level)
    }

    /// Decode the parent texture and crop sprite `j`.
    pub fn sprite_image(&self, j: usize) -> Result<RgbaImage> {
        let s = self
            .sprites
            .get(j)
            .ok_or_else(|| Error::NotFound(format!("sprite index {j}")))?;
        Ok(s.crop_from(&self.decode_texture(s.parent as usize, 0)?))
    }

    /// Replace texture `i`'s payload with a DDS blob (any supported format / size / mip
    /// count); record fields are updated to match.
    pub fn replace_texture_dds(&mut self, i: usize, dds: Vec<u8>) -> Result<()> {
        self.tex_mut(i)?.set_dds(dds)
    }

    /// Re-encode texture `i` from an RGBA8 image, keeping its codec and mip count and
    /// reusing its DDS header as a template (BC7 via Intel ISPC, BGRA8/RGBA8 packed raw).
    pub fn replace_texture_image(
        &mut self,
        i: usize,
        img: &RgbaImage,
        opts: &EncodeOptions,
    ) -> Result<()> {
        let t = self.tex(i)?;
        let info = t.dds_info()?;
        let mips = info
            .mip_count
            .min(dds::max_mip_count(img.width, img.height));
        let dds = dds::encode_dds(img, info.format, mips, Some(&t.dds), opts)?;
        self.tex_mut(i)?.set_dds(dds)
    }

    /// Paste `img` over sprite `j`'s rectangle in its parent texture and re-encode the
    /// parent. `img` is clipped to the sprite rectangle.
    pub fn replace_sprite_image(
        &mut self,
        j: usize,
        img: &RgbaImage,
        opts: &EncodeOptions,
    ) -> Result<()> {
        let s = self
            .sprites
            .get(j)
            .ok_or_else(|| Error::NotFound(format!("sprite index {j}")))?
            .clone();
        let parent = s.parent as usize;
        let mut atlas = self.decode_texture(parent, 0)?;
        let clipped = img.crop(0, 0, s.width as i64, s.height as i64);
        atlas.blit(&clipped, s.x as i64, s.y as i64);
        self.replace_texture_image(parent, &atlas, opts)
    }

    // --------------------------------------------------------------------------------
    // Structure edits
    // --------------------------------------------------------------------------------

    /// Append a texture encoded from `img` in `format` with `mips` levels.
    pub fn add_texture(
        &mut self,
        name: &str,
        img: &RgbaImage,
        format: DdsFormat,
        mips: u32,
        opts: &EncodeOptions,
    ) -> Result<usize> {
        let dds = dds::encode_dds(img, format, mips, None, opts)?;
        self.add_texture_dds(name, dds)
    }

    /// Append a texture from a DDS blob.
    pub fn add_texture_dds(&mut self, name: &str, dds: Vec<u8>) -> Result<usize> {
        if self.textures.len() + self.sprites.len() >= 256 {
            return Err(Error::Limit("G4TX holds at most 256 entries".into()));
        }
        self.textures.push(Texture::from_dds(name, dds)?);
        Ok(self.textures.len() - 1)
    }

    /// Remove texture `i`. Fails while sprites still reference it; parents of later
    /// textures' sprites are renumbered.
    pub fn remove_texture(&mut self, i: usize) -> Result<Texture> {
        self.tex(i)?;
        if let Some(s) = self.sprites.iter().find(|s| s.parent as usize == i) {
            return Err(invalid(
                "G4TX edit",
                format!("sprite {:?} still uses texture {i}", s.name),
            ));
        }
        for s in &mut self.sprites {
            if s.parent as usize > i {
                s.parent -= 1;
            }
        }
        Ok(self.textures.remove(i))
    }

    /// Rename texture `i`.
    pub fn rename_texture(&mut self, i: usize, name: &str) -> Result<()> {
        check_name("G4TX texture", name)?;
        self.tex_mut(i)?.name = name.to_owned();
        Ok(())
    }

    /// Append a sprite.
    pub fn add_sprite(&mut self, sprite: Sprite) -> Result<usize> {
        check_name("G4TX sprite", &sprite.name)?;
        if sprite.parent as usize >= self.textures.len() {
            return Err(invalid(
                "G4TX sprite",
                format!("parent {} out of range", sprite.parent),
            ));
        }
        if self.sprites.len() >= 255 || self.textures.len() + self.sprites.len() >= 256 {
            return Err(Error::Limit(
                "G4TX holds at most 255 sprites / 256 entries".into(),
            ));
        }
        self.sprites.push(sprite);
        Ok(self.sprites.len() - 1)
    }

    /// Remove sprite `j`.
    pub fn remove_sprite(&mut self, j: usize) -> Result<Sprite> {
        self.sprite_mut(j)?;
        Ok(self.sprites.remove(j))
    }

    /// Rename sprite `j`.
    pub fn rename_sprite(&mut self, j: usize, name: &str) -> Result<()> {
        check_name("G4TX sprite", name)?;
        self.sprite_mut(j)?.name = name.to_owned();
        Ok(())
    }

    /// Move sprite `j` to `(x, y)` inside texture `parent`.
    pub fn move_sprite(&mut self, j: usize, parent: u16, x: i16, y: i16) -> Result<()> {
        if parent as usize >= self.textures.len() {
            return Err(invalid(
                "G4TX sprite",
                format!("parent {parent} out of range"),
            ));
        }
        let s = self.sprite_mut(j)?;
        s.parent = parent;
        s.x = x;
        s.y = y;
        Ok(())
    }

    /// Resize sprite `j`.
    pub fn resize_sprite(&mut self, j: usize, width: i16, height: i16) -> Result<()> {
        let s = self.sprite_mut(j)?;
        s.width = width;
        s.height = height;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> G4tx {
        let mut tx = G4tx::new();
        let mut img = RgbaImage::new(140, 68);
        for (i, p) in img.data.chunks_exact_mut(4).enumerate() {
            p.copy_from_slice(&[(i % 251) as u8, 40, 200, 255]);
        }
        let opts = EncodeOptions {
            bc7_quality: dds::Bc7Quality::UltraFast,
            threads: 2,
        };
        // BC7 needs the `encode` feature; fall back to raw BGRA8 without it.
        let fmt = if cfg!(feature = "encode") {
            DdsFormat::Bc7
        } else {
            DdsFormat::Bgra8
        };
        tx.add_texture("plate_btn01_atl", &img, fmt, 1, &opts).unwrap();
        tx.add_texture("font", &RgbaImage::new(64, 32), DdsFormat::Bgra8, 3, &opts)
            .unwrap();
        let mut s = Sprite::new("plate_btn01", 0, 0, 0, 140, 68);
        s.set_nine_slice(Some(([62, 78, 0, 68], [140, 68])));
        tx.add_sprite(s).unwrap();
        tx.add_sprite(Sprite::new("base_btn01", 1, 0, 0, 8, 16))
            .unwrap();
        tx
    }

    #[test]
    #[cfg(feature = "encode")] // asserts BC7 sizes/format codes
    fn build_parse_round_trip() {
        let tx = sample();
        let bytes = tx.to_bytes().unwrap();
        let back = G4tx::parse(&bytes).unwrap();
        assert_eq!(back.textures, tx.textures);
        assert_eq!(back.sprites, tx.sprites);
        assert_eq!(back.to_bytes().unwrap(), bytes);

        let t0 = &back.textures[0];
        assert_eq!(t0.format_code, 0x0003_01A5); // NPOT BC7, 1 mip
        assert_eq!(t0.row_pitch, 560);
        assert_eq!(t0.data_size, 35 * 17 * 16);
        let t1 = &back.textures[1];
        assert_eq!(t1.format_code, 0x0002_03A5); // BGRA8 font style, 3 mips
        assert_eq!(
            (t1.row_pitch, t1.data_size, t1.swizzle),
            (256, 0, SWIZZLE_DEFAULT)
        );
        assert_eq!(
            back.find_hash(name_hash("plate_btn01")),
            Some(EntryRef::Sprite(0))
        );
        assert_eq!(back.sprite_image(0).unwrap().width, 140);
        // header invariants from the spec
        assert_eq!(&bytes[4..12], &[0x60, 0, 0x65, 0, 0, 0, 0x18, 0]);
        let payload = u32::from_le_bytes(bytes[0x2C..0x30].try_into().unwrap()) as usize;
        let base = 0x60 + u16::from_le_bytes([bytes[0x3E], bytes[0x3F]]) as usize * 4;
        assert_eq!(base + payload, bytes.len());
    }

    #[test]
    fn edits_regenerate_index() {
        let mut tx = sample();
        tx.rename_sprite(1, "zzz").unwrap();
        tx.move_sprite(1, 0, 10, 12).unwrap();
        let back = G4tx::parse(&tx.to_bytes().unwrap()).unwrap();
        assert_eq!(back.sprites[1].name, "zzz");
        assert_eq!((back.sprites[1].parent, back.sprites[1].x), (0, 10));
        let hashes: Vec<u32> = ["plate_btn01_atl", "font", "plate_btn01", "zzz"]
            .iter()
            .map(|s| name_hash(s))
            .collect();
        assert!(crate::hash::is_sorted_index(
            &hashes,
            back.sort_ids.as_ref().unwrap()
        ));
        assert!(tx.remove_texture(0).is_err());
        tx.remove_sprite(0).unwrap();
        tx.remove_sprite(0).unwrap();
        tx.remove_texture(0).unwrap();
        assert_eq!(tx.textures.len(), 1);
        assert!(tx.rename_texture(0, "bad\0name").is_err());
        assert!(tx.add_sprite(Sprite::new("x", 5, 0, 0, 1, 1)).is_err());
    }

    #[test]
    #[cfg(feature = "encode")] // asserts BC7 sizes/format codes
    fn replace_image_changes_size() {
        let mut tx = sample();
        let img = RgbaImage::new(256, 128);
        tx.replace_texture_image(0, &img, &EncodeOptions::default())
            .unwrap();
        let t = &tx.textures[0];
        assert_eq!(
            (t.width, t.height, t.format_code, t.row_pitch),
            (256, 128, 0x0003_0185, 0)
        );
        let back = G4tx::parse(&tx.to_bytes().unwrap()).unwrap();
        assert_eq!(back.decode_texture(0, 0).unwrap(), img);
        tx.replace_sprite_image(0, &RgbaImage::new(4, 4), &EncodeOptions::default())
            .unwrap();
    }

    #[test]
    fn garbage_never_panics() {
        let bytes = sample().to_bytes().unwrap();
        for cut in [0, 3, 0x20, 0x60, 0xB0, 0x100, bytes.len() - 1] {
            let _ = G4tx::parse(&bytes[..cut]);
        }
        let mut bad = bytes.clone();
        bad[0x22] = 9; // total != n + m
        assert!(G4tx::parse(&bad).is_err());
        let mut bad = bytes.clone();
        for b in &mut bad[0x38..0x48] {
            *b = 0xFF;
        }
        assert!(G4tx::parse(&bad).is_err());
        let mut noisy = bytes;
        for i in (0x60..noisy.len()).step_by(7) {
            noisy[i] = noisy[i].wrapping_mul(31).wrapping_add(7);
            let _ = G4tx::parse(&noisy);
        }
    }
}
