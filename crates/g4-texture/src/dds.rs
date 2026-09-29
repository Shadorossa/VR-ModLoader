//! DDS parsing, decoding to RGBA8 and encoding from RGBA8.
//!
//! Every G4TX payload on PC is a complete `.dds` file (see `docs/formats/g4tx.md` §6.1):
//! mostly a DX10 header with `BC7_UNORM`, plus legacy `DXT1`/`DXT5` and uncompressed
//! 32-bpp RGBA8/BGRA8 (fonts). Mip levels follow the header, largest first, with no
//! tiling or swizzling.
//!
//! * Decoding uses [`bcdec_rs`] (pure Rust port of `bcdec`) for BC1–BC7.
//! * Encoding uses [`intel_tex_2`] (Intel ISPC Texture Compressor, prebuilt kernels, no C
//!   toolchain needed) for BC1/BC3/BC4/BC5/BC7; uncompressed formats are packed directly.

use crate::error::{Error, Result, invalid};
use crate::pixels::RgbaImage;
use crate::util::{Reader, put_u32};

/// `"DDS "`.
pub const DDS_MAGIC: &[u8; 4] = b"DDS ";
/// Size of the legacy header including the magic.
pub const DDS_HEADER_LEN: usize = 0x80;
/// Size of the header including the magic and the DX10 extension.
pub const DDS_DX10_HEADER_LEN: usize = 0x94;

// Header flags (DDSD_*).
const DDSD_PITCH: u32 = 0x8;
const DDSD_MIPMAPCOUNT: u32 = 0x2_0000;
const DDSD_LINEARSIZE: u32 = 0x8_0000;
const DDSD_REQUIRED: u32 = 0x1007; // CAPS | HEIGHT | WIDTH | PIXELFORMAT
// Pixel format flags (DDPF_*).
const DDPF_ALPHAPIXELS: u32 = 0x1;
const DDPF_ALPHA: u32 = 0x2;
const DDPF_FOURCC: u32 = 0x4;
const DDPF_RGB: u32 = 0x40;
const DDPF_LUMINANCE: u32 = 0x2_0000;
// Caps.
const DDSCAPS_COMPLEX: u32 = 0x8;
const DDSCAPS_TEXTURE: u32 = 0x1000;
const DDSCAPS_MIPMAP: u32 = 0x40_0000;
const DDSCAPS2_CUBEMAP: u32 = 0x200;
const DDSCAPS2_VOLUME: u32 = 0x20_0000;

/// Pixel format of a DDS surface.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DdsFormat {
    /// BC1 / DXT1 (8-byte blocks, 1-bit alpha).
    Bc1,
    /// BC2 / DXT3 (explicit 4-bit alpha).
    Bc2,
    /// BC3 / DXT5 (interpolated alpha).
    Bc3,
    /// BC4 unsigned (single channel, decoded as gray).
    Bc4,
    /// BC4 signed.
    Bc4Snorm,
    /// BC5 unsigned (two channels, decoded to R and G).
    Bc5,
    /// BC5 signed.
    Bc5Snorm,
    /// BC6H unsigned half float (decoded by clamping to 0..1, no tone mapping).
    Bc6hUf16,
    /// BC6H signed half float.
    Bc6hSf16,
    /// BC7 (the format of almost every VR texture).
    Bc7,
    /// 32-bpp R8G8B8A8 (byte order R, G, B, A).
    Rgba8,
    /// 32-bpp B8G8R8A8 (byte order B, G, R, A) — fonts.
    Bgra8,
    /// 32-bpp B8G8R8X8 (alpha ignored).
    Bgrx8,
    /// 24-bpp B8G8R8.
    Bgr8,
    /// 8-bpp red / luminance.
    R8,
    /// 8-bpp alpha.
    A8,
    /// 16-bpp R8G8.
    Rg8,
    /// 64-bpp R16G16B16A16 half float (cube maps in `dx11/map`).
    Rgba16Float,
    /// Other uncompressed legacy format described by channel bit masks (R, G, B, A).
    Masked {
        /// Bits per pixel (8, 16, 24 or 32).
        bits: u8,
        /// R, G, B, A masks (0 = channel absent).
        masks: [u32; 4],
    },
}

impl DdsFormat {
    /// True for the BCn block formats.
    pub fn is_block_compressed(self) -> bool {
        matches!(
            self,
            Self::Bc1
                | Self::Bc2
                | Self::Bc3
                | Self::Bc4
                | Self::Bc4Snorm
                | Self::Bc5
                | Self::Bc5Snorm
                | Self::Bc6hUf16
                | Self::Bc6hSf16
                | Self::Bc7
        )
    }

    /// Bytes per 4×4 block (BCn) or per pixel (uncompressed).
    pub fn unit_bytes(self) -> usize {
        match self {
            Self::Bc1 | Self::Bc4 | Self::Bc4Snorm => 8,
            Self::Bc2
            | Self::Bc3
            | Self::Bc5
            | Self::Bc5Snorm
            | Self::Bc6hUf16
            | Self::Bc6hSf16
            | Self::Bc7 => 16,
            Self::Rgba8 | Self::Bgra8 | Self::Bgrx8 => 4,
            Self::Bgr8 => 3,
            Self::R8 | Self::A8 => 1,
            Self::Rg8 => 2,
            Self::Rgba16Float => 8,
            Self::Masked { bits, .. } => (bits as usize).div_ceil(8).max(1),
        }
    }

    /// Byte size of one `w × h` surface.
    pub fn surface_size(self, w: u32, h: u32) -> usize {
        let (w, h) = (w.max(1) as usize, h.max(1) as usize);
        if self.is_block_compressed() {
            w.div_ceil(4) * h.div_ceil(4) * self.unit_bytes()
        } else {
            w * h * self.unit_bytes()
        }
    }

