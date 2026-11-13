//! The screen that a terminal's output changes: two grids of cells,
//! the primary and the alternate screen, the cursor, the scroll region,
//! tab stops and modes, which rows changed, and what the terminal
//! answers to requests.

use crate::emulator::cell::Cell;
use crate::emulator::cell::{Width, columns, line_drawing};
use crate::emulator::parser::{Action, Parser};
use alloc::format;
use alloc::vec;
use alloc::vec::Vec;

mod modes;

const TAB_WIDTH: usize = 8;

/// The modes that programs set with `SM`, `RM` and their DEC private
/// forms.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Modes {
    /// DECAWM: text wraps at the right margin.
    pub autowrap: bool,
    /// DECTCEM: the cursor shows.
    pub cursor_visible: bool,
    /// DECCKM: cursor keys send application sequences.
    pub app_cursor: bool,
    /// DECKPAM: the keypad sends application sequences.
    pub app_keypad: bool,
    /// IRM: printing inserts rather than replaces.
    pub insert: bool,
    /// DECOM: cursor positions are relative to the scroll region.
    pub origin: bool,
    /// DECSCNM: the whole screen shows in reverse video.
    pub reverse: bool,
    /// LNM: LF also returns the carriage.
    pub newline: bool,
    /// Pasted text is bracketed.
    pub bracketed_paste: bool,
}

impl Default for Modes {
    fn default() -> Modes {
        Modes {
            autowrap: true,
            cursor_visible: true,
            app_cursor: false,
            app_keypad: false,
            insert: false,
            origin: false,
            reverse: false,
            newline: false,
            bracketed_paste: false,
        }
    }
}

/// The cursor, with the pen it writes with and the character set.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Cursor {
    row: usize,
    col: usize,
    pen: Cell,
    /// The cursor stands past the last column: the next character
    /// wraps first.
    pending_wrap: bool,
    /// G0 is the DEC special graphics set.
    line_drawing: bool,
}

pub struct Screen {
    cols: usize,
    rows: usize,
    primary: Vec<Cell>,
    alternate: Vec<Cell>,
    on_alternate: bool,
    cursor: Cursor,
    /// The cursor saved by DECSC on each screen.
    saved: [Cursor; 2],
    /// The scroll region, from `top` to `bottom`, both included.
    top: usize,
    bottom: usize,
    tabs: Vec<bool>,
    pub modes: Modes,
    dirty: Vec<bool>,
    responses: Vec<u8>,
    parser: Parser,
    /// The last character printed, for REP.
    last: Option<char>,
}

impl Screen {
    pub fn new(cols: usize, rows: usize) -> Screen {
        let (cols, rows) = (cols.max(1), rows.max(1));
        Screen {
            cols,
            rows,
            primary: vec![Cell::default(); cols * rows],
            alternate: vec![Cell::default(); cols * rows],
            on_alternate: false,
            cursor: Cursor::default(),
            saved: [Cursor::default(); 2],
            top: 0,
            bottom: rows - 1,
            tabs: (0..cols).map(|c| c % TAB_WIDTH == 0).collect(),
            modes: Modes::default(),
            dirty: vec![true; rows],
            responses: Vec::new(),
            parser: Parser::new(),
            last: None,
        }
    }

    pub fn cols(&self) -> usize {
        self.cols
    }

    pub fn rows(&self) -> usize {
        self.rows
    }

    /// The cell at `row` and `col` of the screen shown.
    pub fn cell(&self, row: usize, col: usize) -> &Cell {
        &self.grid()[row * self.cols + col]
    }

    /// The cursor's row and column, when it shows.
    pub fn cursor(&self) -> Option<(usize, usize)> {
        let col = self.cursor.col.min(self.cols - 1);
        self.modes.cursor_visible.then_some((self.cursor.row, col))
    }

    /// Whether the alternate screen is shown, which the tests check.
    #[cfg(test)]
    pub fn on_alternate(&self) -> bool {
        self.on_alternate
    }

