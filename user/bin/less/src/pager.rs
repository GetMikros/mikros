//! What less shows: the lines of its input, cut into rows of the
//! window's width, the rows on the screen above the last row, and the
//! last row, and how the keys move them.

use crate::keys::Key;

/// The columns between tab stops.
const TAB: usize = 8;

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
    }

    /// Notes that the whole input has been read.
    pub fn end(&mut self) {
        self.ended = true;
        self.to_end = false;
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
    /// screen.
    pub fn wanted(&self) -> usize {
        match self.to_end {
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
            Key::Char('q') => return Next::Quit,
            _ => {}
        }
        Next::Show
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
}
