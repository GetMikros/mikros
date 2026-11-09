//! Reading uncompressed BMP images and scaling them down.

use alloc::vec;
use alloc::vec::Vec;

/// An image with pixels as `0xAARRGGBB`, in rows from the top.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Image {
    pub width: usize,
    pub height: usize,
    pub pixels: Vec<u32>,
}

/// Why a BMP file could not be read.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    NotBmp,
    Truncated,
    Unsupported,
}

/// The largest width or height accepted, to bound memory use.
const MAX_SIDE: usize = 4096;

/// Compression values of uncompressed images.
const BI_RGB: u32 = 0;
const BI_BITFIELDS: u32 = 3;

fn u16_at(data: &[u8], offset: usize) -> Result<u16, Error> {
    let bytes = data.get(offset..offset + 2).ok_or(Error::Truncated)?;
    Ok(u16::from_le_bytes([bytes[0], bytes[1]]))
}

fn u32_at(data: &[u8], offset: usize) -> Result<u32, Error> {
    let bytes = data.get(offset..offset + 4).ok_or(Error::Truncated)?;
    Ok(u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
}

/// Extracts the channel selected by `mask` from `value`, scaled to 8 bits.
fn channel(value: u32, mask: u32) -> u8 {
    if mask == 0 {
        return 0;
    }
    let shift = mask.trailing_zeros();
    let max = mask >> shift;
    ((u64::from((value & mask) >> shift) * 255 + u64::from(max) / 2) / u64::from(max)) as u8
}

/// Decodes a BMP file with a BITMAPINFOHEADER or later header and an
/// uncompressed pixel array.
pub fn decode(data: &[u8]) -> Result<Image, Error> {
    if data.get(0..2) != Some(b"BM") {
        return Err(Error::NotBmp);
    }
    let pixel_offset = u32_at(data, 10)? as usize;
    let header_size = u32_at(data, 14)? as usize;
    if header_size < 40 {
        return Err(Error::Unsupported);
    }
    let width = u32_at(data, 18)? as i32;
    let height = u32_at(data, 22)? as i32;
    let bits = u16_at(data, 28)?;
    let compression = u32_at(data, 30)?;
    let colors_used = u32_at(data, 46)? as usize;

    let top_down = height < 0;
    let (width, height) = (
        width.unsigned_abs() as usize,
        height.unsigned_abs() as usize,
    );
    if width == 0 || height == 0 || width > MAX_SIDE || height > MAX_SIDE {
        return Err(Error::Unsupported);
    }

    let masks = match (compression, bits) {
        (BI_RGB, 16) => (0x7c00, 0x03e0, 0x001f, 0),
        (BI_RGB, 32) => (0x00ff_0000, 0x0000_ff00, 0x0000_00ff, 0),
        (BI_RGB, 1 | 4 | 8 | 24) => (0, 0, 0, 0),
        (BI_BITFIELDS, 16 | 32) => {
            let alpha = if header_size >= 56 {
                u32_at(data, 54 + 12)?
            } else {
                0
            };
            (
                u32_at(data, 54)?,
                u32_at(data, 58)?,
                u32_at(data, 62)?,
                alpha,
            )
        }
        _ => return Err(Error::Unsupported),
    };

    let palette: Vec<u32> = if bits <= 8 {
        let count = if colors_used == 0 {
            1 << bits
        } else {
            colors_used.min(1 << bits)
        };
        let start = 14 + header_size;
        (0..count)
            .map(|i| u32_at(data, start + 4 * i).map(|c| 0xff00_0000 | (c & 0x00ff_ffff)))
            .collect::<Result<_, _>>()?
    } else {
        Vec::new()
    };

    let stride = (width * usize::from(bits)).div_ceil(32) * 4;
    let mut pixels = vec![0u32; width * height];
    for row in 0..height {
        let source_row = if top_down { row } else { height - 1 - row };
        let start = pixel_offset + source_row * stride;
        let line = data.get(start..start + stride).ok_or(Error::Truncated)?;
        for x in 0..width {
            pixels[row * width + x] = match bits {
                1 | 4 | 8 => {
                    let bit = x * usize::from(bits);
                    let byte = line[bit / 8];
                    let shift = 8 - usize::from(bits) - bit % 8;
                    let index = usize::from(byte >> shift) & ((1 << bits) - 1);
                    *palette.get(index).ok_or(Error::Unsupported)?
                }
                24 => {
                    let p = &line[x * 3..x * 3 + 3];
                    0xff00_0000 | u32::from(p[2]) << 16 | u32::from(p[1]) << 8 | u32::from(p[0])
                }
                _ => {
                    let value = if bits == 16 {
                        u32::from(u16::from_le_bytes([line[x * 2], line[x * 2 + 1]]))
                    } else {
                        u32::from_le_bytes([
                            line[x * 4],
                            line[x * 4 + 1],
                            line[x * 4 + 2],
                            line[x * 4 + 3],
                        ])
                    };
                    let (r, g, b, a) = masks;
                    let alpha = if a == 0 { 255 } else { channel(value, a) };
                    u32::from(alpha) << 24
                        | u32::from(channel(value, r)) << 16
                        | u32::from(channel(value, g)) << 8
                        | u32::from(channel(value, b))
                }
            };
        }
    }
    Ok(Image {
        width,
        height,
        pixels,
    })
}

impl Image {
    /// Scales the image down to fit within `max` × `max`, keeping its aspect
    /// ratio, by averaging the source pixels each target pixel covers. An
    /// image that already fits is returned unchanged.
    pub fn fit_within(self, max: usize) -> Image {
        if self.width <= max && self.height <= max {
            return self;
        }
        let (width, height) = if self.width >= self.height {
            (max, (self.height * max / self.width).max(1))
        } else {
            ((self.width * max / self.height).max(1), max)
        };
        let mut pixels = Vec::with_capacity(width * height);
        for y in 0..height {
            let (y0, y1) = (
                y * self.height / height,
                ((y + 1) * self.height / height).max(y * self.height / height + 1),
            );
            for x in 0..width {
                let (x0, x1) = (
                    x * self.width / width,
                    ((x + 1) * self.width / width).max(x * self.width / width + 1),
                );
                let mut sum = [0u64; 4];
                for sy in y0..y1 {
                    for sx in x0..x1 {
                        let p = self.pixels[sy * self.width + sx];
                        for (i, s) in sum.iter_mut().enumerate() {
                            *s += u64::from((p >> (24 - 8 * i)) & 0xff);
                        }
                    }
                }
                let n = ((y1 - y0) * (x1 - x0)) as u64;
                let avg = |i: usize| ((sum[i] + n / 2) / n) as u32;
                pixels.push(avg(0) << 24 | avg(1) << 16 | avg(2) << 8 | avg(3));
            }
        }
        Image {
            width,
            height,
            pixels,
        }
    }
}

#[cfg(test)]
mod tests;
