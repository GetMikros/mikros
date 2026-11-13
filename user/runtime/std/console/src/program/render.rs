//! Drawing a terminal screen on a display: a dumb buffer of the display's
//! preferred mode, cells drawn with the monospaced font, and commits of
//! the rows that changed.

use crate::font::{Font, Glyph};
use crate::palette::{BACKGROUND, FOREGROUND, blend, pixel};
use mikros_rt::interfaces::mikros::base::Status;
use mikros_rt::interfaces::mikros::gpu::{
    Colorspace, Commit, CommitFlags, CrtcState, DisplayAsyncClient, DisplayEvent, DumbBuffer, Mode,
    PlaneState, Rect,
};
use mikros_rt::sys;
use mikros_vt::cell::{BOLD, Cell, INVISIBLE, REVERSE, UNDERLINE, Width};
use mikros_vt::screen::Screen;
use std::collections::HashMap;

const XRGB8888: u32 = u32::from_le_bytes(*b"XR24");
const PERM_READ: u32 = 1 << 0;
const PERM_WRITE: u32 = 1 << 1;
const REGULAR: &str = "/usr/share/fonts/dejavu/DejaVuSansMono.ttf";
const BOLD_FONT: &str = "/usr/share/fonts/dejavu/DejaVuSansMono-Bold.ttf";
/// The font sizes tried. Of those that give at least 80 columns and 24
/// rows, the console takes the one whose rows are nearest the kernel
/// log's, `ROW_TARGET` pixels high, and the larger of two as near.
const SIZES: std::ops::RangeInclusive<u32> = 8..=32;
const ROW_TARGET: usize = 16;
pub const MIN_COLS: usize = 80;
pub const MIN_ROWS: usize = 24;

/// What the console draws with: the display, its buffer, and the fonts.
pub struct Renderer {
    display: DisplayAsyncClient,
    mode: Mode,
    buffer: DumbBuffer,
    pixels: *mut u32,
    /// Pixels per line of the buffer.
    stride: usize,
    regular: Font,
    bold: Font,
    glyphs: HashMap<(char, bool), Option<Glyph>>,
    /// Whether a commit was shown since the whole screen was committed,
    /// so that later commits may name damage.
    shown: bool,
}

fn copy(m: &Mode) -> Mode {
    Mode {
        width: m.width,
        height: m.height,
        refresh_millihertz: m.refresh_millihertz,
        flags: m.flags,
    }
}

impl Renderer {
    /// Takes `display` over: its first connected connector's preferred
    /// mode, a dumb buffer of that size, and fonts of the size whose rows
    /// are nearest `ROW_TARGET` pixels among those that fit `MIN_COLS` by
    /// `MIN_ROWS` cells.
    pub async fn new(
        display: DisplayAsyncClient,
        process: sys::Handle,
    ) -> Result<Renderer, String> {
        let resources = display
            .get_resources()
            .await
            .map_err(|e| format!("GetResources: {e:?}"))?;
        let connector = resources
            .resources
            .connectors
            .iter()
            .find(|c| c.connected)
            .ok_or("no connected connector")?;
        let mode = connector
            .modes
            .iter()
            .find(|m| {
                m.flags
                    .contains(mikros_rt::interfaces::mikros::gpu::ModeFlags::PREFERRED)
            })
            .or(connector.modes.first())
            .map(copy)
            .ok_or("no mode")?;
        let buffer = match display
            .create_dumb_buffer(mode.width, mode.height, XRGB8888)
            .await
        {
            Ok(Ok(r)) => r.buffer,
            other => return Err(format!("CreateDumbBuffer: {other:?}")),
        };
        let mut address = 0;
        // SAFETY: the out pointer is valid for the call.
        let status = unsafe {
            sys::vm_map(
                process,
                PERM_READ | PERM_WRITE,
                0,
                buffer.memory.raw(),
                0,
                buffer.size,
                &mut address,
            )
        };
        if status != mikros_rt::OK {
            return Err(format!("mapping the buffer: {status}"));
        }
        let mut regular = Font::open(REGULAR, *SIZES.start()).ok_or("opening the font")?;
        let mut bold = Font::open(BOLD_FONT, *SIZES.start()).ok_or("opening the bold font")?;
        let (w, h) = (mode.width as usize, mode.height as usize);
        let mut fitting = Vec::new();
        for s in SIZES {
            let fits = regular.resize(s).is_some()
                && w / regular.cell.0 >= MIN_COLS
                && h / regular.cell.1 >= MIN_ROWS;
            if fits {
                fitting.push((s, regular.cell.1));
            }
        }
        let size = fitting
            .iter()
            .min_by_key(|&&(s, row)| (row.abs_diff(ROW_TARGET), std::cmp::Reverse(s)))
            .map_or(*SIZES.start(), |&(s, _)| s);
        regular.resize(size).ok_or("sizing the font")?;
        bold.resize(size).ok_or("sizing the bold font")?;
        let stride = buffer.stride as usize / 4;
        Ok(Renderer {
            display,
            mode,
            buffer,
            pixels: address as *mut u32,
            stride,
            regular,
            bold,
            glyphs: HashMap::new(),
            shown: false,
        })
    }

    /// The columns and rows of cells that fit.
    pub fn grid(&self) -> (usize, usize) {
        let (cw, ch) = self.regular.cell;
        (
            self.mode.width as usize / cw,
            self.mode.height as usize / ch,
        )
    }

    fn put(&mut self, x: usize, y: usize, value: u32) {
        if x < self.mode.width as usize && y < self.mode.height as usize {
            // SAFETY: the pixel lies within the mapped buffer.
            unsafe { self.pixels.add(y * self.stride + x).write_volatile(value) };
        }
    }

