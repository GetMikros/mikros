//! The authorization screen: a request of the session manager's
//! `OnAuthorize`, shown in a bordered popup over a dimmed copy of the
//! screen shown before, the fields that an administrator types the name
//! and password into, and what each key does. The name and the password
//! are overwritten with zeros when the prompt is dropped.

use crate::emulator::cell::{Cell, DIM};
use crate::emulator::screen::Screen;

/// The longest name and password that `Answer` takes.
const NAME_MAX: usize = 32;
const PASSWORD_MAX: usize = 1024;
/// The widest the popup gets, with its border.
const POPUP_MAX: usize = 64;
const FAILED: &str = "Authorization failed";

/// What `OnAuthorize` asks.
pub struct Request {
    pub request: u32,
    pub app_id: String,
    pub action: String,
    pub reason: String,
    /// The administrator's name when the session manager gave it, and
    /// otherwise empty, for a field.
    pub name: String,
}

/// What a key leads to.
#[derive(Debug, PartialEq, Eq)]
pub enum Step {
    Nothing,
    /// `Answer` with the name and the password.
    Answer(String, String),
    Cancel,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Field {
    Name,
    Password,
}

pub struct Prompt {
    pub request: Request,
    /// The screen shown before, by row and column.
    background: Vec<Cell>,
    cols: usize,
    rows: usize,
    typed: String,
    password: String,
    field: Field,
    /// Whether the answer is being checked, or failed, when keys do
    /// nothing.
    waiting: bool,
    failed: bool,
    stale: bool,
}

/// Overwrites `text` with zeros before it is emptied.
fn wipe(text: &mut String) {
    // SAFETY: zeros are valid UTF-8, and the bytes stay within the
    // string's length.
    for byte in unsafe { text.as_mut_vec() }.iter_mut() {
        // SAFETY: the pointer refers to a byte of the string.
        unsafe { core::ptr::write_volatile(byte, 0) };
    }
    text.clear();
}

impl Drop for Prompt {
    fn drop(&mut self) {
        wipe(&mut self.typed);
        wipe(&mut self.password);
    }
}

/// `text` cut into lines of at most `width` characters, at spaces where
/// it can be.
fn wrap(text: &str, width: usize) -> Vec<String> {
    let mut lines = Vec::new();
    let mut line = String::new();
    for word in text.split(' ') {
        let mut word = word.to_string();
        loop {
            let used = line.chars().count();
            let gap = usize::from(used > 0);
            let len = word.chars().count();
            if used + gap + len <= width {
                if gap == 1 {
                    line.push(' ');
                }
                line.push_str(&word);
                break;
            }
            if used > 0 {
                lines.push(core::mem::take(&mut line));
                continue;
            }
            let head: String = word.chars().take(width).collect();
            word = word.chars().skip(width).collect();
            lines.push(head);
        }
    }
    lines.push(line);
    lines
}

impl Prompt {
    /// The prompt of `request` over the screen shown before, `cols` by
    /// `rows`, whose cells `cell` gives.
    pub fn new(
        request: Request,
        cols: usize,
        rows: usize,
        cell: impl Fn(usize, usize) -> Cell,
    ) -> Prompt {
        let background = (0..rows)
            .flat_map(|r| (0..cols).map(move |c| (r, c)))
            .map(|(r, c)| cell(r, c))
            .collect();
        let field = match request.name.is_empty() {
            true => Field::Name,
            false => Field::Password,
        };
        Prompt {
            request,
            background,
            cols,
            rows,
            typed: String::new(),
            password: String::new(),
            field,
            waiting: false,
            failed: false,
            stale: true,
        }
    }