    /// Row pitch in bytes (block rows for BCn).
    pub fn row_pitch(self, w: u32) -> usize {
        let w = w.max(1) as usize;
        if self.is_block_compressed() {
            w.div_ceil(4) * self.unit_bytes()
        } else {
            w * self.unit_bytes()
        }
    }

    /// DXGI format number used in a DX10 header, when one exists.
    pub fn dxgi(self) -> Option<u32> {
        Some(match self {
            Self::Bc1 => 71,
            Self::Bc2 => 74,
            Self::Bc3 => 77,
            Self::Bc4 => 80,
            Self::Bc4Snorm => 81,
            Self::Bc5 => 83,
            Self::Bc5Snorm => 84,
            Self::Bc6hUf16 => 95,
            Self::Bc6hSf16 => 96,
            Self::Bc7 => 98,
            Self::Rgba8 => 28,
            Self::Bgra8 => 87,
            Self::Bgrx8 => 88,
            Self::R8 => 61,
            Self::A8 => 65,
            Self::Rg8 => 49,
            Self::Rgba16Float => 10,
            Self::Bgr8 | Self::Masked { .. } => return None,
        })
    }

    fn from_dxgi(v: u32) -> Option<Self> {
        Some(match v {
            70..=72 => Self::Bc1,
            73..=75 => Self::Bc2,
            76..=78 => Self::Bc3,
            79 | 80 => Self::Bc4,
            81 => Self::Bc4Snorm,
            82 | 83 => Self::Bc5,
            84 => Self::Bc5Snorm,
            94 | 95 => Self::Bc6hUf16,
            96 => Self::Bc6hSf16,
            97..=99 => Self::Bc7,
            27..=29 => Self::Rgba8,
            87 | 90 | 91 => Self::Bgra8,
            88 | 92 | 93 => Self::Bgrx8,
            60 | 61 => Self::R8,
            65 => Self::A8,
            48 | 49 => Self::Rg8,
            10 => Self::Rgba16Float,
            _ => return None,
        })
    }

    fn from_fourcc(fcc: [u8; 4]) -> Option<Self> {
        Some(match &fcc {
            b"DXT1" => Self::Bc1,
            b"DXT2" | b"DXT3" => Self::Bc2,
            b"DXT4" | b"DXT5" => Self::Bc3,
            b"ATI1" | b"BC4U" => Self::Bc4,
            b"BC4S" => Self::Bc4Snorm,
            b"ATI2" | b"BC5U" => Self::Bc5,
            b"BC5S" => Self::Bc5Snorm,
            _ => match u32::from_le_bytes(fcc) {
                113 => Self::Rgba16Float, // D3DFMT_A16B16G16R16F
                _ => return None,
            },
        })
    }
}

/// Parsed DDS header.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DdsInfo {
    /// Width of mip 0.
    pub width: u32,
    /// Height of mip 0.
    pub height: u32,
    /// Raw depth field (0 or 1 for 2D textures).
    pub depth: u32,
    /// Number of mip levels (a stored 0 is reported as 1).
    pub mip_count: u32,
    /// Raw mip count field.
    pub raw_mip_count: u32,
    /// Pixel format.
    pub format: DdsFormat,
    /// DXGI format when a DX10 header is present.
    pub dxgi_format: Option<u32>,
    /// Pixel-format FourCC (`DX10`, `DXT5`, or zeros).
    pub fourcc: [u8; 4],
    /// Header flags (`DDSD_*`).
    pub flags: u32,
    /// Caps (`DDSCAPS_*`).
    pub caps: u32,
    /// Caps2 (`DDSCAPS2_*`).
    pub caps2: u32,
    /// 6 for cube maps, the DX10 array size otherwise (min 1).
    pub faces: u32,
    /// Byte offset of the first level (0x80 or 0x94).
    pub header_len: usize,
}

