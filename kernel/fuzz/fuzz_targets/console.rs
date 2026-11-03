//! Draws text on the framebuffer console as the kernel does, after a
//! clear, into framebuffers of many sizes with padding at the end of each
//! line and room for fewer cells than fit. After each write the target
//! compares every pixel with a model of the cells, and checks that the
//! console never draws outside the visible area.

#![no_main]

use kernel_core::console::font::{PRINTABLE, REPLACEMENT};
use kernel_core::console::{CELL_HEIGHT, CELL_WIDTH, Console, Layout, Surface, pixel};
use libfuzzer_sys::arbitrary::{self, Arbitrary};
use libfuzzer_sys::fuzz_target;

/// A value the console never writes, in pixels it must not touch.
const UNTOUCHED: u32 = 0x5A5A_5A5A;

#[derive(Arbitrary, Debug)]
struct Input {
    width: u8,
    height: u8,
    padding: u8,
    masks: [u32; 3],
    /// Taken from the cells that fit, when set.
    fewer_cells: Option<u8>,
    writes: Vec<Vec<u8>>,
}

struct Pixels {
    layout: Layout,
    pixels: Vec<u32>,
}

impl Surface for Pixels {
    fn set(&mut self, index: usize, value: u32) {
        let (y, x) = (index / self.layout.stride, index % self.layout.stride);
        assert!(y < self.layout.height && x < self.layout.width, "pixel {x},{y} drawn");
        self.pixels[index] = value;
    }
}

/// The cells and cursor the console must have.
struct Model {
    columns: usize,
    rows: usize,
    cells: Vec<u8>,
    column: usize,
    row: usize,
}

impl Model {
    fn new_line(&mut self) {
        self.column = 0;
        if self.row + 1 < self.rows {
            self.row += 1;
        } else {
            self.cells.drain(..self.columns);
            self.cells.extend(std::iter::repeat_n(b' ', self.columns));
        }
    }

    fn write(&mut self, bytes: &[u8]) {
        if self.rows == 0 {
            return;
        }
        for &b in bytes {
            let code = match b {
                b'\n' => {
                    self.new_line();
                    continue;
                }
                0x80..=0xBF => continue,
                0x20..=0x7E => b,
                _ => 0xFF,
            };
            if self.column == self.columns {
                self.new_line();
            }
            self.cells[self.row * self.columns + self.column] = code;
            self.column += 1;
        }
    }
}

fn check(p: &Pixels, m: &Model, text: u32) {
    let l = p.layout;
    for y in 0..l.height {
        for x in 0..l.stride {
            let at = p.pixels[y * l.stride + x];
            let (column, row) = (x / CELL_WIDTH, y / CELL_HEIGHT);
            let want = if x >= l.width {
                UNTOUCHED
            } else if column < m.columns && row < m.rows {
                let code = m.cells[row * m.columns + column];
                let glyph = match code {
                    0x20..=0x7E => &PRINTABLE[usize::from(code - 0x20)],
                    _ => &REPLACEMENT,
                };
                let on = glyph[y % CELL_HEIGHT] & 0x80 >> (x % CELL_WIDTH) != 0;
                if on { text } else { 0 }
            } else {
                0
            };
            assert_eq!(at, want, "pixel {x},{y}");
        }
    }
}

fuzz_target!(|input: Input| {
    let width = usize::from(input.width % 97);
    let height = usize::from(input.height % 97);
    let layout = Layout {
        width,
        height,
        stride: width + usize::from(input.padding % 4),
        masks: input.masks,
    };
    let columns = width / CELL_WIDTH;
    let fit = columns * (height / CELL_HEIGHT);
    let count = input.fewer_cells.map_or(fit, |f| fit.saturating_sub(usize::from(f) % 8));
    let mut cells = vec![0u8; count];
    let mut console = Console::new(layout, &mut cells);
    let mut p = Pixels {
        layout,
        pixels: vec![UNTOUCHED; layout.stride * height],
    };
    let rows = if columns == 0 { 0 } else { (height / CELL_HEIGHT).min(count / columns) };
    let mut m = Model {
        columns,
        rows,
        cells: vec![b' '; columns * rows],
        column: 0,
        row: 0,
    };
    let text = pixel(input.masks, [0xD0; 3]);
    console.clear(&mut p);
    check(&p, &m, text);
    for bytes in input.writes.iter().take(16) {
        console.write(bytes, &mut p);
        m.write(bytes);
        check(&p, &m, text);
    }
});
