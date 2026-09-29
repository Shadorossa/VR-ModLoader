//! Simple owned RGBA8 / 8-bit gray images with PNG import/export.

use std::io::Cursor;
use std::path::Path;

use image::{ExtendedColorType, ImageEncoder, ImageFormat, codecs::png::PngEncoder};

use crate::error::{Result, invalid};

/// Tightly packed RGBA8 image, rows top to bottom.
#[derive(Clone, PartialEq, Eq)]
pub struct RgbaImage {
    /// Width in pixels.
    pub width: u32,
    /// Height in pixels.
    pub height: u32,
    /// `width * height * 4` bytes, R G B A per pixel.
    pub data: Vec<u8>,
}

impl std::fmt::Debug for RgbaImage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "RgbaImage({}x{})", self.width, self.height)
    }
}

impl RgbaImage {
    /// Transparent black image.
    pub fn new(width: u32, height: u32) -> Self {
        Self {
            width,
            height,
            data: vec![0; width as usize * height as usize * 4],
        }
    }

    /// Wrap existing RGBA8 data; the length must be `width * height * 4`.
    pub fn from_raw(width: u32, height: u32, data: Vec<u8>) -> Result<Self> {
        if data.len() != width as usize * height as usize * 4 {
            return Err(invalid(
                "RGBA image",
                format!(
                    "{}x{} needs {} bytes, got {}",
                    width,
                    height,
                    width as usize * height as usize * 4,
                    data.len()
                ),
            ));
        }
        Ok(Self {
            width,
            height,
            data,
        })
    }

    /// RGBA of one pixel, `None` outside the image.
    pub fn pixel(&self, x: u32, y: u32) -> Option<[u8; 4]> {
        if x >= self.width || y >= self.height {
            return None;
        }
        let i = (y as usize * self.width as usize + x as usize) * 4;
        Some([
            self.data[i],
            self.data[i + 1],
            self.data[i + 2],
            self.data[i + 3],
        ])
    }

    /// Copy of the rectangle `(x, y, w, h)` clipped to the image. Negative or oversized
    /// rectangles are clipped, so the result may be smaller (or empty).
    pub fn crop(&self, x: i64, y: i64, w: i64, h: i64) -> RgbaImage {
        let x0 = x.clamp(0, self.width as i64) as usize;
        let y0 = y.clamp(0, self.height as i64) as usize;
        let x1 = (x.saturating_add(w.max(0))).clamp(0, self.width as i64) as usize;
        let y1 = (y.saturating_add(h.max(0))).clamp(0, self.height as i64) as usize;
        let (cw, ch) = (x1.saturating_sub(x0), y1.saturating_sub(y0));
        let mut out = Vec::with_capacity(cw * ch * 4);
        let stride = self.width as usize * 4;
        for row in y0..y1 {
            let s = row * stride + x0 * 4;
            out.extend_from_slice(&self.data[s..s + cw * 4]);
        }
        RgbaImage {
            width: cw as u32,
            height: ch as u32,
            data: out,
        }
    }

    /// Paste `src` with its top-left corner at `(x, y)`, replacing pixels (no blending).
    /// Parts falling outside `self` are clipped.
    pub fn blit(&mut self, src: &RgbaImage, x: i64, y: i64) {
        let dst_stride = self.width as usize * 4;
        let src_stride = src.width as usize * 4;
        for sy in 0..src.height as i64 {
            let dy = y + sy;
            if dy < 0 || dy >= self.height as i64 {
                continue;
            }
            let sx0 = (-x).clamp(0, src.width as i64);
            let sx1 = (self.width as i64 - x).clamp(0, src.width as i64);
            if sx0 >= sx1 {
                continue;
            }
            let n = (sx1 - sx0) as usize * 4;
            let s = sy as usize * src_stride + sx0 as usize * 4;
            let d = dy as usize * dst_stride + (x + sx0) as usize * 4;
            self.data[d..d + n].copy_from_slice(&src.data[s..s + n]);
        }
    }

    /// Next mip level: `max(1, w/2) x max(1, h/2)`, 2×2 box filter (edge pixels are
    /// reused when a dimension is odd or already 1).
    pub fn downsample(&self) -> RgbaImage {
        let (w, h) = (self.width.max(1), self.height.max(1));
        let (nw, nh) = ((w / 2).max(1), (h / 2).max(1));
        let mut out = RgbaImage::new(nw, nh);
        if self.data.is_empty() {
            return out;
        }
        let sw = self.width as usize;
        for y in 0..nh as usize {
            let y0 = (y * 2).min(self.height as usize - 1);
            let y1 = (y * 2 + 1).min(self.height as usize - 1);
            for x in 0..nw as usize {
                let x0 = (x * 2).min(sw - 1);
                let x1 = (x * 2 + 1).min(sw - 1);
                let o = (y * nw as usize + x) * 4;
                for c in 0..4 {
                    let s = self.data[(y0 * sw + x0) * 4 + c] as u32
                        + self.data[(y0 * sw + x1) * 4 + c] as u32
                        + self.data[(y1 * sw + x0) * 4 + c] as u32
                        + self.data[(y1 * sw + x1) * 4 + c] as u32;
                    out.data[o + c] = ((s + 2) / 4) as u8;
                }
            }
        }
        out
    }