impl DdsInfo {
    /// Parse the header of a DDS blob.
    pub fn parse(blob: &[u8]) -> Result<Self> {
        let r = Reader::new(blob, "DDS header");
        let magic = r.bytes(0, 4).map_err(|_| Error::BadMagic {
            expected: "DDS ",
            found: blob.iter().take(4).copied().collect(),
        })?;
        if magic != DDS_MAGIC {
            if blob.starts_with(b"NXTCH") {
                return Err(Error::Unsupported(
                    "NXTCH000 (Switch, block-linear swizzled) payloads".into(),
                ));
            }
            return Err(Error::BadMagic {
                expected: "DDS ",
                found: magic.to_vec(),
            });
        }
        r.bytes(0, DDS_HEADER_LEN)?;
        let flags = r.u32(0x08)?;
        let height = r.u32(0x0C)?;
        let width = r.u32(0x10)?;
        let depth = r.u32(0x18)?;
        let raw_mip_count = r.u32(0x1C)?;
        let pf_flags = r.u32(0x50)?;
        let f = r.bytes(0x54, 4)?;
        let fourcc = [f[0], f[1], f[2], f[3]];
        let bits = r.u32(0x58)?;
        let masks = [r.u32(0x5C)?, r.u32(0x60)?, r.u32(0x64)?, r.u32(0x68)?];
        let caps = r.u32(0x6C)?;
        let caps2 = r.u32(0x70)?;

        let mut faces = if caps2 & DDSCAPS2_CUBEMAP != 0 { 6 } else { 1 };
        let (format, dxgi_format, header_len) = if pf_flags & DDPF_FOURCC != 0 && &fourcc == b"DX10"
        {
            let dxgi = r.u32(0x80)?;
            let misc = r.u32(0x88)?;
            let array = r.u32(0x8C)?.max(1);
            faces = if misc & 0x4 != 0 { 6 * array } else { array };
            let fmt = DdsFormat::from_dxgi(dxgi)
                .ok_or_else(|| Error::Unsupported(format!("DXGI format {dxgi}")))?;
            (fmt, Some(dxgi), DDS_DX10_HEADER_LEN)
        } else if pf_flags & DDPF_FOURCC != 0 {
            let fmt = DdsFormat::from_fourcc(fourcc)
                .ok_or_else(|| Error::Unsupported(format!("DDS FourCC {fourcc:02X?}")))?;
            (fmt, None, DDS_HEADER_LEN)
        } else {
            (
                legacy_uncompressed(pf_flags, bits, masks)?,
                None,
                DDS_HEADER_LEN,
            )
        };
        if width == 0 || height == 0 {
            return Err(invalid("DDS header", format!("zero size {width}x{height}")));
        }
        if width > 1 << 16 || height > 1 << 16 {
            return Err(Error::Unsupported(format!(
                "DDS size {width}x{height} too large"
            )));
        }
        if caps2 & DDSCAPS2_VOLUME != 0 && depth > 1 {
            return Err(Error::Unsupported("volume DDS textures".into()));
        }
        let mip_count = raw_mip_count.clamp(1, 32);
        Ok(Self {
            width,
            height,
            depth,
            mip_count,
            raw_mip_count,
            format,
            dxgi_format,
            fourcc,
            flags,
            caps,
            caps2,
            faces,
            header_len,
        })
    }

    /// Dimensions of a mip level.
    pub fn level_dims(&self, level: u32) -> (u32, u32) {
        ((self.width >> level).max(1), (self.height >> level).max(1))
    }

    /// Byte size of one mip level (one face).
    pub fn level_size(&self, level: u32) -> usize {
        let (w, h) = self.level_dims(level);
        self.format.surface_size(w, h)
    }

    /// Byte offset of a mip level of face 0 inside the blob.
    pub fn level_offset(&self, level: u32) -> usize {
        self.header_len + (0..level).map(|l| self.level_size(l)).sum::<usize>()
    }

    /// Byte size of mip 0.
    pub fn mip0_size(&self) -> usize {
        self.level_size(0)
    }

    /// Total expected blob length (header + all levels of all faces).
    pub fn expected_len(&self) -> usize {
        self.header_len
            + self.faces as usize
                * (0..self.mip_count)
                    .map(|l| self.level_size(l))
                    .sum::<usize>()
    }

    /// Slice of `blob` holding one mip level of face 0.
    pub fn level_data<'a>(&self, blob: &'a [u8], level: u32) -> Result<&'a [u8]> {
        if level >= self.mip_count {
            return Err(Error::NotFound(format!(
                "mip level {level} (texture has {})",
                self.mip_count
            )));
        }
        Reader::new(blob, "DDS mip level").bytes(self.level_offset(level), self.level_size(level))
    }
}

fn legacy_uncompressed(pf_flags: u32, bits: u32, masks: [u32; 4]) -> Result<DdsFormat> {
    let [r, g, b, a] = masks;
    let a = if pf_flags & (DDPF_ALPHAPIXELS | DDPF_ALPHA) != 0 {
        a
    } else {
        0
    };
    let fmt = match (bits, [r, g, b, a]) {
        (32, [0xFF, 0xFF00, 0xFF_0000, 0xFF00_0000]) => DdsFormat::Rgba8,
        (32, [0xFF_0000, 0xFF00, 0xFF, 0xFF00_0000]) => DdsFormat::Bgra8,
        (32, [0xFF_0000, 0xFF00, 0xFF, 0]) => DdsFormat::Bgrx8,
        (24, [0xFF_0000, 0xFF00, 0xFF, 0]) => DdsFormat::Bgr8,
        (8, _) if pf_flags & DDPF_ALPHA != 0 && pf_flags & (DDPF_RGB | DDPF_LUMINANCE) == 0 => {
            DdsFormat::A8
        }
        (8, [0xFF, 0, 0, 0]) => DdsFormat::R8,
        (8 | 16 | 24 | 32, m) if m.iter().any(|&x| x != 0) => DdsFormat::Masked {
            bits: bits as u8,
            masks: m,
        },
        _ => {
            return Err(Error::Unsupported(format!(
                "legacy DDS pixel format flags 0x{pf_flags:X} bits {bits} masks {masks:08X?}"
            )));
        }
    };
    Ok(fmt)
}

// ------------------------------------------------------------------------------------
// Decoding
// ------------------------------------------------------------------------------------

/// Decode one mip level (face 0) of a DDS blob to RGBA8.
pub fn decode(blob: &[u8], level: u32) -> Result<RgbaImage> {
    let info = DdsInfo::parse(blob)?;
    let (w, h) = info.level_dims(level);
    decode_surface(info.format, info.level_data(blob, level)?, w, h)
}

/// Decode a single `w × h` surface of the given format to RGBA8.
pub fn decode_surface(format: DdsFormat, data: &[u8], w: u32, h: u32) -> Result<RgbaImage> {
    let need = format.surface_size(w, h);
    if data.len() < need {
        return Err(Error::Truncated {
            what: "DDS surface",
            offset: 0,
            len: need,
            size: data.len(),
        });
    }
    let data = &data[..need];
    if format.is_block_compressed() {
        Ok(decode_blocks(format, data, w, h))
    } else {
        Ok(decode_raw(format, data, w, h))
    }
}