    /// The rows that changed since the last call, in order, which are
    /// then clean.
    pub fn take_dirty(&mut self) -> Vec<usize> {
        let rows = (0..self.rows).filter(|&r| self.dirty[r]).collect();
        self.dirty.fill(false);
        rows
    }

    /// Marks every row changed.
    pub fn touch(&mut self) {
        self.dirty.fill(true);
    }

    /// What the terminal answers to the requests written since the last
    /// call, to send back to the program.
    pub fn take_responses(&mut self) -> Vec<u8> {
        core::mem::take(&mut self.responses)
    }

    /// Takes in `bytes` that a program wrote.
    pub fn write(&mut self, bytes: &[u8]) {
        let mut parser = core::mem::take(&mut self.parser);
        parser.feed(bytes, |action| self.act(action));
        self.parser = parser;
    }

    /// Changes the size, keeping the cells that still fit.
    pub fn resize(&mut self, cols: usize, rows: usize) {
        let (cols, rows) = (cols.max(1), rows.max(1));
        let copy = |old: &[Cell]| {
            let mut new = vec![Cell::default(); cols * rows];
            for r in 0..rows.min(self.rows) {
                for c in 0..cols.min(self.cols) {
                    new[r * cols + c] = old[r * self.cols + c];
                }
            }
            new
        };
        self.primary = copy(&self.primary);
        self.alternate = copy(&self.alternate);
        self.cols = cols;
        self.rows = rows;
        self.top = 0;
        self.bottom = rows - 1;
        self.tabs = (0..cols).map(|c| c % TAB_WIDTH == 0).collect();
        self.cursor.row = self.cursor.row.min(rows - 1);
        self.cursor.col = self.cursor.col.min(cols - 1);
        self.cursor.pending_wrap = false;
        self.dirty = vec![true; rows];
    }

    fn grid(&self) -> &[Cell] {
        match self.on_alternate {
            true => &self.alternate,
            false => &self.primary,
        }
    }

    fn grid_mut(&mut self) -> &mut [Cell] {
        match self.on_alternate {
            true => &mut self.alternate,
            false => &mut self.primary,
        }
    }

    fn at(&mut self, row: usize, col: usize) -> &mut Cell {
        let cols = self.cols;
        &mut self.grid_mut()[row * cols + col]
    }

    fn act(&mut self, action: Action) {
        match action {
            Action::Print(c) => self.print(c),
            Action::Execute(b) => self.execute(b),
            Action::Csi {
                intermediates,
                params,
                sub,
                last,
            } => self.csi(&intermediates, &params, &sub, last),
            Action::Esc {
                intermediates,
                last,
            } => self.esc(&intermediates, last),
            // Titles and palette changes have nowhere to go.
            Action::Osc(_) => {}
        }
    }

    fn print(&mut self, c: char) {
        let c = match self.cursor.line_drawing {
            true => line_drawing(c).unwrap_or(c),
            false => c,
        };
        let width = columns(c);
        if width == 0 {
            return;
        }
        if self.cursor.pending_wrap && self.modes.autowrap {
            self.cursor.pending_wrap = false;
            self.cursor.col = 0;
            self.line_feed();
        }
        // A wide character that does not fit in the line wraps whole.
        if width == 2 && self.cursor.col + 1 >= self.cols {
            if !self.modes.autowrap || self.cols < 2 {
                return;
            }
            let (row, col) = (self.cursor.row, self.cursor.col);
            let blank = Cell::blank(&self.cursor.pen);
            *self.at(row, col) = blank;
            self.cursor.col = 0;
            self.line_feed();
        }
        let (row, col) = (self.cursor.row, self.cursor.col);
        if self.modes.insert {
            self.insert_cells(width);
        }
        self.clear_wide_at(row, col);
        let pen = self.cursor.pen;
        *self.at(row, col) = Cell {
            ch: c,
            width: if width == 2 {
                Width::Wide
            } else {
                Width::Single
            },
            ..pen
        };
        if width == 2 {
            self.clear_wide_at(row, col + 1);
            *self.at(row, col + 1) = Cell {
                ch: ' ',
                width: Width::Continuation,
                ..pen
            };
        }
        self.dirty[row] = true;
        self.last = Some(c);
        if col + width >= self.cols {
            self.cursor.col = self.cols - 1;
            self.cursor.pending_wrap = true;
        } else {
            self.cursor.col = col + width;
        }
    }

