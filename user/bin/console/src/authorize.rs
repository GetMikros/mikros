//! The authorization screen: a request of the session manager's
//! `OnAuthorize`, shown as `[authorize]` lines below the cursor of its
//! console's terminal, over that terminal's screen dimmed; the fields
//! that an administrator types the name and the password into, the
//! password shown as one `*` per character; what each key does; and the
//! lines that the terminal keeps afterwards. The name and the password
//! are overwritten with zeros when the prompt is dropped. A request of
//! `OnAsk` shows there as one `[y/N]` line instead (MIKSPEC-0067.R21).

use crate::emulator::cell::{Cell, DIM};
use crate::emulator::screen::Screen;
use crate::input::SecureKey;

/// The longest name and password that `Answer` takes.
const NAME_MAX: usize = 32;
const PASSWORD_MAX: usize = 1024;
const PREFIX: &str = "[authorize] ";
/// The action of sudo's requests, whose lines say `[sudo]` and leave the
/// request's line out, as the command shows above them.
const ADMIN_RUN: &str = "admin.run";
const SUDO_PREFIX: &str = "[sudo] ";
const FAILED: &str = "authorization failed";
const ASK: &str = "[y/N]";

/// What `OnAuthorize` or `OnAsk` asks.
pub struct Request {
    pub request: u32,
    /// The text console of the client's session.
    pub console: u8,
    pub app_id: String,
    /// The service that asks for the app, or empty.
    pub via: String,
    /// The action, or the permission of an `OnAsk`.
    pub action: String,
    pub reason: String,
    /// The administrator's name when the session manager gave it, and
    /// otherwise empty, for a field.
    pub name: String,
    /// Whether it is an `OnAsk`, which `AnswerAsk` answers.
    pub ask: bool,
}

/// What a key leads to.
#[derive(Debug, PartialEq, Eq)]
pub enum Step {
    Nothing,
    /// `Answer` with the name and the password.
    Answer(String, String),
    /// `AnswerAsk` with whether the user allowed it.
    AnswerAsk(bool),
    Cancel,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Field {
    Name,
    Password,
}

pub struct Prompt {
    pub request: Request,
    /// The terminal's screen, by row and column.
    background: Vec<Cell>,
    cols: usize,
    rows: usize,
    /// The row that the lines start on, and whether that is the
    /// cursor's own row.
    prompt_row: usize,
    on_cursor_row: bool,
    typed: String,
    password: String,
    field: Field,
    /// Whether the answer is being checked, or failed, when keys do
    /// nothing.
    waiting: bool,
    failed: bool,
    stale: bool,
    /// Whether the terminal has been given the lines already.
    kept: bool,
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

/// `text` cut into rows of `width` characters, as a terminal wraps a
/// long line; at least one row.
fn wrap(text: &str, width: usize) -> Vec<String> {
    let chars: Vec<char> = text.chars().collect();
    if chars.is_empty() {
        return vec![String::new()];
    }
    chars
        .chunks(width.max(1))
        .map(|c| c.iter().collect())
        .collect()
}

impl Prompt {
    /// The prompt of `request` on a terminal whose screen, `cols` by
    /// `rows`, has the cells that `cell` gives and the cursor at `cursor`.
    /// The lines start on the cursor's row when the cursor is in the
    /// first column of an empty row, and otherwise on the row below.
    pub fn new(
        request: Request,
        cols: usize,
        rows: usize,
        cursor: (usize, usize),
        cell: impl Fn(usize, usize) -> Cell,
    ) -> Prompt {
        let background: Vec<Cell> = (0..rows)
            .flat_map(|r| (0..cols).map(move |c| (r, c)))
            .map(|(r, c)| cell(r, c))
            .collect();
        let (row, col) = (cursor.0.min(rows.saturating_sub(1)), cursor.1);
        let empty = background[row * cols..(row + 1) * cols]
            .iter()
            .all(|c| c.ch == ' ');
        let on_cursor_row = col == 0 && empty;
        let field = match request.name.is_empty() {
            true => Field::Name,
            false => Field::Password,
        };
        Prompt {
            request,
            background,
            cols,
            rows,
            prompt_row: if on_cursor_row { row } else { row + 1 },
            on_cursor_row,
            typed: String::new(),
            password: String::new(),
            field,
            waiting: false,
            failed: false,
            stale: true,
            kept: false,
        }
    }

    /// Takes in a key that the input router gave the request.
    pub fn key(&mut self, key: &SecureKey) -> Step {
        self.type_bytes(key.bytes())
    }

