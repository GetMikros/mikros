//! What less shows: the lines of its input, cut into rows of the
//! window's width, the rows on the screen above the last row, and the
//! last row, and how the keys move them.

use crate::keys::Key;

/// The columns between tab stops.
const TAB: usize = 8;
/// The last row after a search that found no line.
const NOT_FOUND: &str = "Pattern not found";

/// What follows a key.
#[derive(Debug, PartialEq, Eq)]
pub enum Next {
    Show,
    Quit,
}

/// One screen: its rows of text, then its last row, in a window of
/// `height` rows.
#[derive(Debug, PartialEq, Eq)]
pub struct Shown {
    pub rows: Vec<String>,
    pub last: String,
    pub height: usize,
}

pub struct Pager {
    /// The file name, or `(standard input)`.
    name: String,
    /// The lines read so far.
    lines: Vec<String>,
    /// The rows that the lines take in the window's width.
    rows: Vec<String>,
    /// The row where each line starts.
    first: Vec<usize>,
    /// Whether the whole input has been read.
    ended: bool,
    columns: usize,
    height: usize,
    /// The first row on the screen.
    top: usize,
    /// Whether the last screen was asked for before the whole input was
    /// read, so that it follows the input until its end.
    to_end: bool,
    /// The text typed after `/`, while it is typed.
    typing: Option<String>,
    /// The text of the last search, and the line it found last.
    searched: Option<String>,
    matched: Option<usize>,
    /// Whether a search forward goes on in the lines still to be read.
    pending: bool,
    /// What the last row shows until the next key.
    message: Option<&'static str>,
}

impl Pager {
    /// A pager of `name` in a window of `columns` and `height` rows,
    /// before any input.
    pub fn new(name: &str, columns: usize, height: usize) -> Pager {
        Pager {
            name: name.into(),
            lines: Vec::new(),
            rows: Vec::new(),
            first: Vec::new(),
            ended: false,
            columns: columns.max(1),
            height: height.max(2),
            top: 0,
            to_end: false,
            typing: None,
            searched: None,
            matched: None,
            pending: false,
            message: None,
        }
    }

    /// Adds the next line of the input, without its line feed.
    pub fn push(&mut self, line: String) {
        self.first.push(self.rows.len());
        self.rows.extend(cut(&line, self.columns));
        self.lines.push(line);
        if self.to_end {
            self.top = self.last_top();
        }
        let found = self
            .searched
            .as_ref()
            .is_some_and(|t| self.lines.last().unwrap().contains(t.as_str()));
        if self.pending && found {
            self.pending = false;
            self.show_match(self.lines.len() - 1);
        }
    }

    /// Notes that the whole input has been read.
    pub fn end(&mut self) {
        self.ended = true;
        self.to_end = false;
        if std::mem::take(&mut self.pending) {
            self.message = Some(NOT_FOUND);
        }
    }

    /// Cuts the lines again for a window of `columns` and `height` rows,
    /// keeping the line at the top of the screen there.
    pub fn resize(&mut self, columns: usize, height: usize) {
        let line = self.line_of(self.top);
        self.columns = columns.max(1);
        self.height = height.max(2);
        self.rows.clear();
        self.first.clear();
        for text in &self.lines {
            self.first.push(self.rows.len());
            self.rows.extend(cut(text, self.columns));
        }
        self.top = self
            .first
            .get(line)
            .copied()
            .unwrap_or(0)
            .min(self.last_top());
        if self.to_end {
            self.top = self.last_top();
        }
    }

    /// How many lines of the input are worth reading for what is shown:
    /// those up to two screens past the top, or all of them for the last
    /// screen and for a search that goes on.
    pub fn wanted(&self) -> usize {
        match self.to_end || self.pending {
            true => usize::MAX,
            false => self.line_of(self.top) + 2 * self.page() + 1,
        }
    }

    /// What the screen shows.
    pub fn shown(&self) -> Shown {
        let end = (self.top + self.page()).min(self.rows.len());
        let mut last = self.name.clone();
        if self.ended && end == self.rows.len() {
            last.push_str(" (END)");
        }
        if let Some(message) = self.message {
            last = message.into();
        }
        if let Some(text) = &self.typing {
            last = format!("/{text}");
        }
        Shown {
            rows: self.rows[self.top.min(end)..end].to_vec(),
            last: last.chars().take(self.columns).collect(),
            height: self.height,
        }
    }