    /// Before `(row, col)` is overwritten: the other half of a wide
    /// character there becomes blank.
    fn clear_wide_at(&mut self, row: usize, col: usize) {
        let blank = Cell::blank(&self.cursor.pen);
        let cols = self.cols;
        match self.at(row, col).width {
            Width::Wide if col + 1 < cols => *self.at(row, col + 1) = blank,
            Width::Continuation if col > 0 => *self.at(row, col - 1) = blank,
            _ => {}
        }
    }

    fn execute(&mut self, b: u8) {
        match b {
            0x08 => {
                self.cursor.pending_wrap = false;
                self.cursor.col = self.cursor.col.saturating_sub(1);
            }
            0x09 => self.tab_forward(1),
            0x0A..=0x0C => {
                self.line_feed();
                if self.modes.newline {
                    self.cursor.col = 0;
                }
            }
            0x0D => {
                self.cursor.pending_wrap = false;
                self.cursor.col = 0;
            }
            // SO and SI switch between G0 and G1; only G0 is kept.
            _ => {}
        }
    }

    /// Moves the cursor down a line, scrolling the region at its bottom.
    fn line_feed(&mut self) {
        self.cursor.pending_wrap = false;
        if self.cursor.row == self.bottom {
            self.scroll_up(1);
        } else if self.cursor.row + 1 < self.rows {
            self.cursor.row += 1;
        }
    }

    fn reverse_index(&mut self) {
        self.cursor.pending_wrap = false;
        if self.cursor.row == self.top {
            self.scroll_down(1);
        } else {
            self.cursor.row = self.cursor.row.saturating_sub(1);
        }
    }

    /// Scrolls the scroll region up by `n` lines.
    fn scroll_up(&mut self, n: usize) {
        self.shift_rows(self.top, self.bottom, n, true);
    }

    fn scroll_down(&mut self, n: usize) {
        self.shift_rows(self.top, self.bottom, n, false);
    }

    /// Moves rows `top` to `bottom` up, or down, by `n`, filling the
    /// rows left with blanks.
    fn shift_rows(&mut self, top: usize, bottom: usize, n: usize, up: bool) {
        let span = bottom + 1 - top;
        let n = n.min(span);
        let cols = self.cols;
        let blank = Cell::blank(&self.cursor.pen);
        let grid = self.grid_mut();
        let region = &mut grid[top * cols..(bottom + 1) * cols];
        if up {
            region.rotate_left(n * cols);
            region[(span - n) * cols..].fill(blank);
        } else {
            region.rotate_right(n * cols);
            region[..n * cols].fill(blank);
        }
        self.dirty[top..=bottom].fill(true);
    }

    fn tab_forward(&mut self, n: usize) {
        self.cursor.pending_wrap = false;
        for _ in 0..n {
            let next = (self.cursor.col + 1..self.cols).find(|&c| self.tabs[c]);
            self.cursor.col = next.unwrap_or(self.cols - 1);
        }
    }

    fn tab_back(&mut self, n: usize) {
        self.cursor.pending_wrap = false;
        for _ in 0..n {
            let previous = (0..self.cursor.col).rev().find(|&c| self.tabs[c]);
            self.cursor.col = previous.unwrap_or(0);
        }
    }

    /// Moves the cursor to `row` and `col`, relative to the scroll
    /// region in origin mode.
    fn go_to(&mut self, row: usize, col: usize) {
        let (first, last) = match self.modes.origin {
            true => (self.top, self.bottom),
            false => (0, self.rows - 1),
        };
        self.cursor.row = (first + row).min(last);
        self.cursor.col = col.min(self.cols - 1);
        self.cursor.pending_wrap = false;
    }