    /// Takes in the bytes of keys typed.
    pub fn key(&mut self, bytes: &[u8]) -> Step {
        if self.waiting {
            return Step::Nothing;
        }
        self.stale = true;
        // Escape alone cancels; other escape sequences, such as those of
        // the arrows, do nothing.
        if bytes == [0x1b] {
            return Step::Cancel;
        }
        if bytes.first() == Some(&0x1b) {
            return Step::Nothing;
        }
        for &b in bytes {
            let (text, max) = match self.field {
                Field::Name => (&mut self.typed, NAME_MAX),
                Field::Password => (&mut self.password, PASSWORD_MAX),
            };
            match b {
                b'\r' | b'\n' if self.field == Field::Name => self.field = Field::Password,
                b'\r' | b'\n' => {
                    self.waiting = true;
                    let name = match self.request.name.is_empty() {
                        true => self.typed.clone(),
                        false => self.request.name.clone(),
                    };
                    return Step::Answer(name, self.password.clone());
                }
                0x7f | 0x08 => {
                    text.pop();
                }
                0x15 => wipe(text),
                b if b < 0x20 => {}
                _ if text.len() < max => {
                    // Typed text arrives as whole UTF-8 sequences.
                    let mut bytes = core::mem::take(text).into_bytes();
                    bytes.push(b);
                    *text = String::from_utf8(bytes)
                        .unwrap_or_else(|e| String::from_utf8_lossy(e.as_bytes()).into_owned());
                }
                _ => {}
            }
        }
        Step::Nothing
    }

    /// Notes that `Answer` failed: the popup says so until the prompt
    /// ends.
    pub fn failed(&mut self) {
        self.failed = true;
        self.stale = true;
    }

    /// Whether the screen changed since the last call.
    pub fn take_stale(&mut self) -> bool {
        core::mem::replace(&mut self.stale, false)
    }

    /// The lines of the popup, and the line and column of the cursor in
    /// them, wrapped to `width`.
    fn lines(&self, width: usize) -> (Vec<String>, Option<(usize, usize)>) {
        let r = &self.request;
        let mut lines = wrap(&format!("Authorize {} for {}", r.action, r.app_id), width);
        lines.extend(wrap(&r.reason, width));
        lines.push(String::new());
        let name = match r.name.is_empty() {
            true => &self.typed,
            false => &r.name,
        };
        let name_line = format!("Name: {name}");
        let cut: String = name_line.chars().take(width).collect();
        let name_at = (lines.len(), cut.chars().count());
        lines.push(cut);
        let password_at = (lines.len(), "Password: ".len().min(width));
        lines.push("Password:".chars().take(width).collect());
        if self.failed {
            lines.push(String::new());
            lines.extend(wrap(FAILED, width));
        }
        let cursor = match (self.waiting || self.failed, self.field) {
            (true, _) => None,
            (false, Field::Name) => Some(name_at),
            (false, Field::Password) => Some(password_at),
        };
        (lines, cursor)
    }