    /// Moves the screen as `key` asks.
    pub fn handle(&mut self, key: Key) -> Next {
        let (page, last) = (self.page(), self.last_top());
        self.to_end = false;
        self.pending = false;
        self.message = None;
        if let Some(text) = &mut self.typing {
            match key {
                Key::Char(c) => text.push(c),
                Key::Backspace if !text.is_empty() => {
                    text.pop();
                }
                Key::Enter => {
                    let text = self.typing.take().unwrap_or_default();
                    if !text.is_empty() {
                        self.searched = Some(text);
                    }
                    self.search(true);
                }
                Key::Backspace | Key::Escape => self.typing = None,
                _ => {}
            }
            return Next::Show;
        }
        match key {
            Key::Char(' ' | 'f') | Key::PageDown => self.top = (self.top + page).min(last),
            Key::Char('b') | Key::PageUp => self.top = self.top.saturating_sub(page),
            Key::Enter | Key::Char('j') | Key::Down => self.top = (self.top + 1).min(last),
            Key::Char('k') | Key::Up => self.top = self.top.saturating_sub(1),
            Key::Char('g') | Key::Home => self.top = 0,
            Key::Char('G') | Key::End => {
                self.top = last;
                self.to_end = !self.ended;
            }
            Key::Char('/') => self.typing = Some(String::new()),
            Key::Char('n') => self.search(true),
            Key::Char('N') => self.search(false),
            Key::Char('q') => return Next::Quit,
            _ => {}
        }
        Next::Show
    }

    /// Shows the next line, or the previous one, that contains the text
    /// of the last search, after the line it found last when that is on
    /// the screen and otherwise after the top line. Not finding one, it
    /// goes on in the lines still to be read, or says so.
    fn search(&mut self, forward: bool) {
        let Some(text) = self.searched.clone() else {
            return;
        };
        let on_screen = |line: &usize| {
            let row = self.first[*line];
            row >= self.top && row < self.top + self.page()
        };
        let from = self
            .matched
            .filter(on_screen)
            .unwrap_or(self.line_of(self.top));
        let has = |line: &usize| self.lines[*line].contains(text.as_str());
        let found = match forward {
            true => (from + 1..self.lines.len()).find(has),
            false => (0..from).rev().find(has),
        };
        match found {
            Some(line) => self.show_match(line),
            None if forward && !self.ended => self.pending = true,
            None => self.message = Some(NOT_FOUND),
        }
    }

    /// Shows the line `line` that a search found at the top, or on the
    /// last screen.
    fn show_match(&mut self, line: usize) {
        self.matched = Some(line);
        self.top = self.first[line].min(self.last_top());
    }

    /// The rows of text on a screen, above its last row.
    fn page(&self) -> usize {
        self.height - 1
    }

    /// The top row of the last screen.
    fn last_top(&self) -> usize {
        self.rows.len().saturating_sub(self.page())
    }

    /// The line that row `row` belongs to.
    fn line_of(&self, row: usize) -> usize {
        self.first.partition_point(|&f| f <= row).saturating_sub(1)
    }
}