fn decode_blocks(format: DdsFormat, data: &[u8], w: u32, h: u32) -> RgbaImage {
    let (w, h) = (w.max(1), h.max(1));
    let mut out = RgbaImage::new(w, h);
    let bw = (w as usize).div_ceil(4);
    let bb = format.unit_bytes();
    let stride = w as usize * 4;
    let mut px = [0u8; 64];
    for (bi, block) in data.chunks_exact(bb).enumerate() {
        let (bx, by) = (bi % bw, bi / bw);
        decode_block(format, block, &mut px);
        for row in 0..4 {
            let y = by * 4 + row;
            if y >= h as usize {
                break;
            }
            let x0 = bx * 4;
            let n = (w as usize - x0).min(4);
            let d = y * stride + x0 * 4;
            out.data[d..d + n * 4].copy_from_slice(&px[row * 16..row * 16 + n * 4]);
        }
    }
    out
}

/// Decode one BCn block into 4×4 RGBA8 (row pitch 16 bytes).
fn decode_block(format: DdsFormat, block: &[u8], px: &mut [u8; 64]) {
    match format {
        DdsFormat::Bc1 => bcdec_rs::bc1(block, px, 16),
        DdsFormat::Bc2 => bcdec_rs::bc2(block, px, 16),
        DdsFormat::Bc3 => bcdec_rs::bc3(block, px, 16),
        DdsFormat::Bc7 => bcdec_rs::bc7(block, px, 16),
        DdsFormat::Bc4 | DdsFormat::Bc4Snorm => {
            let signed = format == DdsFormat::Bc4Snorm;
            let mut r = [0u8; 16];
            bcdec_rs::bc4(block, &mut r, 4, signed);
            for (i, &v) in r.iter().enumerate() {
                let v = if signed { snorm_to_u8(v) } else { v };
                px[i * 4..i * 4 + 4].copy_from_slice(&[v, v, v, 255]);
            }
        }
        DdsFormat::Bc5 | DdsFormat::Bc5Snorm => {
            let signed = format == DdsFormat::Bc5Snorm;
            let mut rg = [0u8; 32];
            bcdec_rs::bc5(block, &mut rg, 8, signed);
            for i in 0..16 {
                let (mut r, mut g) = (rg[i * 2], rg[i * 2 + 1]);
                if signed {
                    r = snorm_to_u8(r);
                    g = snorm_to_u8(g);
                }
                px[i * 4..i * 4 + 4].copy_from_slice(&[r, g, 0, 255]);
            }
        }
        DdsFormat::Bc6hUf16 | DdsFormat::Bc6hSf16 => {
            let mut hf = [0u16; 48];
            bcdec_rs::bc6h_half(block, &mut hf, 12, format == DdsFormat::Bc6hSf16);
            for i in 0..16 {
                px[i * 4] = unit_to_u8(half_to_f32(hf[i * 3]));
                px[i * 4 + 1] = unit_to_u8(half_to_f32(hf[i * 3 + 1]));
                px[i * 4 + 2] = unit_to_u8(half_to_f32(hf[i * 3 + 2]));
                px[i * 4 + 3] = 255;
            }
        }
        _ => px.fill(0),
    }
}

fn snorm_to_u8(v: u8) -> u8 {
    ((v as i8 as i16) + 128).clamp(0, 255) as u8
}

fn half_to_f32(h: u16) -> f32 {
    let sign = if h & 0x8000 != 0 { -1.0 } else { 1.0 };
    let e = ((h >> 10) & 0x1F) as i32;
    let m = (h & 0x3FF) as f32;
    let v = match e {
        0 => m * 2f32.powi(-24),
        31 => {
            if m == 0.0 {
                f32::INFINITY
            } else {
                f32::NAN
            }
        }
        _ => (1.0 + m / 1024.0) * 2f32.powi(e - 15),
    };
    sign * v
}

fn unit_to_u8(v: f32) -> u8 {
    if v.is_nan() {
        0
    } else {
        (v.clamp(0.0, 1.0) * 255.0 + 0.5) as u8
    }
}

fn decode_raw(format: DdsFormat, data: &[u8], w: u32, h: u32) -> RgbaImage {
    let n = w.max(1) as usize * h.max(1) as usize;
    let mut out = RgbaImage::new(w.max(1), h.max(1));
    let o = &mut out.data;
    match format {
        DdsFormat::Rgba8 => o.copy_from_slice(&data[..n * 4]),
        DdsFormat::Bgra8 | DdsFormat::Bgrx8 => {
            for (d, s) in o.chunks_exact_mut(4).zip(data.chunks_exact(4)) {
                d.copy_from_slice(&[
                    s[2],
                    s[1],
                    s[0],
                    if format == DdsFormat::Bgra8 {
                        s[3]
                    } else {
                        255
                    },
                ]);
            }
        }
        DdsFormat::Bgr8 => {
            for (d, s) in o.chunks_exact_mut(4).zip(data.chunks_exact(3)) {
                d.copy_from_slice(&[s[2], s[1], s[0], 255]);
            }
        }
        DdsFormat::R8 => {
            for (d, &s) in o.chunks_exact_mut(4).zip(data.iter()) {
                d.copy_from_slice(&[s, s, s, 255]);
            }
        }
        DdsFormat::A8 => {
            for (d, &s) in o.chunks_exact_mut(4).zip(data.iter()) {
                d.copy_from_slice(&[255, 255, 255, s]);
            }
        }
        DdsFormat::Rg8 => {
            for (d, s) in o.chunks_exact_mut(4).zip(data.chunks_exact(2)) {
                d.copy_from_slice(&[s[0], s[1], 0, 255]);
            }
        }
        DdsFormat::Rgba16Float => {
            for (d, s) in o.chunks_exact_mut(4).zip(data.chunks_exact(8)) {
                for c in 0..4 {
                    d[c] = unit_to_u8(half_to_f32(u16::from_le_bytes([s[c * 2], s[c * 2 + 1]])));
                }
            }
        }
        DdsFormat::Masked { bits, masks } => {
            let bpp = (bits as usize).div_ceil(8).max(1);
            for (d, s) in o.chunks_exact_mut(4).zip(data.chunks_exact(bpp)) {
                let mut v = 0u32;
                for (i, &b) in s.iter().enumerate() {
                    v |= (b as u32) << (8 * i);
                }
                for c in 0..4 {
                    let m = masks[c];
                    d[c] = if m == 0 {
                        if c == 3 { 255 } else { 0 }
                    } else {
                        let shift = m.trailing_zeros();
                        let max = (m >> shift) as u64;
                        (((v & m) >> shift) as u64 * 255 / max) as u8
                    };
                }
            }
        }
        _ => {}
    }
    out
}