    /// Draws `cell` at column `col` and row `row`, inverted for the
    /// cursor or the screen's reverse video.
    fn draw_cell(&mut self, row: usize, col: usize, cell: &Cell, invert: bool) {
        let (cw, ch) = self.regular.cell;
        let mut fg = pixel(cell.fg, FOREGROUND);
        let mut bg = pixel(cell.bg, BACKGROUND);
        if (cell.attrs & REVERSE != 0) != invert {
            std::mem::swap(&mut fg, &mut bg);
        }
        let width = if cell.width == Width::Wide {
            2 * cw
        } else {
            cw
        };
        let (x0, y0) = (col * cw, row * ch);
        for y in 0..ch {
            for x in 0..width {
                self.put(x0 + x, y0 + y, bg);
            }
        }
        if cell.ch != ' ' && cell.attrs & INVISIBLE == 0 {
            let bold = cell.attrs & BOLD != 0;
            let key = (cell.ch, bold);
            if !self.glyphs.contains_key(&key) {
                let font = if bold { &self.bold } else { &self.regular };
                self.glyphs.insert(key, font.glyph(cell.ch));
            }
            if let Some(glyph) = self.glyphs.get(&key).and_then(Option::as_ref) {
                let baseline = self.regular.baseline as i32;
                let (w, rows, left, top) = (glyph.width, glyph.rows, glyph.left, glyph.top);
                let coverage = glyph.coverage.clone();
                for gy in 0..rows {
                    for gx in 0..w {
                        let a = coverage[gy * w + gx];
                        let x = x0 as i32 + left + gx as i32;
                        let y = y0 as i32 + baseline - top + gy as i32;
                        if a > 0
                            && x >= x0 as i32
                            && y >= y0 as i32
                            && (x as usize) < x0 + width
                            && (y as usize) < y0 + ch
                        {
                            self.put(x as usize, y as usize, blend(fg, bg, a));
                        }
                    }
                }
            }
        }
        if cell.attrs & UNDERLINE != 0 {
            let y = y0 + (self.regular.baseline + 1).min(ch - 1);
            for x in 0..width {
                self.put(x0 + x, y, fg);
            }
        }
    }

    /// Draws `rows` of `screen`, and the cursor on its row, with the
    /// screen's top row on row `top` of the grid.
    pub fn draw(&mut self, screen: &Screen, rows: &[usize], top: usize) {
        let (cols, grid_rows) = self.grid();
        let reverse = screen.modes.reverse;
        let cursor = screen.cursor();
        for &row in rows {
            if row >= screen.rows() || top + row >= grid_rows {
                continue;
            }
            for col in 0..cols.min(screen.cols()) {
                let cell = *screen.cell(row, col);
                if cell.width == Width::Continuation {
                    continue;
                }
                let at_cursor = cursor == Some((row, col));
                self.draw_cell(top + row, col, &cell, reverse != at_cursor);
            }
        }
    }

    /// Shows the buffer, naming `rows` as damage once the whole screen
    /// was shown, and returns once the commit completed.
    pub async fn present(&mut self, rows: &[usize]) -> Result<(), Status> {
        let (w, h) = (self.mode.width, self.mode.height);
        let ch = self.regular.cell.1 as u32;
        let damage = match self.shown {
            true => damage(rows, ch, w, h),
            false => Vec::new(),
        };
        let whole = Rect {
            x: 0,
            y: 0,
            width: w,
            height: h,
        };
        let commit = Commit {
            flags: CommitFlags(0),
            crtcs: vec![CrtcState {
                crtc: 1,
                active: true,
                connector: 1,
                mode: copy(&self.mode),
                vrr: false,
                gamma: None,
                colorspace: Colorspace::Default,
                hdr: None,
            }],
            planes: vec![PlaneState {
                plane: 1,
                crtc: 1,
                buffer: self.buffer.buffer,
                src: Rect { ..whole },
                dst: whole,
                damage,
                acquire_sync: 0,
                acquire_value: 0,
            }],
        };
        let id = match self.display.commit(commit).await {
            Ok(Ok(r)) => r.commit_id,
            Ok(Err(status)) => return Err(status),
            Err(_) => return Err(Status::PeerClosed),
        };
        loop {
            match self.display.next_event().await {
                Ok(DisplayEvent::OnCommitComplete(c)) if c.commit_id == id => break,
                Ok(_) => {}
                Err(_) => return Err(Status::PeerClosed),
            }
        }
        self.shown = true;
        Ok(())
    }
}

/// The most rectangles a commit names as damage.
const DAMAGE_MAX: usize = 16;

/// The damage of the screen `width` by `height` pixels when `rows`, of
/// `ch` pixels each, changed: one rectangle for each run of adjacent
/// rows, or a single one around them all when there are more runs than a
/// commit takes.
fn damage(rows: &[usize], ch: u32, width: u32, height: u32) -> Vec<Rect> {
    let mut runs: Vec<(u32, u32)> = Vec::new();
    for &row in rows {
        let row = row as u32;
        match runs.last_mut() {
            Some((_, end)) if *end == row => *end = row + 1,
            _ => runs.push((row, row + 1)),
        }
    }
    if runs.len() > DAMAGE_MAX {
        runs = vec![(runs[0].0, runs[runs.len() - 1].1)];
    }
    runs.into_iter()
        .filter_map(|(first, end)| {
            let y = first * ch;
            let bottom = (end * ch).min(height);
            (bottom > y).then(|| Rect {
                x: 0,
                y: y as i32,
                width,
                height: bottom - y,
            })
        })
        .collect()
}
