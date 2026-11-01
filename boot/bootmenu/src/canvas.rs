//! An off-screen image that the menu is drawn into before it is copied to the
//! framebuffer.

use alloc::vec;
use alloc::vec::Vec;

/// A color as `0xRRGGBB`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Color(pub u32);

impl Color {
    pub const fn rgb(r: u8, g: u8, b: u8) -> Color {
        Color((r as u32) << 16 | (g as u32) << 8 | b as u32)
    }

    pub const fn r(self) -> u8 {
        (self.0 >> 16) as u8
    }

    pub const fn g(self) -> u8 {
        (self.0 >> 8) as u8
    }

    pub const fn b(self) -> u8 {
        self.0 as u8
    }

    /// Mixes `self` over `under` with coverage `alpha`, from 0 (only `under`)
    /// to 255 (only `self`).
    pub fn over(self, under: Color, alpha: u8) -> Color {
        let mix = |top: u8, bottom: u8| {
            let a = u32::from(alpha);
            ((u32::from(top) * a + u32::from(bottom) * (255 - a) + 127) / 255) as u8
        };
        Color::rgb(
            mix(self.r(), under.r()),
            mix(self.g(), under.g()),
            mix(self.b(), under.b()),
        )
    }
}

/// A `width` × `height` image of `Color` pixels, in rows from the top.
pub struct Canvas {
    width: usize,
    height: usize,
    pixels: Vec<Color>,
}

impl Canvas {
    pub fn new(width: usize, height: usize) -> Canvas {
        Canvas {
            width,
            height,
            pixels: vec![Color(0); width * height],
        }
    }

    pub fn width(&self) -> usize {
        self.width
    }

    pub fn height(&self) -> usize {
        self.height
    }

    /// The pixels of row `y`.
    pub fn row(&self, y: usize) -> &[Color] {
        &self.pixels[y * self.width..(y + 1) * self.width]
    }

    pub fn get(&self, x: usize, y: usize) -> Color {
        self.pixels[y * self.width + x]
    }

    pub fn clear(&mut self, color: Color) {
        self.pixels.fill(color);
    }

    /// Mixes `color` into the pixel at (`x`, `y`) with coverage `alpha`.
    /// Pixels outside the canvas are ignored.
    pub fn blend(&mut self, x: i32, y: i32, color: Color, alpha: u8) {
        if alpha == 0 || x < 0 || y < 0 {
            return;
        }
        let (x, y) = (x as usize, y as usize);
        if x >= self.width || y >= self.height {
            return;
        }
        let pixel = &mut self.pixels[y * self.width + x];
        *pixel = color.over(*pixel, alpha);
    }

    /// Fills the rectangle with its top left corner at (`x`, `y`), clipped to
    /// the canvas.
    pub fn fill_rect(&mut self, x: i32, y: i32, width: i32, height: i32, color: Color) {
        let x0 = x.clamp(0, self.width as i32) as usize;
        let x1 = (x + width).clamp(0, self.width as i32) as usize;
        let y0 = y.clamp(0, self.height as i32) as usize;
        let y1 = (y + height).clamp(0, self.height as i32) as usize;
        for row in y0..y1 {
            self.pixels[row * self.width + x0..row * self.width + x1].fill(color);
        }
    }

    /// Draws a coverage mask of `width` × `height` bytes in rows, with its
    /// top left corner at (`x`, `y`).
    pub fn draw_mask(&mut self, x: i32, y: i32, width: usize, mask: &[u8], color: Color) {
        if width == 0 {
            return;
        }
        for (row, line) in mask.chunks(width).enumerate() {
            for (column, &alpha) in line.iter().enumerate() {
                self.blend(x + column as i32, y + row as i32, color, alpha);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const RED: Color = Color::rgb(255, 0, 0);
    const BLUE: Color = Color::rgb(0, 0, 255);

    #[test]
    fn mixes_colors() {
        assert_eq!(RED.over(BLUE, 255), RED);
        assert_eq!(RED.over(BLUE, 0), BLUE);
        assert_eq!(RED.over(BLUE, 128), Color::rgb(128, 0, 127));
    }

    #[test]
    fn clips_rectangles() {
        let mut canvas = Canvas::new(4, 3);
        canvas.fill_rect(-2, 1, 4, 10, RED);
        assert_eq!(canvas.row(0), [Color(0); 4]);
        assert_eq!(canvas.row(1), [RED, RED, Color(0), Color(0)]);
        assert_eq!(canvas.row(2), [RED, RED, Color(0), Color(0)]);
    }

    #[test]
    fn draws_masks_and_ignores_pixels_outside() {
        let mut canvas = Canvas::new(2, 2);
        canvas.clear(BLUE);
        canvas.draw_mask(1, 1, 2, &[255, 255, 0, 0], RED);
        canvas.blend(-1, 0, RED, 255);
        assert_eq!(canvas.get(1, 1), RED);
        assert_eq!(canvas.get(0, 0), BLUE);
        assert_eq!(canvas.get(0, 1), BLUE);
    }
}
