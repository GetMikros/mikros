//! The escape sequences and modes that change how the screen works:
//! saving the cursor, the alternate screen, resets and the character
//! set.

use super::Screen;
use crate::cell::Cell;

impl Screen {
    pub(super) fn esc(&mut self, intermediates: &[u8], last: u8) {
        match (intermediates, last) {
            (b"", b'7') => self.save_cursor(),
            (b"", b'8') => self.restore_cursor(),
            (b"", b'D') => self.line_feed(),
            (b"", b'E') => {
                self.cursor.col = 0;
                self.line_feed();
            }
            (b"", b'H') => self.tabs[self.cursor.col] = true,
            (b"", b'M') => self.reverse_index(),
            (b"", b'c') => self.reset(),
            (b"", b'=') => self.modes.app_keypad = true,
            (b"", b'>') => self.modes.app_keypad = false,
            (b"(", b'0') => self.cursor.line_drawing = true,
            (b"(", _) => self.cursor.line_drawing = false,
            // DECALN fills the screen with E.
            (b"#", b'8') => {
                let e = Cell {
                    ch: 'E',
                    ..Cell::default()
                };
                self.grid_mut().fill(e);
                self.touch();
            }
            _ => {}
        }
    }
    pub(super) fn save_cursor(&mut self) {
        self.saved[usize::from(self.on_alternate)] = self.cursor;
    }
    pub(super) fn restore_cursor(&mut self) {
        let saved = self.saved[usize::from(self.on_alternate)];
        self.cursor = saved;
        self.cursor.row = self.cursor.row.min(self.rows - 1);
        self.cursor.col = self.cursor.col.min(self.cols - 1);
    }
    /// RIS: as a new terminal of the same size.
    pub(super) fn reset(&mut self) {
        *self = Screen::new(self.cols, self.rows);
    }
    /// Switches to the alternate screen, or back.
    pub(super) fn alternate(&mut self, on: bool, clear: bool) {
        if self.on_alternate == on {
            return;
        }
        self.on_alternate = on;
        if on && clear {
            self.alternate.fill(Cell::blank(&self.cursor.pen));
        }
        self.touch();
    }
    pub(super) fn set_mode(&mut self, private: bool, mode: u32, on: bool) {
        match (private, mode) {
            (false, 4) => self.modes.insert = on,
            (false, 20) => self.modes.newline = on,
            (true, 1) => self.modes.app_cursor = on,
            (true, 5) => {
                self.modes.reverse = on;
                self.touch();
            }
            (true, 6) => {
                self.modes.origin = on;
                self.go_to(0, 0);
            }
            (true, 7) => self.modes.autowrap = on,
            (true, 25) => self.modes.cursor_visible = on,
            (true, 47 | 1047) => self.alternate(on, mode == 1047),
            (true, 1048) => match on {
                true => self.save_cursor(),
                false => self.restore_cursor(),
            },
            (true, 1049) => {
                if on {
                    self.save_cursor();
                    self.alternate(true, true);
                } else {
                    self.alternate(false, false);
                    self.restore_cursor();
                }
            }
            (true, 2004) => self.modes.bracketed_paste = on,
            _ => {}
        }
    }
}
