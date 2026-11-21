//! The haze of an output (MIKSPEC-0066.R18): a copy of its wallpaper,
//! blurred and tinted with the theme's scheme, made on the CPU only when
//! the wallpaper or the scheme changes. The copy is made at a quarter of
//! the output's size, blurred there with three box blurs, which together
//! come close to a Gaussian, tinted, and scaled back up to the output's
//! size with bilinear filtering, so that the renderer takes the pixel at
//! an output position as it is. Its values follow the shell's designs: a
//! background blur of 96 pixels, and white at 70% and then at 30% over it
//! in the light scheme, black at 65% and then at 32% in the dark one.

use crate::theme::Scheme;
use std::time::Duration;

/// How long a new haze takes to replace the old one.
pub const FADE: Duration = Duration::from_millis(250);

/// The factor by which the copy is smaller than the output while it is
/// blurred.
const SCALE: usize = 4;

/// The radius of each box blur at the reduced size. Three boxes 25 pixels
/// wide come close to a Gaussian of a standard deviation of 12 pixels
/// there, 48 at the output's size, which a blur of 96 means.
const RADIUS: usize = 12;

/// The colour of the tint and how much of each pixel it covers.
fn tint(scheme: Scheme) -> (f32, f32) {
    match scheme {
        // 1 - (1 - 0.70) * (1 - 0.30)
        Scheme::Light => (255.0, 0.79),
        // 1 - (1 - 0.65) * (1 - 0.32)
        Scheme::Dark => (0.0, 0.762),
    }
}

/// The flat colour, as XRGB8888, under haze of a window that is not
/// focused and on an output without a wallpaper.
pub fn flat(scheme: Scheme) -> u32 {
    match scheme {
        Scheme::Light => 0x00ed_eded,
        Scheme::Dark => 0x0011_1111,
    }
}

/// The translucent colour, as premultiplied ARGB8888, under blur, which
/// the CPU driver draws instead of blurring what lies beneath: the first
/// layer of the haze's fill, white at 70% or black at 65%.
pub fn translucent(scheme: Scheme) -> u32 {
    match scheme {
        Scheme::Light => 0xb3b3_b3b3,
        Scheme::Dark => 0xa600_0000,
    }
}

/// The haze of a wallpaper of `width` by `height` XRGB8888 pixels with
/// `stride` pixels a row, as `width * height` XRGB8888 pixels.
pub fn haze(
    pixels: &[u32],
    width: usize,
    height: usize,
    stride: usize,
    scheme: Scheme,
) -> Vec<u32> {
    if width == 0 || height == 0 {
        return Vec::new();
    }
    let (w, h) = (width.div_ceil(SCALE), height.div_ceil(SCALE));
    let mut small = reduce(pixels, width, height, stride);
    for _ in 0..3 {
        small = blur(&small, w, h, 1, w);
        small = blur(&small, h, w, w, 1);
    }
    let (colour, share) = tint(scheme);
    for pixel in &mut small {
        for c in pixel.iter_mut() {
            *c = *c * (1.0 - share) + colour * share;
        }
    }
    enlarge(&small, w, h, width, height)
}

/// How much of the new haze a frame shows `elapsed` after it was made.
pub fn faded_in(elapsed: Duration) -> f32 {
    (elapsed.as_secs_f32() / FADE.as_secs_f32()).min(1.0)
}

/// The red, green and blue of an XRGB8888 pixel.
fn channels(pixel: u32) -> [f32; 3] {
    [(pixel >> 16) & 0xff, (pixel >> 8) & 0xff, pixel & 0xff].map(|c| c as f32)
}

fn pixel(c: [f32; 3]) -> u32 {
    let [r, g, b] = c.map(|c| c.round().clamp(0.0, 255.0) as u32);
    (r << 16) | (g << 8) | b
}

/// The averages of the blocks of SCALE by SCALE pixels, the last ones of
/// a row or a column smaller where the size is not a multiple of it.
fn reduce(pixels: &[u32], width: usize, height: usize, stride: usize) -> Vec<[f32; 3]> {
    let (w, h) = (width.div_ceil(SCALE), height.div_ceil(SCALE));
    let mut small = vec![[0.0; 3]; w * h];
    for (i, out) in small.iter_mut().enumerate() {
        let (x0, y0) = ((i % w) * SCALE, (i / w) * SCALE);
        let (x1, y1) = ((x0 + SCALE).min(width), (y0 + SCALE).min(height));
        let mut sum = [0.0; 3];
        for y in y0..y1 {
            for &p in &pixels[y * stride + x0..y * stride + x1] {
                let c = channels(p);
                (0..3).for_each(|k| sum[k] += c[k]);
            }
        }
        let n = ((x1 - x0) * (y1 - y0)) as f32;
        *out = sum.map(|s| s / n);
    }
    small
}