// ------------------------------------------------------------------------------------
// Encoding
// ------------------------------------------------------------------------------------

/// BC7 encoder speed/quality preset (maps to the Intel ISPC presets).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Bc7Quality {
    /// Fastest, lowest quality.
    UltraFast,
    /// Very fast.
    VeryFast,
    /// Fast (good default for previews).
    Fast,
    /// Balanced (default).
    #[default]
    Basic,
    /// Best quality, slow.
    Slow,
}

/// Options for [`encode_surface`] / [`encode_dds`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct EncodeOptions {
    /// BC7 preset (ignored for other formats).
    pub bc7_quality: Bc7Quality,
    /// Worker threads for block compression; 0 = all available cores.
    pub threads: usize,
}

/// Formats [`encode_surface`] can produce.
pub fn can_encode(format: DdsFormat) -> bool {
    if cfg!(feature = "encode")
        && matches!(
            format,
            DdsFormat::Bc1 | DdsFormat::Bc3 | DdsFormat::Bc4 | DdsFormat::Bc5 | DdsFormat::Bc7
        )
    {
        return true;
    }
    matches!(
        format,
        DdsFormat::Rgba8
            | DdsFormat::Bgra8
            | DdsFormat::Bgrx8
            | DdsFormat::Bgr8
            | DdsFormat::R8
            | DdsFormat::A8
            | DdsFormat::Rg8
    )
}

/// Encode one RGBA8 surface into `format`.
///
/// Block-compressed targets need the `encode` feature (on by default); without it they
/// return [`Error::Unsupported`].
#[cfg_attr(not(feature = "encode"), allow(unused_variables))]
pub fn encode_surface(format: DdsFormat, img: &RgbaImage, opts: &EncodeOptions) -> Result<Vec<u8>> {
    if img.data.len() != img.width as usize * img.height as usize * 4
        || img.width == 0
        || img.height == 0
    {
        return Err(invalid(
            "RGBA image",
            format!(
                "bad size {}x{} / {} bytes",
                img.width,
                img.height,
                img.data.len()
            ),
        ));
    }
    let px = img.data.chunks_exact(4);
    Ok(match format {
        DdsFormat::Rgba8 => img.data.clone(),
        DdsFormat::Bgra8 => px.flat_map(|p| [p[2], p[1], p[0], p[3]]).collect(),
        DdsFormat::Bgrx8 => px.flat_map(|p| [p[2], p[1], p[0], 255]).collect(),
        DdsFormat::Bgr8 => px.flat_map(|p| [p[2], p[1], p[0]]).collect(),
        DdsFormat::R8 => px.map(|p| p[0]).collect(),
        DdsFormat::A8 => px.map(|p| p[3]).collect(),
        DdsFormat::Rg8 => px.flat_map(|p| [p[0], p[1]]).collect(),
        #[cfg(feature = "encode")]
        DdsFormat::Bc1 | DdsFormat::Bc3 | DdsFormat::Bc4 | DdsFormat::Bc5 | DdsFormat::Bc7 => {
            encode_bc(format, img, opts)
        }
        other => return Err(Error::Unsupported(format!("encoding to {other:?}"))),
    })
}

/// Pad to multiples of 4 by edge replication and keep `channels` bytes per pixel.
#[cfg(feature = "encode")]
fn padded_plane(img: &RgbaImage, channels: usize) -> (Vec<u8>, usize, usize) {
    let (w, h) = (img.width as usize, img.height as usize);
    let (pw, ph) = (w.div_ceil(4) * 4, h.div_ceil(4) * 4);
    let mut buf = Vec::with_capacity(pw * ph * channels);
    for y in 0..ph {
        let sy = y.min(h - 1);
        for x in 0..pw {
            let sx = x.min(w - 1);
            let s = (sy * w + sx) * 4;
            buf.extend_from_slice(&img.data[s..s + channels]);
        }
    }
    (buf, pw, ph)
}