    /// Takes in the bytes of keys typed.
    fn type_bytes(&mut self, bytes: &[u8]) -> Step {
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
        if self.request.ask {
            return self.ask_bytes(bytes);
        }
        // Each key's bytes are whole UTF-8 sequences.
        for c in String::from_utf8_lossy(bytes).chars() {
            let (text, max) = match self.field {
                Field::Name => (&mut self.typed, NAME_MAX),
                Field::Password => (&mut self.password, PASSWORD_MAX),
            };
            match c {
                '\r' | '\n' if self.field == Field::Name => self.field = Field::Password,
                '\r' | '\n' => {
                    self.waiting = true;
                    let name = match self.request.name.is_empty() {
                        true => self.typed.clone(),
                        false => self.request.name.clone(),
                    };
                    return Step::Answer(name, self.password.clone());
                }
                '\x7f' | '\x08' => {
                    text.pop();
                }
                '\x15' => wipe(text),
                c if c < ' ' => {}
                c if text.len() + c.len_utf8() <= max => text.push(c),
                _ => {}
            }
        }
        Step::Nothing
    }

    /// Takes in the keys of an `OnAsk`: `y` allows, `n` or `Enter`
    /// denies, and other keys do nothing.
    fn ask_bytes(&mut self, bytes: &[u8]) -> Step {
        for c in String::from_utf8_lossy(bytes).chars() {
            let allowed = match c {
                'y' | 'Y' => true,
                'n' | 'N' | '\r' | '\n' => false,
                _ => continue,
            };
            if c.is_alphabetic() {
                self.typed.push(c);
            }
            self.waiting = true;
            return Step::AnswerAsk(allowed);
        }
        Step::Nothing
    }

    /// Notes that `Answer` failed: a line says so, which the terminal
    /// keeps.
    pub fn failed(&mut self) {
        self.failed = true;
        self.stale = true;
    }

    /// Whether the screen changed since the last call.
    pub fn take_stale(&mut self) -> bool {
        core::mem::replace(&mut self.stale, false)
    }

    /// The lines, as they show now: the request, the name when it is
    /// asked for, the password once the name is given, and the failure,
    /// with the field that the cursor ends, if any.
    /// Who asks: the app, and the service that asks for it when there is
    /// one (MIKSPEC-0067.R22).
    fn who(&self) -> String {
        let r = &self.request;
        match r.via.is_empty() {
            true => r.app_id.clone(),
            false => format!("{} through {}", r.app_id, r.via),
        }
    }

    fn lines(&self) -> (Vec<String>, Option<usize>) {
        let r = &self.request;
        let sudo = r.action == ADMIN_RUN;
        let prefix = if sudo { SUDO_PREFIX } else { PREFIX };
        let mut lines = Vec::new();
        if r.ask {
            let answer = match self.typed.is_empty() {
                true => String::new(),
                false => format!(" {}", self.typed),
            };
            let line = format!(
                "{PREFIX}{} wants {}: {} {ASK}{answer}",
                self.who(),
                r.action,
                r.reason
            );
            let cursor = (!self.waiting).then_some(0);
            return (vec![line], cursor);
        }
        if !sudo {
            lines.push(format!(
                "{prefix}{} wants {}: {}",
                self.who(),
                r.action,
                r.reason
            ));
        }
        let name = match r.name.is_empty() {
            true => {
                lines.push(format!("{prefix}name: {}", self.typed));
                &self.typed
            }
            false => &r.name,
        };
        let mut cursor = lines.len().checked_sub(1);
        if self.field == Field::Password {
            let stars = "*".repeat(self.password.chars().count());
            lines.push(format!("{prefix}password for {name}: {stars}"));
            cursor = Some(lines.len() - 1);
        }
        if self.failed {
            lines.push(format!("{prefix}{FAILED}"));
        }
        if self.waiting || self.failed {
            cursor = None;
        }
        (lines, cursor)
    }

    /// The authorization screen: the terminal's screen dimmed, scrolled
    /// up when the lines do not fit below the prompt row, and the lines.
    pub fn screen(&self) -> Screen {
        let (cols, rows) = (self.cols, self.rows);
        let (lines, cursor_line) = self.lines();
        let mut wrapped: Vec<String> = Vec::new();
        let mut cursor = None;
        for (i, line) in lines.iter().enumerate() {
            let rows_of = wrap(line, cols);
            if Some(i) == cursor_line {
                let last = rows_of.last().map_or(0, |r| r.chars().count());
                cursor = Some((wrapped.len() + rows_of.len() - 1, last));
            }
            wrapped.extend(rows_of);
        }
        // The lines that fit, the last ones, and how far the screen
        // scrolls up for them.
        let skip = wrapped.len().saturating_sub(rows);
        let shown = &wrapped[skip..];
        let scroll = (self.prompt_row + shown.len()).saturating_sub(rows);
        let top = self.prompt_row - scroll.min(self.prompt_row);
        let mut screen = Screen::new(cols, rows);
        for r in 0..rows {
            let from = r + scroll;
            if from >= rows {
                break;
            }
            for c in 0..cols {
                let mut cell = self.background[from * cols + c];
                cell.attrs |= DIM;
                screen.set_cell(r, c, cell);
            }
        }
        for (i, line) in shown.iter().enumerate() {
            let row = top + i;
            let chars: Vec<char> = line.chars().collect();
            for c in 0..cols {
                let ch = chars.get(c).copied().unwrap_or(' ');
                let cell = Cell {
                    ch,
                    ..Cell::default()
                };
                screen.set_cell(row, c, cell);
            }
        }
        match cursor {
            Some((line, col)) if line >= skip => {
                let row = top + line - skip;
                let at = format!("\x1b[{};{}H", row + 1, col.min(cols - 1) + 1);
                screen.write(at.as_bytes());
            }
            _ => screen.modes.cursor_visible = false,
        }
        screen
    }