/// The rows of `line` in a window `columns` wide: tabs as spaces to the
/// next tab stop of the line, the ASCII control characters as `^` and a letter, and
/// the other control characters as U+FFFD.
pub fn cut(line: &str, columns: usize) -> Vec<String> {
    let mut rows = Vec::new();
    let mut row = String::new();
    let (mut width, mut column) = (0, 0);
    for c in line.chars() {
        let shown = match c {
            '\t' => " ".repeat(TAB - column % TAB),
            '\0'..='\x1f' | '\x7f' => format!("^{}", char::from(c as u8 ^ 0x40)),
            c if c.is_control() => char::REPLACEMENT_CHARACTER.to_string(),
            c => c.to_string(),
        };
        for c in shown.chars() {
            if width == columns {
                rows.push(std::mem::take(&mut row));
                width = 0;
            }
            row.push(c);
            width += 1;
            column += 1;
        }
    }
    rows.push(row);
    rows
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A pager of `count` numbered lines, read to the end, in a window of
    /// 20 columns and 5 rows.
    fn numbered(count: usize) -> Pager {
        let mut pager = Pager::new("file", 20, 5);
        for n in 1..=count {
            pager.push(format!("line {n}"));
        }
        pager.end();
        pager
    }

    fn first_row(pager: &Pager) -> String {
        pager.shown().rows.first().cloned().unwrap_or_default()
    }

    #[test]
    fn cuts_lines_into_rows() {
        assert_eq!(cut("", 4), [""]);
        assert_eq!(cut("abcdefghij", 4), ["abcd", "efgh", "ij"]);
        assert_eq!(cut("a\tb", 20), ["a       b"]);
        assert_eq!(cut("ab\tc", 4), ["ab  ", "    ", "c"]);
        assert_eq!(cut("abcde\tf", 4), ["abcd", "e   ", "f"]);
        assert_eq!(cut("a\x1b[1mb\x7f", 20), ["a^[[1mb^?"]);
        assert_eq!(cut("a\u{85}b", 20), ["a\u{fffd}b"]);
    }

    #[test]
    fn shows_a_screen_and_the_name_and_the_end() {
        let mut pager = Pager::new("notes", 20, 5);
        pager.push("one".into());
        pager.push("two".into());
        assert_eq!(
            pager.shown(),
            Shown {
                rows: vec!["one".into(), "two".into()],
                last: "notes".into(),
                height: 5,
            }
        );
        pager.end();
        assert_eq!(pager.shown().last, "notes (END)");
        assert_eq!(numbered(10).shown().last, "file");
    }

    #[test]
    fn moves_by_screens_lines_and_to_the_ends() {
        let mut pager = numbered(10);
        let mut press = |key: Key| {
            assert_eq!(pager.handle(key), Next::Show);
            first_row(&pager)
        };
        assert_eq!(press(Key::Char(' ')), "line 5");
        assert_eq!(press(Key::PageDown), "line 7");
        assert_eq!(press(Key::Char('f')), "line 7");
        assert_eq!(press(Key::Char('b')), "line 3");
        assert_eq!(press(Key::PageUp), "line 1");
        assert_eq!(press(Key::Char('k')), "line 1");
        assert_eq!(press(Key::Enter), "line 2");
        assert_eq!(press(Key::Char('j')), "line 3");
        assert_eq!(press(Key::Down), "line 4");
        assert_eq!(press(Key::Up), "line 3");
        assert_eq!(press(Key::Char('G')), "line 7");
        assert_eq!(press(Key::Char('g')), "line 1");
        assert_eq!(press(Key::End), "line 7");
        assert_eq!(press(Key::Home), "line 1");
        let mut pager = numbered(10);
        assert_eq!(pager.handle(Key::Char('q')), Next::Quit);
        pager.handle(Key::Char('G'));
        assert_eq!(pager.shown().last, "file (END)");
    }

    #[test]
    fn follows_the_input_to_its_end_after_g() {
        let mut pager = Pager::new("(standard input)", 30, 3);
        pager.push("a".into());
        assert_eq!(pager.wanted(), 5);
        pager.handle(Key::Char('G'));
        assert_eq!(pager.wanted(), usize::MAX);
        for line in ["b", "c", "d"] {
            pager.push(line.into());
        }
        assert_eq!(pager.shown().rows, ["c", "d"]);
        pager.end();
        assert_eq!(pager.shown().last, "(standard input) (END)");
        assert_eq!(pager.wanted(), 2 + 4 + 1);
    }

    #[test]
    fn keeps_the_top_line_when_resized() {
        let mut pager = Pager::new("file", 10, 3);
        for line in ["aaaaaaaaaaaaaaa", "b", "c", "d"] {
            pager.push(line.into());
        }
        pager.end();
        pager.handle(Key::Down);
        pager.handle(Key::Down);
        assert_eq!(first_row(&pager), "b");
        pager.resize(5, 3);
        assert_eq!(first_row(&pager), "b");
        pager.resize(20, 10);
        assert_eq!(first_row(&pager), "aaaaaaaaaaaaaaa");
        assert_eq!(pager.shown().height, 10);
    }

    #[test]
    fn searches_forward_and_back() {
        let mut pager = Pager::new("file", 30, 3);
        for line in ["alpha", "beta", "Gamma", "gamma", "delta", "beta 2", "end"] {
            pager.push(line.into());
        }
        pager.end();
        let mut keys = |keys: &str| {
            for c in keys.chars() {
                pager.handle(if c == '\n' { Key::Enter } else { Key::Char(c) });
            }
            let shown = pager.shown();
            (shown.rows.first().cloned().unwrap_or_default(), shown.last)
        };
        assert_eq!(keys("/gam"), ("alpha".into(), "/gam".into()));
        assert_eq!(keys("\n"), ("gamma".into(), "file".into()));
        assert_eq!(keys("/beta\n"), ("beta 2".into(), "file (END)".into()));
        assert_eq!(keys("n"), ("beta 2".into(), NOT_FOUND.into()));
        assert_eq!(keys("N"), ("beta".into(), "file".into()));
        assert_eq!(keys("N"), ("beta".into(), NOT_FOUND.into()));
        assert_eq!(keys("j"), ("Gamma".into(), "file".into()));
        assert_eq!(keys("/nowhere\n"), ("Gamma".into(), NOT_FOUND.into()));
    }

    #[test]
    fn edits_and_cancels_the_text() {
        let mut pager = numbered(10);
        pager.handle(Key::Char('/'));
        for key in [
            Key::Char('l'),
            Key::Char('x'),
            Key::Backspace,
            Key::Char('i'),
        ] {
            pager.handle(key);
        }
        assert_eq!(pager.shown().last, "/li");
        pager.handle(Key::Escape);
        assert_eq!(pager.shown().last, "file");
        pager.handle(Key::Char('/'));
        pager.handle(Key::Backspace);
        assert_eq!(pager.shown().last, "file");
        assert_eq!(pager.handle(Key::Char('q')), Next::Quit);
    }

    #[test]
    fn searches_on_in_the_input_still_to_read() {
        let mut pager = Pager::new("(standard input)", 30, 3);
        pager.push("a".into());
        for key in [Key::Char('/'), Key::Char('z'), Key::Enter] {
            pager.handle(key);
        }
        assert_eq!(pager.wanted(), usize::MAX);
        pager.push("b".into());
        pager.push("z here".into());
        assert_eq!(pager.shown().rows, ["b", "z here"]);
        assert_eq!(pager.wanted(), 1 + 4 + 1);
        pager.handle(Key::Char('n'));
        pager.end();
        assert_eq!(pager.shown().last, NOT_FOUND);
    }
}
