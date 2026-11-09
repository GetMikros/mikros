//! The framebuffer console: text in a fixed 8 by 16 pixel font, light on
//! black, drawn into a framebuffer of 32-bit pixels.

pub mod font;

use core::sync::atomic::{AtomicBool, Ordering};

/// The size of a character cell, in pixels.
pub const CELL_WIDTH: usize = 8;
pub const CELL_HEIGHT: usize = 16;

/// The text color as red, green and blue.
const TEXT: [u8; 3] = [0xD0, 0xD0, 0xD0];

/// A cell's content: a byte from 0x20 to 0x7E, or this for the
/// replacement glyph.
const REPLACED: u8 = 0xFF;

/// Pixels the console draws into, addressed as `y * stride + x`.
pub trait Surface {
    fn set(&mut self, index: usize, value: u32);
}

/// The geometry and pixel layout of a framebuffer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Layout {
    pub width: usize,
    pub height: usize,
    /// Pixels per line in memory.
    pub stride: usize,
    /// The masks of red, green and blue in a pixel value.
    pub masks: [u32; 3],
}

/// The pixel value of the color `rgb` with the channel masks `masks`.
pub fn pixel(masks: [u32; 3], rgb: [u8; 3]) -> u32 {
    let mut value = 0;
    for (mask, c) in masks.into_iter().zip(rgb) {
        if mask == 0 {
            continue;
        }
        let shift = mask.trailing_zeros();
        let max = u64::from(mask >> shift);
        let scaled = (u64::from(c) * max + 127) / 255;
        value |= (scaled as u32) << shift & mask;
    }
    value
}

/// Serializes drawing log lines with releasing the console: one line is
/// drawn at a time, and none once `release` has returned.
pub struct Gate {
    busy: AtomicBool,
    released: AtomicBool,
}

impl Default for Gate {
    fn default() -> Self {
        Self::new()
    }
}

impl Gate {
    pub const fn new() -> Gate {
        Gate {
            busy: AtomicBool::new(false),
            released: AtomicBool::new(false),
        }
    }

    /// Runs `draw` unless the console is released or another line is
    /// being drawn, and returns whether it ran.
    pub fn draw(&self, draw: impl FnOnce()) -> bool {
        if self.busy.swap(true, Ordering::SeqCst) {
            return false;
        }
        // Read after `busy` is set, which `release` reads after setting
        // `released`, so that one of the two sees the other.
        let open = !self.released.load(Ordering::SeqCst);
        if open {
            draw();
        }
        self.busy.store(false, Ordering::SeqCst);
        open
    }

    /// Stops drawing for good, and returns once no line is being drawn.
    /// Must not interrupt a `draw` on the same processor.
    pub fn release(&self) {
        self.released.store(true, Ordering::SeqCst);
        while self.busy.load(Ordering::SeqCst) {
            core::hint::spin_loop();
        }
    }
}

pub struct Console<'a> {
    layout: Layout,
    text: u32,
    columns: usize,
    rows: usize,
    /// The content of every cell, row by row, so that scrolling redraws
    /// instead of reading the framebuffer.
    cells: &'a mut [u8],
    column: usize,
    row: usize,
}