    /// What the terminal keeps of the prompt, to write to its screen once
    /// the answer is known, before what its programs write next: the
    /// lines shown, from the prompt row, and the cursor at the start of
    /// the row after them. None once it was given.
    pub fn take_transcript(&mut self) -> Option<String> {
        if std::mem::replace(&mut self.kept, true) {
            return None;
        }
        let (lines, _) = self.lines();
        let start = if self.on_cursor_row { "\r" } else { "\r\n" };
        Some(format!("{start}{}\r\n", lines.join("\r\n")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request(name: &str) -> Request {
        Request {
            request: 7,
            console: 1,
            app_id: "system.power".into(),
            via: String::new(),
            action: "admin.power.control".into(),
            reason: "power off".into(),
            name: name.into(),
            ask: false,
        }
    }

    /// A prompt on an 80 by 6 screen whose first `used` rows hold text,
    /// with the cursor at `cursor`.
    fn prompt(name: &str, used: usize, cursor: (usize, usize)) -> Prompt {
        let cell = move |r: usize, _| Cell {
            ch: if r < used { 'x' } else { ' ' },
            ..Cell::default()
        };
        Prompt::new(request(name), 80, 6, cursor, cell)
    }

    fn rows(screen: &Screen) -> Vec<String> {
        (0..screen.rows())
            .map(|r| {
                (0..screen.cols())
                    .map(|c| screen.cell(r, c).ch)
                    .collect::<String>()
            })
            .map(|r| r.trim_end().to_string())
            .collect()
    }

    #[test]
    fn asks_for_the_name_then_the_password() {
        let mut p = prompt("", 1, (1, 0));
        assert_eq!(p.type_bytes(b"ad"), Step::Nothing);
        assert_eq!(p.type_bytes(b"x\x7fa"), Step::Nothing);
        assert_eq!(p.type_bytes(b"\r"), Step::Nothing);
        assert_eq!(p.type_bytes(b"s3cret"), Step::Nothing);
        assert_eq!(
            p.type_bytes(b"\r"),
            Step::Answer("ada".into(), "s3cret".into())
        );
        // While the answer is checked, keys do nothing.
        assert_eq!(p.type_bytes(b"\x1b"), Step::Nothing);
    }

    #[test]
    fn takes_only_the_password_for_a_named_administrator() {
        let mut p = prompt("ada", 1, (1, 0));
        assert_eq!(p.type_bytes("pä\x15pw".as_bytes()), Step::Nothing);
        assert_eq!(p.type_bytes("ä".as_bytes()), Step::Nothing);
        assert_eq!(
            p.type_bytes(b"\n"),
            Step::Answer("ada".into(), "pwä".into())
        );
    }

    #[test]
    fn cancels_on_escape_but_not_on_arrows() {
        let mut p = prompt("", 1, (1, 0));
        assert_eq!(p.type_bytes(b"\x1b[A"), Step::Nothing);
        assert_eq!(p.type_bytes(b"\x1b"), Step::Cancel);
    }

    #[test]
    fn names_the_service_that_asks_for_the_app() {
        let mut asked = request("");
        asked.via = "system.btrfs".into();
        let p = Prompt::new(asked, 120, 6, (1, 0), |_, _| Cell::default());
        assert_eq!(
            rows(&p.screen())[1],
            "[authorize] system.power through system.btrfs wants admin.power.control: power off"
        );
    }

    #[test]
    fn writes_the_lines_on_the_empty_cursor_row() {
        let mut p = prompt("", 1, (1, 0));
        p.type_bytes(b"ad");
        let screen = p.screen();
        let shown = rows(&screen);
        assert_eq!(shown[0], "x".repeat(80));
        assert!(screen.cell(0, 0).attrs & DIM != 0);
        assert_eq!(
            shown[1],
            "[authorize] system.power wants admin.power.control: power off"
        );
        assert_eq!(shown[2], "[authorize] name: ad");
        assert_eq!(shown[3], "");
        assert_eq!(screen.cell(2, 0).attrs & DIM, 0);
        assert_eq!(screen.cursor(), Some((2, 20)));
        p.type_bytes("\rpä".as_bytes());
        let shown = rows(&p.screen());
        assert_eq!(shown[3], "[authorize] password for ad: **");
        assert!(!shown[3].contains('ä'));
    }

    #[test]
    fn writes_the_lines_below_a_row_with_text_and_scrolls() {
        // The cursor after text on the last row but one: the lines start
        // below it, and the screen scrolls up for the ones that do not
        // fit.
        let mut p = prompt("ada", 5, (4, 3));
        p.type_bytes(b"pw");
        let shown = rows(&p.screen());
        assert_eq!(
            &shown[..3],
            ["x".repeat(80), "x".repeat(80), "x".repeat(80)]
        );
        assert_eq!(shown[3], "x".repeat(80));
        assert_eq!(
            shown[4],
            "[authorize] system.power wants admin.power.control: power off"
        );
        assert_eq!(shown[5], "[authorize] password for ada: **");
    }

    #[test]
    fn keeps_the_lines_with_stars_and_the_failure() {
        let mut p = prompt("ada", 1, (1, 0));
        p.type_bytes(b"pw\r");
        p.failed();
        let screen = p.screen();
        assert_eq!(rows(&screen)[3], "[authorize] authorization failed");
        assert_eq!(screen.cursor(), None);
        assert_eq!(
            p.take_transcript().unwrap(),
            "\r[authorize] system.power wants admin.power.control: power off\r\n\
             [authorize] password for ada: **\r\n[authorize] authorization failed\r\n"
        );
        assert_eq!(p.take_transcript(), None);
        let mut below = prompt("ada", 1, (0, 4));
        assert!(
            below
                .take_transcript()
                .unwrap()
                .starts_with("\r\n[authorize] ")
        );
    }

    #[test]
    fn asks_for_sudo_with_the_password_line_only() {
        let sudo = |name: &str| {
            let mut request = request(name);
            request.action = "admin.run".into();
            let cell = |_, _| Cell::default();
            Prompt::new(request, 60, 6, (1, 0), cell)
        };
        let mut p = sudo("sip");
        p.type_bytes(b"pw");
        let shown = rows(&p.screen());
        assert_eq!(shown[1], "[sudo] password for sip: **");
        assert_eq!(shown[2], "");
        let mut p = sudo("");
        p.type_bytes(b"ada");
        assert_eq!(rows(&p.screen())[1], "[sudo] name: ada");
        p.type_bytes(b"\rx\r");
        p.failed();
        assert_eq!(
            p.take_transcript().unwrap(),
            "\r[sudo] name: ada\r\n[sudo] password for ada: *\r\n[sudo] authorization failed\r\n"
        );
    }

    #[test]
    fn asks_with_one_line_and_keeps_the_answer() {
        let ask = || {
            let mut request = request("");
            request.ask = true;
            request.action = "user.camera.use".into();
            request.reason = "take a photo".into();
            Prompt::new(request, 80, 6, (1, 0), |_, _| Cell::default())
        };
        let line = "[authorize] system.power wants user.camera.use: take a photo [y/N]";
        let mut p = ask();
        assert_eq!(p.type_bytes(b"x\x1b[A"), Step::Nothing);
        let screen = p.screen();
        assert_eq!(rows(&screen)[1], line);
        assert_eq!(rows(&screen)[2], "");
        assert_eq!(screen.cursor(), Some((1, line.len())));
        assert_eq!(p.type_bytes(b"y"), Step::AnswerAsk(true));
        assert_eq!(p.type_bytes(b"n"), Step::Nothing);
        assert_eq!(p.take_transcript().unwrap(), format!("\r{line} y\r\n"));
        let mut p = ask();
        assert_eq!(p.type_bytes(b"N"), Step::AnswerAsk(false));
        assert_eq!(rows(&p.screen())[1], format!("{line} N"));
        let mut p = ask();
        assert_eq!(p.type_bytes(b"\r"), Step::AnswerAsk(false));
        assert_eq!(rows(&p.screen())[1], line);
        let mut p = ask();
        assert_eq!(p.type_bytes(b"\x1b"), Step::Cancel);
    }

    #[test]
    fn wraps_long_lines_as_a_terminal_does() {
        assert_eq!(wrap("abcdefghij", 4), ["abcd", "efgh", "ij"]);
        assert_eq!(wrap("", 4), [""]);
    }
}