    /// Blanks columns `from` to `to`, not included, of `row`.
    fn erase(&mut self, row: usize, from: usize, to: usize) {
        let blank = Cell::blank(&self.cursor.pen);
        let to = to.min(self.cols);
        if from >= to {
            return;
        }
        self.clear_wide_at(row, from);
        self.clear_wide_at(row, to - 1);
        let cols = self.cols;
        self.grid_mut()[row * cols + from..row * cols + to].fill(blank);
        self.dirty[row] = true;
    }

    fn erase_display(&mut self, how: u32) {
        let (row, col) = (self.cursor.row, self.cursor.col);
        match how {
            0 => {
                self.erase(row, col, self.cols);
                for r in row + 1..self.rows {
                    self.erase(r, 0, self.cols);
                }
            }
            1 => {
                for r in 0..row {
                    self.erase(r, 0, self.cols);
                }
                self.erase(row, 0, col + 1);
            }
            // 3 also clears the scrollback, which there is none of.
            2 | 3 => {
                for r in 0..self.rows {
                    self.erase(r, 0, self.cols);
                }
            }
            _ => {}
        }
    }

    fn erase_line(&mut self, how: u32) {
        let (row, col) = (self.cursor.row, self.cursor.col);
        match how {
            0 => self.erase(row, col, self.cols),
            1 => self.erase(row, 0, col + 1),
            2 => self.erase(row, 0, self.cols),
            _ => {}
        }
    }

    /// Inserts `n` blank cells at the cursor, shifting the rest of the
    /// line right.
    fn insert_cells(&mut self, n: usize) {
        let (row, col, cols) = (self.cursor.row, self.cursor.col, self.cols);
        let n = n.min(cols - col);
        let blank = Cell::blank(&self.cursor.pen);
        let line = &mut self.grid_mut()[row * cols..(row + 1) * cols];
        line[col..].rotate_right(n);
        line[col..col + n].fill(blank);
        self.dirty[row] = true;
    }

    fn delete_cells(&mut self, n: usize) {
        let (row, col, cols) = (self.cursor.row, self.cursor.col, self.cols);
        let n = n.min(cols - col);
        let blank = Cell::blank(&self.cursor.pen);
        let line = &mut self.grid_mut()[row * cols..(row + 1) * cols];
        line[col..].rotate_left(n);
        line[cols - n..].fill(blank);
        self.dirty[row] = true;
    }

    /// Inserts or deletes `n` lines at the cursor, within the scroll
    /// region, which the cursor must be in.
    fn insert_lines(&mut self, n: usize, insert: bool) {
        let row = self.cursor.row;
        if row < self.top || row > self.bottom {
            return;
        }
        self.shift_rows(row, self.bottom, n, !insert);
        self.cursor.col = 0;
        self.cursor.pending_wrap = false;
    }