#[cfg(feature = "encode")]
fn encode_bc(format: DdsFormat, img: &RgbaImage, opts: &EncodeOptions) -> Vec<u8> {
    let channels = match format {
        DdsFormat::Bc4 => 1,
        DdsFormat::Bc5 => 2,
        _ => 4,
    };
    let (buf, pw, ph) = padded_plane(img, channels);
    let bb = format.unit_bytes();
    let (bw, bh) = (pw / 4, ph / 4);
    let mut out = vec![0u8; bw * bh * bb];
    let alpha = channels == 4 && img.has_alpha();
    let bc7 = bc7_settings(opts.bc7_quality, alpha);

    let threads = if opts.threads == 0 {
        std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(1)
    } else {
        opts.threads
    };
    // Split into horizontal strips of block rows; each strip is an independent surface.
    let rows_per = bh.div_ceil(threads.max(1)).max(1);
    let stride = pw * channels;
    let strip_bytes = rows_per * bw * bb;
    std::thread::scope(|s| {
        for (i, dst) in out.chunks_mut(strip_bytes).enumerate() {
            let y0 = i * rows_per * 4;
            let rows = dst.len() / (bw * bb) * 4;
            let src = &buf[y0 * stride..(y0 + rows) * stride];
            let (w32, h32, st32) = (pw as u32, rows as u32, stride as u32);
            let bc7 = &bc7;
            s.spawn(move || match format {
                DdsFormat::Bc1 => intel_tex_2::bc1::compress_blocks_into(
                    &intel_tex_2::RgbaSurface {
                        data: src,
                        width: w32,
                        height: h32,
                        stride: st32,
                    },
                    dst,
                ),
                DdsFormat::Bc3 => intel_tex_2::bc3::compress_blocks_into(
                    &intel_tex_2::RgbaSurface {
                        data: src,
                        width: w32,
                        height: h32,
                        stride: st32,
                    },
                    dst,
                ),
                DdsFormat::Bc4 => intel_tex_2::bc4::compress_blocks_into(
                    &intel_tex_2::RSurface {
                        data: src,
                        width: w32,
                        height: h32,
                        stride: st32,
                    },
                    dst,
                ),
                DdsFormat::Bc5 => intel_tex_2::bc5::compress_blocks_into(
                    &intel_tex_2::RgSurface {
                        data: src,
                        width: w32,
                        height: h32,
                        stride: st32,
                    },
                    dst,
                ),
                _ => intel_tex_2::bc7::compress_blocks_into(
                    bc7,
                    &intel_tex_2::RgbaSurface {
                        data: src,
                        width: w32,
                        height: h32,
                        stride: st32,
                    },
                    dst,
                ),
            });
        }
    });
    out
}

#[cfg(feature = "encode")]
fn bc7_settings(q: Bc7Quality, alpha: bool) -> intel_tex_2::bc7::EncodeSettings {
    use intel_tex_2::bc7::*;
    match (q, alpha) {
        (Bc7Quality::UltraFast, false) => opaque_ultra_fast_settings(),
        (Bc7Quality::VeryFast, false) => opaque_very_fast_settings(),
        (Bc7Quality::Fast, false) => opaque_fast_settings(),
        (Bc7Quality::Basic, false) => opaque_basic_settings(),
        (Bc7Quality::Slow, false) => opaque_slow_settings(),
        (Bc7Quality::UltraFast, true) => alpha_ultra_fast_settings(),
        (Bc7Quality::VeryFast, true) => alpha_very_fast_settings(),
        (Bc7Quality::Fast, true) => alpha_fast_settings(),
        (Bc7Quality::Basic, true) => alpha_basic_settings(),
        (Bc7Quality::Slow, true) => alpha_slow_settings(),
    }
}

/// Number of mip levels of a full chain down to 1×1.
pub fn max_mip_count(w: u32, h: u32) -> u32 {
    32 - w.max(h).max(1).leading_zeros()
}

/// `count` levels starting with `img` itself, each a 2×2 box-filtered half of the previous.
pub fn generate_mips(img: &RgbaImage, count: u32) -> Vec<RgbaImage> {
    let count = count.clamp(1, max_mip_count(img.width, img.height));
    let mut levels = vec![img.clone()];
    while (levels.len() as u32) < count {
        let next = levels[levels.len() - 1].downsample();
        levels.push(next);
    }
    levels
}