/// A box blur of RADIUS along `lines` lines of `length` pixels, a pixel
/// `step` apart and a line `next` apart, with the edge pixels repeated
/// beyond the edges.
fn blur(src: &[[f32; 3]], length: usize, lines: usize, step: usize, next: usize) -> Vec<[f32; 3]> {
    let mut out = vec![[0.0; 3]; src.len()];
    let width = (2 * RADIUS + 1) as f32;
    let at = |line: usize, i: isize| {
        let i = i.clamp(0, length as isize - 1) as usize;
        src[line * next + i * step]
    };
    for line in 0..lines {
        let mut sum = [0.0; 3];
        for i in -(RADIUS as isize)..=RADIUS as isize {
            let c = at(line, i);
            (0..3).for_each(|k| sum[k] += c[k]);
        }
        for i in 0..length {
            out[line * next + i * step] = sum.map(|s| s / width);
            let (enter, leave) = (
                at(line, (i + RADIUS + 1) as isize),
                at(line, i as isize - RADIUS as isize),
            );
            (0..3).for_each(|k| sum[k] += enter[k] - leave[k]);
        }
    }
    out
}

/// `small`, `w` by `h`, scaled to `width` by `height` with bilinear
/// filtering between the centres of its pixels.
fn enlarge(small: &[[f32; 3]], w: usize, h: usize, width: usize, height: usize) -> Vec<u32> {
    let place = |x: usize, n: usize| {
        let u = ((x as f32 + 0.5) / SCALE as f32 - 0.5).clamp(0.0, (n - 1) as f32);
        let i = (u as usize).min(n.saturating_sub(2));
        (i, (i + 1).min(n - 1), u - i as f32)
    };
    let columns: Vec<_> = (0..width).map(|x| place(x, w)).collect();
    let mut out = Vec::with_capacity(width * height);
    for y in 0..height {
        let (y0, y1, fy) = place(y, h);
        for &(x0, x1, fx) in &columns {
            let mix =
                |a: [f32; 3], b: [f32; 3], f: f32| [0, 1, 2].map(|k| a[k] + (b[k] - a[k]) * f);
            let top = mix(small[y0 * w + x0], small[y0 * w + x1], fx);
            let bottom = mix(small[y1 * w + x0], small[y1 * w + x1], fx);
            out.push(pixel(mix(top, bottom, fy)));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const COLOUR: u32 = 0x0028_5aa0;

    #[test]
    fn tints_a_flat_wallpaper_evenly() {
        let wallpaper = vec![COLOUR; 37 * 21];
        let light = haze(&wallpaper, 37, 21, 37, Scheme::Light);
        assert_eq!(light.len(), 37 * 21);
        assert!(light.iter().all(|&p| p == 0x00d2_dceb), "{:x}", light[0]);
        let dark = haze(&wallpaper, 37, 21, 37, Scheme::Dark);
        assert!(dark.iter().all(|&p| p == 0x000a_1526), "{:x}", dark[0]);
    }

    #[test]
    fn skips_the_padding_of_rows() {
        let mut wallpaper = vec![0x00ff_0000; 16 * 8];
        for row in wallpaper.chunks_mut(16) {
            row[..12].fill(COLOUR);
        }
        let hazed = haze(&wallpaper, 12, 8, 16, Scheme::Dark);
        assert!(hazed.iter().all(|&p| p == 0x000a_1526));
    }

    #[test]
    fn blurs_an_edge_smoothly() {
        let (width, height) = (400, 40);
        let wallpaper: Vec<u32> = (0..width * height)
            .map(|i| {
                if i % width < width / 2 {
                    0
                } else {
                    0x00ff_ffff
                }
            })
            .collect();
        let hazed = haze(&wallpaper, width, height, width, Scheme::Dark);
        let row: Vec<u32> = hazed[..width].iter().map(|p| p & 0xff).collect();
        assert_eq!(row[0], 0);
        assert_eq!(row[width - 1], 61);
        assert!(row.windows(2).all(|w| w[0] <= w[1]));
        // The blur spreads the edge over far more than one pixel.
        let between = row.iter().filter(|&&c| c > 2 && c < 59).count();
        assert!(between > 100, "{between}");
        assert!(
            (row[width / 2] as i32 - 30).abs() <= 2,
            "{}",
            row[width / 2]
        );
    }

    #[test]
    fn fades_in_over_a_quarter_second() {
        assert_eq!(faded_in(Duration::ZERO), 0.0);
        assert_eq!(faded_in(Duration::from_millis(125)), 0.5);
        assert_eq!(faded_in(Duration::from_secs(1)), 1.0);
    }
}
