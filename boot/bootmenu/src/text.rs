//! Text drawn with the Inter fonts in `fonts/`.

use crate::canvas::{Canvas, Color};
use alloc::collections::BTreeMap;
use alloc::vec::Vec;
use fontdue::{Font, FontSettings, Metrics};

static REGULAR: &[u8] = include_bytes!("../fonts/Inter-Regular.ttf");
static SEMIBOLD: &[u8] = include_bytes!("../fonts/Inter-SemiBold.ttf");

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Weight {
    Regular,
    SemiBold,
}

/// A text style: a weight and a size in pixels.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Style {
    pub weight: Weight,
    pub size: f32,
}

/// A rasterized glyph.
struct Glyph {
    metrics: Metrics,
    coverage: Vec<u8>,
}

/// The fonts, with the glyphs rasterized so far.
pub struct Text {
    regular: Font,
    semibold: Font,
    glyphs: BTreeMap<(Weight, u32, char), Glyph>,
}

/// Rounds to the nearest whole number, halves away from zero.
fn round(value: f32) -> i32 {
    if value < 0.0 {
        (value - 0.5) as i32
    } else {
        (value + 0.5) as i32
    }
}

impl Text {
    /// Parses the fonts. Fails only if the font files are damaged.
    pub fn new() -> Option<Text> {
        let settings = FontSettings::default();
        Some(Text {
            regular: Font::from_bytes(REGULAR, settings).ok()?,
            semibold: Font::from_bytes(SEMIBOLD, settings).ok()?,
            glyphs: BTreeMap::new(),
        })
    }

    fn glyph(&mut self, style: Style, c: char) -> &Glyph {
        let font = match style.weight {
            Weight::Regular => &self.regular,
            Weight::SemiBold => &self.semibold,
        };
        self.glyphs
            .entry((style.weight, style.size.to_bits(), c))
            .or_insert_with(|| {
                let (metrics, coverage) = font.rasterize(c, style.size);
                Glyph { metrics, coverage }
            })
    }

    /// The width of `text` in pixels, from the sum of the glyph advances.
    pub fn width(&mut self, text: &str, style: Style) -> i32 {
        let advance: f32 = text
            .chars()
            .map(|c| self.glyph(style, c).metrics.advance_width)
            .sum();
        round(advance)
    }

    /// Draws `text` with its left end at `x` and its baseline at `y`.
    pub fn draw(
        &mut self,
        canvas: &mut Canvas,
        text: &str,
        style: Style,
        x: i32,
        y: i32,
        color: Color,
    ) {
        let mut pen = x as f32;
        for c in text.chars() {
            let glyph = self.glyph(style, c);
            let m = &glyph.metrics;
            let left = round(pen) + m.xmin;
            let top = y - m.height as i32 - m.ymin;
            canvas.draw_mask(left, top, m.width, &glyph.coverage, color);
            pen += m.advance_width;
        }
    }

    /// Draws `text` centered on `x`, with its baseline at `y`.
    pub fn draw_centered(
        &mut self,
        canvas: &mut Canvas,
        text: &str,
        style: Style,
        x: i32,
        y: i32,
        color: Color,
    ) {
        let width = self.width(text, style);
        self.draw(canvas, text, style, x - width / 2, y, color);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const STYLE: Style = Style {
        weight: Weight::SemiBold,
        size: 20.0,
    };

    #[test]
    fn rounds_halves_away_from_zero() {
        assert_eq!(round(1.5), 2);
        assert_eq!(round(1.49), 1);
        assert_eq!(round(-1.5), -2);
    }

    #[test]
    fn measures_text() {
        let mut text = Text::new().unwrap();
        let one = text.width("M", STYLE);
        assert!(one > 10 && one < 25, "{one}");
        assert!(text.width("MM", STYLE) >= 2 * one - 1);
        assert_eq!(text.width("", STYLE), 0);
    }

    #[test]
    fn draws_text_above_the_baseline() {
        let mut text = Text::new().unwrap();
        let mut canvas = Canvas::new(60, 40);
        let white = Color::rgb(255, 255, 255);
        text.draw_centered(&mut canvas, "Mi", STYLE, 30, 30, white);
        let lit = |rows: core::ops::Range<usize>| {
            rows.flat_map(|y| (0..60).map(move |x| (x, y)))
                .filter(|&(x, y)| canvas.get(x, y) != Color(0))
                .count()
        };
        assert!(lit(10..30) > 50);
        assert_eq!(lit(31..40), 0);
    }
}