/// Build a fresh DDS header in the style of the retail VR files:
///
/// * BC7/BC4/BC5/BC6H and DXGI-only formats: DX10 header (dimension 3 = 2D, array 1),
///   flags `0xA1007` (linear size = mip-0 bytes), caps `0x1000` (+`0x400008` with mips).
/// * BC1/BC2/BC3: legacy FourCC `DXT1`/`DXT3`/`DXT5`, same flags.
/// * RGBA8/BGRA8/BGRX8/BGR8: legacy RGB masks, flags `0x2100F` (pitch = width·bpp).
pub fn build_header(format: DdsFormat, width: u32, height: u32, mips: u32) -> Result<Vec<u8>> {
    let mips = mips.max(1);
    let legacy_masks: Option<(u32, [u32; 4])> = match format {
        DdsFormat::Rgba8 => Some((32, [0xFF, 0xFF00, 0xFF_0000, 0xFF00_0000])),
        DdsFormat::Bgra8 => Some((32, [0xFF_0000, 0xFF00, 0xFF, 0xFF00_0000])),
        DdsFormat::Bgrx8 => Some((32, [0xFF_0000, 0xFF00, 0xFF, 0])),
        DdsFormat::Bgr8 => Some((24, [0xFF_0000, 0xFF00, 0xFF, 0])),
        _ => None,
    };
    let legacy_fourcc: Option<&[u8; 4]> = match format {
        DdsFormat::Bc1 => Some(b"DXT1"),
        DdsFormat::Bc2 => Some(b"DXT3"),
        DdsFormat::Bc3 => Some(b"DXT5"),
        _ => None,
    };
    let dx10 = legacy_masks.is_none() && legacy_fourcc.is_none();
    if dx10 && format.dxgi().is_none() {
        return Err(Error::Unsupported(format!(
            "building a DDS header for {format:?}"
        )));
    }
    let mut h = vec![
        0u8;
        if dx10 {
            DDS_DX10_HEADER_LEN
        } else {
            DDS_HEADER_LEN
        }
    ];
    h[0..4].copy_from_slice(DDS_MAGIC);
    put_u32(&mut h, 0x04, 124);
    put_u32(&mut h, 0x0C, height);
    put_u32(&mut h, 0x10, width);
    put_u32(&mut h, 0x18, 1);
    put_u32(&mut h, 0x1C, mips);
    put_u32(&mut h, 0x4C, 32);
    if let Some((bits, masks)) = legacy_masks {
        put_u32(&mut h, 0x08, DDSD_REQUIRED | DDSD_PITCH | DDSD_MIPMAPCOUNT);
        put_u32(&mut h, 0x14, format.row_pitch(width) as u32);
        put_u32(
            &mut h,
            0x50,
            DDPF_RGB | if masks[3] != 0 { DDPF_ALPHAPIXELS } else { 0 },
        );
        put_u32(&mut h, 0x58, bits);
        for (i, m) in masks.iter().enumerate() {
            put_u32(&mut h, 0x5C + i * 4, *m);
        }
    } else {
        put_u32(
            &mut h,
            0x08,
            DDSD_REQUIRED | DDSD_LINEARSIZE | DDSD_MIPMAPCOUNT,
        );
        put_u32(&mut h, 0x14, format.surface_size(width, height) as u32);
        put_u32(&mut h, 0x50, DDPF_FOURCC);
        h[0x54..0x58].copy_from_slice(legacy_fourcc.unwrap_or(b"DX10"));
    }
    put_u32(
        &mut h,
        0x6C,
        DDSCAPS_TEXTURE
            | if mips > 1 {
                DDSCAPS_MIPMAP | DDSCAPS_COMPLEX
            } else {
                0
            },
    );
    if dx10 {
        put_u32(&mut h, 0x80, format.dxgi().unwrap_or(0));
        put_u32(&mut h, 0x84, 3); // D3D10_RESOURCE_DIMENSION_TEXTURE2D
        put_u32(&mut h, 0x8C, 1); // array size
    }
    Ok(h)
}

/// Copy `template`'s header (which must describe the same `format`) and patch only the
/// size, pitch/linear-size, mip count and mip caps — the safe way to rebuild a retail
/// payload (the DX11 loader is reportedly sensitive to header details).
pub fn patch_header(
    template: &[u8],
    format: DdsFormat,
    width: u32,
    height: u32,
    mips: u32,
) -> Result<Vec<u8>> {
    let info = DdsInfo::parse(template)?;
    if info.format != format {
        return Err(invalid(
            "DDS template",
            format!(
                "template format {:?} differs from {:?}",
                info.format, format
            ),
        ));
    }
    if info.faces != 1 {
        return Err(Error::Unsupported(
            "patching cube-map / array DDS headers".into(),
        ));
    }
    let mips = mips.max(1);
    let mut h = template[..info.header_len].to_vec();
    put_u32(&mut h, 0x0C, height);
    put_u32(&mut h, 0x10, width);
    let mut flags = info.flags;
    if mips > 1 {
        flags |= DDSD_MIPMAPCOUNT;
    }
    put_u32(&mut h, 0x08, flags);
    if flags & DDSD_PITCH != 0 {
        put_u32(&mut h, 0x14, format.row_pitch(width) as u32);
    } else if flags & DDSD_LINEARSIZE != 0 {
        put_u32(&mut h, 0x14, format.surface_size(width, height) as u32);
    }
    let raw_mips = if info.raw_mip_count == 0 && mips == 1 {
        0
    } else {
        mips
    };
    put_u32(&mut h, 0x1C, raw_mips);
    let caps = if mips > 1 {
        info.caps | DDSCAPS_MIPMAP | DDSCAPS_COMPLEX
    } else {
        info.caps & !(DDSCAPS_MIPMAP | DDSCAPS_COMPLEX)
    };
    put_u32(&mut h, 0x6C, caps);
    Ok(h)
}