    /// The authorization screen: the screen shown before, dimmed, and the
    /// popup in the middle.
    pub fn screen(&self) -> Screen {
        let (cols, rows) = (self.cols, self.rows);
        let mut screen = Screen::new(cols, rows);
        for (i, cell) in self.background.iter().enumerate() {
            let mut cell = *cell;
            cell.attrs |= DIM;
            screen.set_cell(i / cols, i % cols, cell);
        }
        let outer = POPUP_MAX.min(cols);
        let inner = outer.saturating_sub(4).max(1);
        let (lines, cursor) = self.lines(inner);
        let height = (lines.len() + 2).min(rows);
        let (top, left) = ((rows - height) / 2, (cols - outer) / 2);
        let put = |screen: &mut Screen, row: usize, col: usize, ch: char| {
            let cell = Cell {
                ch,
                ..Cell::default()
            };
            screen.set_cell(top + row, left + col, cell);
        };
        for row in 0..height {
            for col in 0..outer {
                let last = (row + 1 == height, col + 1 == outer);
                let ch = match (row, col, last) {
                    (0, 0, _) => '┌',
                    (0, _, (_, true)) => '┐',
                    (_, 0, (true, _)) => '└',
                    (_, _, (true, true)) => '┘',
                    (0, _, _) | (_, _, (true, false)) => '─',
                    (_, 0, _) | (_, _, (false, true)) => '│',
                    _ => ' ',
                };
                put(&mut screen, row, col, ch);
            }
        }
        for (i, line) in lines.iter().enumerate().take(height.saturating_sub(2)) {
            for (j, ch) in line.chars().enumerate() {
                put(&mut screen, i + 1, j + 2, ch);
            }
        }
        match cursor {
            Some((line, col)) if line + 1 < height => {
                let at = format!("\x1b[{};{}H", top + line + 2, left + col + 3);
                screen.write(at.as_bytes());
            }
            _ => screen.modes.cursor_visible = false,
        }
        screen
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request(name: &str) -> Request {
        Request {
            request: 7,
            app_id: "system.power".into(),
            action: "power.control".into(),
            reason: "power off".into(),
            name: name.into(),
        }
    }

    fn prompt(name: &str) -> Prompt {
        let background = |_, _| Cell {
            ch: 'x',
            ..Cell::default()
        };
        Prompt::new(request(name), 40, 12, background)
    }

    fn row(screen: &Screen, row: usize) -> String {
        (0..screen.cols()).map(|c| screen.cell(row, c).ch).collect()
    }

    #[test]
    fn asks_for_the_name_then_the_password() {
        let mut p = prompt("");
        assert_eq!(p.key(b"ad"), Step::Nothing);
        assert_eq!(p.key(b"x\x7fa"), Step::Nothing);
        assert_eq!(p.key(b"\r"), Step::Nothing);
        assert_eq!(p.key(b"s3cret"), Step::Nothing);
        assert_eq!(p.key(b"\r"), Step::Answer("ada".into(), "s3cret".into()));
        // While the answer is checked, keys do nothing.
        assert_eq!(p.key(b"\x1b"), Step::Nothing);
    }

    #[test]
    fn takes_only_the_password_for_a_named_administrator() {
        let mut p = prompt("ada");
        assert_eq!(p.key("pä\x15pw".as_bytes()), Step::Nothing);
        assert_eq!(p.key(b"\n"), Step::Answer("ada".into(), "pw".into()));
    }

    #[test]
    fn cancels_on_escape_but_not_on_arrows() {
        let mut p = prompt("");
        assert_eq!(p.key(b"\x1b[A"), Step::Nothing);
        assert_eq!(p.key(b"\x1b"), Step::Cancel);
    }

    #[test]
    fn draws_the_popup_over_the_dimmed_screen() {
        let mut p = prompt("");
        p.key(b"ad");
        let screen = p.screen();
        assert!(screen.cell(0, 0).attrs & DIM != 0 && screen.cell(0, 0).ch == 'x');
        let rows: Vec<String> = (0..12).map(|r| row(&screen, r)).collect();
        let popup: Vec<&str> = rows
            .iter()
            .map(|r| r.trim_matches('x'))
            .filter(|r| r.starts_with(['┌', '│', '└']))
            .collect();
        assert_eq!(popup[0], format!("┌{}┐", "─".repeat(38)));
        assert_eq!(
            popup[1].trim_end_matches('│').trim(),
            "│ Authorize power.control for"
        );
        assert_eq!(popup[2].trim_end_matches('│').trim(), "│ system.power");
        assert_eq!(popup[3].trim_end_matches('│').trim(), "│ power off");
        assert_eq!(popup[5].trim_end_matches('│').trim(), "│ Name: ad");
        assert_eq!(popup[6].trim_end_matches('│').trim(), "│ Password:");
        assert!(popup[7].starts_with('└'));
        assert!(
            screen
                .cell(rows.iter().position(|r| r.contains("Name")).unwrap(), 2)
                .attrs
                & DIM
                == 0
        );
        let (line, col) = screen.cursor().unwrap();
        assert!(
            rows[line].contains("Name: ad") && rows[line][..].chars().nth(col - 1) == Some('d')
        );
        p.failed();
        let failed = p.screen();
        assert!((0..12).any(|r| row(&failed, r).contains(FAILED)));
        assert_eq!(failed.cursor(), None);
    }

    #[test]
    fn wraps_long_words_and_lines() {
        assert_eq!(wrap("a bb ccc", 4), ["a bb", "ccc"]);
        assert_eq!(wrap("abcdefghij", 4), ["abcd", "efgh", "ij"]);
        assert_eq!(wrap("", 4), [""]);
    }
}