    fn csi(
        &mut self,
        intermediates: &[u8],
        params: &[Option<u32>],
        sub: &[Vec<Option<u32>>],
        last: u8,
    ) {
        // A missing or zero count is 1.
        let n = |i: usize| params.get(i).copied().flatten().unwrap_or(0).max(1) as usize;
        let p = |i: usize| params.get(i).copied().flatten().unwrap_or(0);
        let (row, col) = (self.cursor.row, self.cursor.col);
        match (intermediates, last) {
            (b"", b'@') => self.insert_cells(n(0)),
            (b"", b'A') => {
                let top = if row >= self.top { self.top } else { 0 };
                self.cursor.row = row.saturating_sub(n(0)).max(top);
                self.cursor.pending_wrap = false;
            }
            (b"", b'B' | b'e') => {
                let bottom = if row <= self.bottom {
                    self.bottom
                } else {
                    self.rows - 1
                };
                self.cursor.row = (row + n(0)).min(bottom);
                self.cursor.pending_wrap = false;
            }
            (b"", b'C' | b'a') => {
                self.cursor.col = (col + n(0)).min(self.cols - 1);
                self.cursor.pending_wrap = false;
            }
            (b"", b'D') => {
                self.cursor.col = col.saturating_sub(n(0));
                self.cursor.pending_wrap = false;
            }
            (b"", b'E') => {
                self.cursor.row = (row + n(0)).min(self.bottom.max(row));
                self.cursor.col = 0;
                self.cursor.pending_wrap = false;
            }
            (b"", b'F') => {
                self.cursor.row =
                    row.saturating_sub(n(0))
                        .max(if row >= self.top { self.top } else { 0 });
                self.cursor.col = 0;
                self.cursor.pending_wrap = false;
            }
            (b"", b'G' | b'`') => {
                self.cursor.col = (n(0) - 1).min(self.cols - 1);
                self.cursor.pending_wrap = false;
            }
            (b"", b'H' | b'f') => self.go_to(n(0) - 1, n(1) - 1),
            (b"", b'I') => self.tab_forward(n(0)),
            (b"", b'J') => self.erase_display(p(0)),
            (b"", b'K') => self.erase_line(p(0)),
            (b"", b'L') => self.insert_lines(n(0), true),
            (b"", b'M') => self.insert_lines(n(0), false),
            (b"", b'P') => self.delete_cells(n(0)),
            (b"", b'S') => self.scroll_up(n(0)),
            (b"", b'T') if params.len() <= 1 => self.scroll_down(n(0)),
            (b"", b'X') => self.erase(row, col, col + n(0)),
            (b"", b'Z') => self.tab_back(n(0)),
            (b"", b'b') => {
                if let Some(c) = self.last {
                    for _ in 0..n(0).min(self.cols * self.rows) {
                        self.print(c);
                    }
                }
            }
            (b"", b'c') if p(0) == 0 => self.responses.extend_from_slice(b"\x1b[?62;22c"),
            (b">", b'c') => self.responses.extend_from_slice(b"\x1b[>0;10;0c"),
            (b"", b'd') => {
                let first = if self.modes.origin { self.top } else { 0 };
                self.cursor.row = (first + n(0) - 1).min(self.rows - 1);
                self.cursor.pending_wrap = false;
            }
            (b"", b'g') => match p(0) {
                0 => self.tabs[col] = false,
                3 => self.tabs.fill(false),
                _ => {}
            },
            (b"" | b"?", b'h' | b'l') => {
                let private = intermediates == b"?";
                for &mode in params.iter().flatten() {
                    self.set_mode(private, mode, last == b'h');
                }
            }
            (b"", b'm') => crate::emulator::sgr::sgr(&mut self.cursor.pen, params, sub),
            (b"", b'n') => match p(0) {
                5 => self.responses.extend_from_slice(b"\x1b[0n"),
                6 => {
                    let first = if self.modes.origin { self.top } else { 0 };
                    let report = format!("\x1b[{};{}R", row - first + 1, col + 1);
                    self.responses.extend_from_slice(report.as_bytes());
                }
                _ => {}
            },
            (b"", b'r') => {
                let top = n(0) - 1;
                let bottom = params
                    .get(1)
                    .copied()
                    .flatten()
                    .map_or(self.rows, |b| b as usize);
                let bottom = bottom.min(self.rows).max(1) - 1;
                if top < bottom {
                    self.top = top;
                    self.bottom = bottom;
                    self.go_to(0, 0);
                }
            }
            (b"", b's') => self.save_cursor(),
            (b"", b'u') => self.restore_cursor(),
            // DECSTR, a soft reset.
            (b"!", b'p') => {
                self.modes = Modes::default();
                self.cursor.pen = Cell::default();
                self.cursor.line_drawing = false;
                self.top = 0;
                self.bottom = self.rows - 1;
            }
            _ => {}
        }
    }
}