impl<'a> Console<'a> {
    /// A console over a framebuffer of `layout`, keeping its text in
    /// `cells`, which limits the number of cells used.
    pub fn new(layout: Layout, cells: &'a mut [u8]) -> Console<'a> {
        let columns = layout.width / CELL_WIDTH;
        let rows = match columns {
            0 => 0,
            c => (layout.height / CELL_HEIGHT).min(cells.len() / c),
        };
        Console {
            text: pixel(layout.masks, TEXT),
            layout,
            columns,
            rows,
            cells,
            column: 0,
            row: 0,
        }
    }

    /// Paints the whole framebuffer black and empties the console.
    pub fn clear(&mut self, s: &mut impl Surface) {
        for y in 0..self.layout.height {
            for x in 0..self.layout.width {
                s.set(y * self.layout.stride + x, 0);
            }
        }
        self.cells.fill(b' ');
        (self.column, self.row) = (0, 0);
    }

    /// Draws `bytes`: a line break for `\n`, a glyph for each byte from
    /// 0x20 to 0x7E, nothing for UTF-8 continuation bytes, and the
    /// replacement glyph for every other byte.
    pub fn write(&mut self, bytes: &[u8], s: &mut impl Surface) {
        if self.rows == 0 {
            return;
        }
        for &b in bytes {
            match b {
                b'\n' => self.new_line(s),
                0x20..=0x7E => self.put(b, s),
                0x80..=0xBF => {}
                _ => self.put(REPLACED, s),
            }
        }
    }

    /// Draws `code` at the cursor, first moving to a new line when the
    /// current one is full.
    fn put(&mut self, code: u8, s: &mut impl Surface) {
        if self.column == self.columns {
            self.new_line(s);
        }
        self.cells[self.row * self.columns + self.column] = code;
        self.draw(self.column, self.row, code, s);
        self.column += 1;
    }

    fn new_line(&mut self, s: &mut impl Surface) {
        self.column = 0;
        if self.row + 1 < self.rows {
            self.row += 1;
            return;
        }
        let used = self.rows * self.columns;
        self.cells.copy_within(self.columns..used, 0);
        self.cells[used - self.columns..used].fill(b' ');
        for row in 0..self.rows {
            for column in 0..self.columns {
                self.draw(column, row, self.cells[row * self.columns + column], s);
            }
        }
    }

    fn draw(&self, column: usize, row: usize, code: u8, s: &mut impl Surface) {
        let glyph = match code {
            0x20..=0x7E => &font::PRINTABLE[usize::from(code - 0x20)],
            _ => &font::REPLACEMENT,
        };
        let top = row * CELL_HEIGHT * self.layout.stride + column * CELL_WIDTH;
        for (y, bits) in glyph.iter().enumerate() {
            let line = top + y * self.layout.stride;
            for x in 0..CELL_WIDTH {
                let on = bits & (0x80 >> x) != 0;
                s.set(line + x, if on { self.text } else { 0 });
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::vec;
    use std::vec::Vec;

    const BGRX: [u32; 3] = [0xFF_0000, 0xFF00, 0xFF];

    struct Pixels(Vec<u32>);

    impl Surface for Pixels {
        fn set(&mut self, index: usize, value: u32) {
            self.0[index] = value;
        }
    }

    /// A framebuffer of `columns` by `rows` cells with two spare pixels
    /// per line, filled with a value the console never writes.
    fn screen(columns: usize, rows: usize) -> (Layout, Pixels) {
        let layout = Layout {
            width: columns * CELL_WIDTH,
            height: rows * CELL_HEIGHT,
            stride: columns * CELL_WIDTH + 2,
            masks: BGRX,
        };
        (layout, Pixels(vec![7; layout.stride * layout.height]))
    }

    /// The glyph shown in a cell, read back as bits.
    fn cell(layout: &Layout, p: &Pixels, column: usize, row: usize) -> [u8; 16] {
        let mut rows = [0u8; 16];
        for (y, bits) in rows.iter_mut().enumerate() {
            let line = (row * CELL_HEIGHT + y) * layout.stride + column * CELL_WIDTH;
            for x in 0..CELL_WIDTH {
                if p.0[line + x] != 0 {
                    *bits |= 0x80 >> x;
                }
            }
        }
        rows
    }

    fn glyph(b: u8) -> [u8; 16] {
        font::PRINTABLE[usize::from(b - 0x20)]
    }

    #[test]
    fn converts_colors_to_pixels() {
        assert_eq!(pixel(BGRX, [0xD0, 0x10, 0x01]), 0xD0_1001);
        assert_eq!(pixel([0xFF, 0xFF00, 0xFF_0000], [1, 2, 3]), 0x03_0201);
        // 5-6-5 masks scale the 8-bit channels.
        assert_eq!(pixel([0xF800, 0x07E0, 0x1F], [0xFF, 0xFF, 0]), 0xFFE0);
        assert_eq!(pixel([0, 0, 0], [0xFF; 3]), 0);
    }

    #[test]
    fn clears_to_black_inside_the_visible_area() {
        let (layout, mut p) = screen(2, 1);
        let mut cells = [0; 2];
        Console::new(layout, &mut cells).clear(&mut p);
        assert!(
            p.0.chunks(layout.stride)
                .all(|l| l[..16] == [0; 16] && l[16..] == [7, 7])
        );
    }

    #[test]
    fn draws_glyphs_and_replacements() {
        let (layout, mut p) = screen(4, 1);
        let mut cells = [0; 4];
        let mut c = Console::new(layout, &mut cells);
        c.clear(&mut p);
        // "A", then U+00E9 (two bytes, one glyph), then a control byte.
        c.write(b"A\xC3\xA9\x01", &mut p);
        assert_eq!(cell(&layout, &p, 0, 0), glyph(b'A'));
        assert_eq!(cell(&layout, &p, 1, 0), font::REPLACEMENT);
        assert_eq!(cell(&layout, &p, 2, 0), font::REPLACEMENT);
        assert_eq!(cell(&layout, &p, 3, 0), [0; 16]);
        let lit = p.0.iter().find(|&&v| v != 0 && v != 7);
        assert_eq!(lit, Some(&0xD0_D0D0));
    }

    #[test]
    fn wraps_and_scrolls() {
        let (layout, mut p) = screen(2, 2);
        let mut cells = [0; 4];
        let mut c = Console::new(layout, &mut cells);
        c.clear(&mut p);
        c.write(b"ab\ncde", &mut p);
        // "ab" filled the first line exactly, so the line break did not
        // add an empty one. "e" wrapped onto a third line, which
        // scrolled "ab" away.
        assert_eq!(cell(&layout, &p, 0, 0), glyph(b'c'));
        assert_eq!(cell(&layout, &p, 1, 0), glyph(b'd'));
        assert_eq!(cell(&layout, &p, 0, 1), glyph(b'e'));
        assert_eq!(cell(&layout, &p, 1, 1), [0; 16]);
        c.write(b"\n\n", &mut p);
        assert!((0..2).all(|x| cell(&layout, &p, x, 0) == [0; 16]));
    }

    #[test]
    fn draws_one_line_at_a_time_until_released() {
        let gate = Gate::new();
        let mut lines = 0;
        assert!(gate.draw(|| lines += 1));
        assert_eq!(lines, 1);
        // A line that arrives while another is drawn is not drawn.
        assert!(gate.draw(|| assert!(!gate.draw(|| unreachable!()))));
        gate.release();
        assert!(!gate.draw(|| unreachable!()));
    }

    #[test]
    fn release_waits_for_the_line_being_drawn() {
        use std::sync::atomic::AtomicBool;
        let gate = Gate::new();
        let (drawing, drawn) = (AtomicBool::new(false), AtomicBool::new(false));
        std::thread::scope(|s| {
            s.spawn(|| {
                gate.draw(|| {
                    drawing.store(true, Ordering::SeqCst);
                    std::thread::sleep(std::time::Duration::from_millis(50));
                    drawn.store(true, Ordering::SeqCst);
                })
            });
            while !drawing.load(Ordering::SeqCst) {
                std::hint::spin_loop();
            }
            gate.release();
            assert!(drawn.load(Ordering::SeqCst));
        });
    }

    #[test]
    fn uses_only_the_cells_given() {
        let (layout, mut p) = screen(2, 3);
        let mut cells = [0; 5];
        let mut c = Console::new(layout, &mut cells);
        c.clear(&mut p);
        c.write(b"a\nb\nc", &mut p);
        // Two rows fit in five cells, so the third line scrolled.
        assert_eq!(cell(&layout, &p, 0, 0), glyph(b'b'));
        assert_eq!(cell(&layout, &p, 0, 1), glyph(b'c'));
        let mut none: [u8; 0] = [];
        Console::new(layout, &mut none).write(b"x", &mut p);
    }
}