/// Encode `img` plus `mips - 1` generated mip levels into a complete DDS blob.
///
/// With `template`, the template's header is reused via [`patch_header`]; otherwise a
/// fresh header comes from [`build_header`].
pub fn encode_dds(
    img: &RgbaImage,
    format: DdsFormat,
    mips: u32,
    template: Option<&[u8]>,
    opts: &EncodeOptions,
) -> Result<Vec<u8>> {
    if !can_encode(format) {
        return Err(Error::Unsupported(format!("encoding to {format:?}")));
    }
    let levels = generate_mips(img, mips);
    let n = levels.len() as u32;
    let mut out = match template {
        Some(t) => patch_header(t, format, img.width, img.height, n)?,
        None => build_header(format, img.width, img.height, n)?,
    };
    for level in &levels {
        out.extend_from_slice(&encode_surface(format, level, opts)?);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn gradient(w: u32, h: u32, alpha: bool) -> RgbaImage {
        let mut img = RgbaImage::new(w, h);
        for y in 0..h {
            for x in 0..w {
                let i = ((y * w + x) * 4) as usize;
                img.data[i] = (x * 255 / w.max(2).saturating_sub(1).max(1)) as u8;
                img.data[i + 1] = (y * 255 / h.max(2).saturating_sub(1).max(1)) as u8;
                img.data[i + 2] = 128;
                img.data[i + 3] = if alpha {
                    ((x + y) * 255 / (w + h)) as u8
                } else {
                    255
                };
            }
        }
        img
    }

    fn max_diff(a: &RgbaImage, b: &RgbaImage) -> u8 {
        a.data
            .iter()
            .zip(&b.data)
            .map(|(x, y)| x.abs_diff(*y))
            .max()
            .unwrap_or(0)
    }

    fn mean_diff(a: &RgbaImage, b: &RgbaImage) -> f64 {
        a.data
            .iter()
            .zip(&b.data)
            .map(|(x, y)| x.abs_diff(*y) as f64)
            .sum::<f64>()
            / a.data.len() as f64
    }

    #[test]
    fn raw_formats_round_trip_exactly() {
        let img = gradient(13, 9, true);
        for f in [DdsFormat::Rgba8, DdsFormat::Bgra8] {
            let dds = encode_dds(&img, f, 4, None, &EncodeOptions::default()).unwrap();
            let info = DdsInfo::parse(&dds).unwrap();
            assert_eq!(info.format, f);
            assert_eq!(info.mip_count, 4);
            assert_eq!(info.expected_len(), dds.len());
            assert_eq!(decode(&dds, 0).unwrap(), img);
            let m3 = decode(&dds, 3).unwrap();
            assert_eq!((m3.width, m3.height), (1, 1));
        }
    }

    #[test]
    #[cfg(feature = "encode")]
    fn bc_formats_encode_and_decode() {
        let img = gradient(37, 21, true); // NPOT, not a multiple of 4
        let opts = EncodeOptions {
            bc7_quality: Bc7Quality::Fast,
            threads: 3,
        };
        for (f, tol) in [
            (DdsFormat::Bc7, 4.0),
            (DdsFormat::Bc3, 6.0),
            (DdsFormat::Bc1, 64.0),
        ] {
            let dds = encode_dds(&img, f, 1, None, &opts).unwrap();
            let info = DdsInfo::parse(&dds).unwrap();
            assert_eq!((info.width, info.height, info.format), (37, 21, f));
            assert_eq!(dds.len(), info.expected_len());
            let back = decode(&dds, 0).unwrap();
            assert_eq!((back.width, back.height), (37, 21));
            assert!(
                mean_diff(&img, &back) <= tol,
                "{f:?} mean diff {}",
                mean_diff(&img, &back)
            );
        }
        let opaque = gradient(16, 16, false);
        let back = decode(
            &encode_dds(&opaque, DdsFormat::Bc1, 1, None, &opts).unwrap(),
            0,
        )
        .unwrap();
        assert!(
            mean_diff(&opaque, &back) <= 10.0,
            "BC1 mean diff {}",
            mean_diff(&opaque, &back)
        );
        assert!(max_diff(&opaque, &back) <= 64);
        let back = decode(
            &encode_dds(&opaque, DdsFormat::Bc4, 1, None, &opts).unwrap(),
            0,
        )
        .unwrap();
        assert!(
            back.data
                .chunks(4)
                .zip(opaque.data.chunks(4))
                .all(|(b, o)| b[0].abs_diff(o[0]) <= 8)
        );
        let back = decode(
            &encode_dds(&opaque, DdsFormat::Bc5, 1, None, &opts).unwrap(),
            0,
        )
        .unwrap();
        assert!(
            back.data
                .chunks(4)
                .zip(opaque.data.chunks(4))
                .all(|(b, o)| b[1].abs_diff(o[1]) <= 8)
        );
    }

    #[test]
    #[cfg(feature = "encode")]
    fn patch_header_keeps_template_bytes() {
        let img = gradient(8, 8, false);
        let mut t = build_header(DdsFormat::Bc7, 16, 16, 1).unwrap();
        t[0x20] = 0xAB; // reserved byte must survive
        t.extend(vec![0u8; 16 * 16]);
        let dds = encode_dds(&img, DdsFormat::Bc7, 2, Some(&t), &EncodeOptions::default()).unwrap();
        assert_eq!(dds[0x20], 0xAB);
        let info = DdsInfo::parse(&dds).unwrap();
        assert_eq!((info.width, info.height, info.mip_count), (8, 8, 2));
        assert_eq!(u32::from_le_bytes(dds[0x14..0x18].try_into().unwrap()), 64);
        assert!(patch_header(&t, DdsFormat::Bc3, 8, 8, 1).is_err());
    }

    #[test]
    fn garbage_is_rejected_without_panic() {
        assert!(DdsInfo::parse(b"").is_err());
        assert!(DdsInfo::parse(b"DDS ").is_err());
        assert!(matches!(
            DdsInfo::parse(b"NXTCH000aaaa"),
            Err(Error::Unsupported(_))
        ));
        let mut h = build_header(DdsFormat::Bc7, 64, 64, 1).unwrap();
        assert!(decode(&h, 0).is_err()); // no pixel data
        h[0x80] = 200; // unknown DXGI
        assert!(matches!(DdsInfo::parse(&h), Err(Error::Unsupported(_))));
        // random bytes decode as blocks without panicking
        let noise: Vec<u8> = (0..16 * 64).map(|i| (i * 131 % 251) as u8).collect();
        for f in [
            DdsFormat::Bc1,
            DdsFormat::Bc2,
            DdsFormat::Bc3,
            DdsFormat::Bc4Snorm,
            DdsFormat::Bc5,
            DdsFormat::Bc6hSf16,
            DdsFormat::Bc7,
        ] {
            decode_surface(f, &noise, 30, 30).unwrap();
        }
    }

    #[test]
    fn half_float() {
        assert_eq!(half_to_f32(0x3C00), 1.0);
        assert_eq!(half_to_f32(0xC000), -2.0);
        assert_eq!(unit_to_u8(half_to_f32(0x3800)), 128);
        assert_eq!(max_mip_count(4096, 2048), 13);
        assert_eq!(max_mip_count(1, 1), 1);
    }
}