    /// True if any pixel has alpha < 255.
    pub fn has_alpha(&self) -> bool {
        self.data.chunks_exact(4).any(|p| p[3] != 255)
    }

    /// Decode a PNG (any colour type) into RGBA8.
    pub fn from_png_bytes(bytes: &[u8]) -> Result<Self> {
        let img = image::load_from_memory_with_format(bytes, ImageFormat::Png)?.into_rgba8();
        let (w, h) = img.dimensions();
        Ok(Self {
            width: w,
            height: h,
            data: img.into_raw(),
        })
    }

    /// Encode as an RGBA8 PNG.
    pub fn to_png_bytes(&self) -> Result<Vec<u8>> {
        encode_png(
            &self.data,
            self.width,
            self.height,
            ExtendedColorType::Rgba8,
        )
    }

    /// Read a PNG file.
    pub fn load_png(path: impl AsRef<Path>) -> Result<Self> {
        Self::from_png_bytes(&std::fs::read(path)?)
    }

    /// Write a PNG file.
    pub fn save_png(&self, path: impl AsRef<Path>) -> Result<()> {
        std::fs::write(path, self.to_png_bytes()?)?;
        Ok(())
    }
}

/// Tightly packed 8-bit single-channel image (used for font pages).
#[derive(Clone, PartialEq, Eq)]
pub struct GrayImage {
    /// Width in pixels.
    pub width: u32,
    /// Height in pixels.
    pub height: u32,
    /// `width * height` bytes.
    pub data: Vec<u8>,
}

impl std::fmt::Debug for GrayImage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "GrayImage({}x{})", self.width, self.height)
    }
}

impl GrayImage {
    /// Wrap existing data; the length must be `width * height`.
    pub fn from_raw(width: u32, height: u32, data: Vec<u8>) -> Result<Self> {
        if data.len() != width as usize * height as usize {
            return Err(invalid(
                "gray image",
                format!(
                    "{}x{} needs {} bytes, got {}",
                    width,
                    height,
                    width as usize * height as usize,
                    data.len()
                ),
            ));
        }
        Ok(Self {
            width,
            height,
            data,
        })
    }

    /// Decode a PNG and convert it to 8-bit luma.
    pub fn from_png_bytes(bytes: &[u8]) -> Result<Self> {
        let img = image::load_from_memory_with_format(bytes, ImageFormat::Png)?.into_luma8();
        let (w, h) = img.dimensions();
        Ok(Self {
            width: w,
            height: h,
            data: img.into_raw(),
        })
    }

    /// Encode as an 8-bit grayscale PNG.
    pub fn to_png_bytes(&self) -> Result<Vec<u8>> {
        encode_png(&self.data, self.width, self.height, ExtendedColorType::L8)
    }

    /// Read a PNG file as grayscale.
    pub fn load_png(path: impl AsRef<Path>) -> Result<Self> {
        Self::from_png_bytes(&std::fs::read(path)?)
    }

    /// Write a PNG file.
    pub fn save_png(&self, path: impl AsRef<Path>) -> Result<()> {
        std::fs::write(path, self.to_png_bytes()?)?;
        Ok(())
    }
}

fn encode_png(data: &[u8], w: u32, h: u32, ct: ExtendedColorType) -> Result<Vec<u8>> {
    let mut out = Cursor::new(Vec::new());
    PngEncoder::new(&mut out).write_image(data, w, h, ct)?;
    Ok(out.into_inner())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn checker(w: u32, h: u32) -> RgbaImage {
        let mut img = RgbaImage::new(w, h);
        for (i, p) in img.data.chunks_exact_mut(4).enumerate() {
            let v = (i * 7 % 256) as u8;
            p.copy_from_slice(&[v, 255 - v, v / 2, if i % 3 == 0 { 128 } else { 255 }]);
        }
        img
    }

    #[test]
    fn png_round_trip() {
        let img = checker(13, 7);
        let png = img.to_png_bytes().unwrap();
        assert_eq!(RgbaImage::from_png_bytes(&png).unwrap(), img);
        let g = GrayImage::from_raw(3, 2, vec![0, 50, 100, 150, 200, 250]).unwrap();
        assert_eq!(
            GrayImage::from_png_bytes(&g.to_png_bytes().unwrap()).unwrap(),
            g
        );
        assert!(RgbaImage::from_png_bytes(b"not a png").is_err());
    }

    #[test]
    fn crop_and_blit() {
        let img = checker(10, 10);
        let c = img.crop(8, 8, 5, 5);
        assert_eq!((c.width, c.height), (2, 2));
        assert_eq!(c.pixel(0, 0), img.pixel(8, 8));
        let e = img.crop(-5, -5, 3, 3);
        assert_eq!((e.width, e.height), (0, 0));
        let mut dst = RgbaImage::new(4, 4);
        dst.blit(&img, -8, -8);
        assert_eq!(dst.pixel(0, 0), img.pixel(8, 8));
        assert_eq!(dst.pixel(2, 2), Some([0; 4]));
    }

    #[test]
    fn downsample_dims() {
        let img = checker(5, 1);
        let d = img.downsample();
        assert_eq!((d.width, d.height), (2, 1));
        let d = d.downsample().downsample();
        assert_eq!((d.width, d.height), (1, 1));
    }
}
