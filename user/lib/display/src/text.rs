//! Text in the built-in 8 by 16 pixel bitmap font, drawn one row of
//! pixels at a time.

use crate::font::PRINTABLE;

/// The size of a character's cell, in pixels.
pub const CELL_WIDTH: usize = 8;
pub const CELL_HEIGHT: usize = 16;

/// The glyph of `c`: its own for printable ASCII, and that of `?` for
/// every other character.
pub fn glyph(c: char) -> &'static [u8; CELL_HEIGHT] {
    let at = match c {
        ' '..='~' => c as usize - 0x20,
        _ => '?' as usize - 0x20,
    };
    &PRINTABLE[at]
}

/// Row `y`, from 0 to `CELL_HEIGHT`, of the pixels of `text` drawn in
/// `fg` on `bg`, `width` pixels in all. Each character takes one cell;
/// the pixels after the text, and those of a cell cut by the width, are
/// `bg`.
pub fn row(text: &str, y: usize, width: usize, fg: u32, bg: u32) -> impl Iterator<Item = u32> {
    let mut chars = text.chars();
    let mut bits = 0u8;
    (0..width).map(move |x| {
        if x % CELL_WIDTH == 0 {
            bits = match x + CELL_WIDTH <= width {
                true => chars
                    .next()
                    .map_or(0, |c| glyph(c).get(y).copied().unwrap_or(0)),
                false => 0,
            };
        }
        match bits & (0x80 >> (x % CELL_WIDTH)) {
            0 => bg,
            _ => fg,
        }
    })
}
