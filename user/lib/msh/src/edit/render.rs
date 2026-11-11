//! Drawing the prompt, the line, the suggestion after it and the lines
//! below it, such as a completion menu, on a terminal `columns` wide,
//! over what was drawn before, and putting the cursor back in the line.
//! Every character takes one column; the terminal wraps at the right
//! margin, and a line feed in the prompt or the line starts a new row.

/// What to draw.
pub struct View<'a> {
    /// The expanded prompt, which may hold escape sequences.
    pub prompt: &'a str,
    pub line: &'a str,
    /// The cursor, a byte offset in `line`.
    pub cursor: usize,
    /// Text shown dimmed after the line.
    pub suggestion: &'a str,
    /// Rows under the line, cut to the width; they may hold escape
    /// sequences.
    pub below: &'a [String],
}

/// What drawing gives: the bytes to write, and the row of the cursor
/// below the first row drawn, which the next drawing starts from.
#[derive(Debug, PartialEq, Eq)]
pub struct Frame {
    pub text: String,
    pub cursor_row: usize,
}

const DIM: &str = "\x1b[2m";
const NORMAL: &str = "\x1b[0m";

/// The length of the escape sequence that starts `s`, or 0.
fn escape_len(s: &str) -> usize {
    let bytes = s.as_bytes();
    if bytes.first() != Some(&0x1B) {
        return 0;
    }
    match bytes.get(1) {
        Some(b'[') => bytes[2..]
            .iter()
            .position(|b| (0x40..=0x7E).contains(b))
            .map_or(bytes.len(), |end| end + 3),
        Some(b']') => match bytes.iter().position(|&b| b == 0x07) {
            Some(bel) => bel + 1,
            None => s.find("\x1b\\").map_or(bytes.len(), |st| st + 2),
        },
        Some(_) => 2,
        None => 1,
    }
}

/// The text of `s` without its escape sequences, which take no columns.
pub fn visible(s: &str) -> String {
    let mut out = String::new();
    let mut rest = s;
    while let Some(c) = rest.chars().next() {
        let skip = escape_len(rest);
        if skip > 0 {
            rest = &rest[skip..];
            continue;
        }
        out.push(c);
        rest = &rest[c.len_utf8()..];
    }
    out
}

/// `s` cut to `width` visible characters, keeping its escape sequences.
pub fn cut(s: &str, width: usize) -> String {
    let mut out = String::new();
    let mut shown = 0;
    let mut rest = s;
    while let Some(c) = rest.chars().next() {
        let skip = escape_len(rest);
        if skip > 0 {
            out.push_str(&rest[..skip]);
            rest = &rest[skip..];
            continue;
        }
        if shown == width {
            break;
        }
        out.push(c);
        shown += 1;
        rest = &rest[c.len_utf8()..];
    }
    out
}

/// A position on the terminal while text is written: the column may
/// equal the width, when the next character wraps.
#[derive(Clone, Copy)]
struct At {
    row: usize,
    column: usize,
}

impl At {
    fn advance(&mut self, c: char, columns: usize) {
        if c == '\n' {
            self.row += 1;
            self.column = 0;
            return;
        }
        if self.column == columns {
            self.row += 1;
            self.column = 0;
        }
        self.column += 1;
    }

    /// Where the cursor shows: past the right margin is the next row.
    fn settled(self, columns: usize) -> At {
        match self.column == columns {
            true => At {
                row: self.row + 1,
                column: 0,
            },
            false => self,
        }
    }
}

/// How a character of the line is written: a line feed as a new row,
/// other control characters as a space.
fn shown(c: char) -> char {
    match c {
        '\n' => '\n',
        c if c.is_control() => ' ',
        c => c,
    }
}

/// Draws `view` over the frame drawn before, whose cursor was
/// `previous_row` rows below its first row.
pub fn draw(view: &View, columns: usize, previous_row: usize) -> Frame {
    let columns = columns.max(1);
    let mut text = String::from("\r");
    if previous_row > 0 {
        text.push_str(&format!("\x1b[{previous_row}A"));
    }
    text.push_str("\x1b[J");
    let mut at = At { row: 0, column: 0 };
    let mut cursor = None;
    text.push_str(&view.prompt.replace('\n', "\r\n"));
    for c in visible(view.prompt).chars() {
        at.advance(c, columns);
    }
    for (i, c) in view.line.char_indices() {
        if i == view.cursor {
            cursor = Some(at);
        }
        let c = shown(c);
        match c {
            '\n' => text.push_str("\r\n"),
            c => text.push(c),
        }
        at.advance(c, columns);
    }
    let cursor = cursor.unwrap_or(at).settled(columns);
    if !view.suggestion.is_empty() {
        text.push_str(DIM);
        for c in view.suggestion.chars().map(shown) {
            match c {
                '\n' => text.push_str("\r\n"),
                c => text.push(c),
            }
            at.advance(c, columns);
        }
        text.push_str(NORMAL);
    }
    // Past the right margin, the terminal waits to wrap: go on to the
    // next row, as the positions above count.
    if at.column == columns {
        text.push_str("\r\n");
    }
    let mut row = at.settled(columns).row;
    for line in view.below {
        text.push_str("\r\n");
        text.push_str(&cut(line, columns - 1));
        text.push_str(NORMAL);
        row += 1;
    }
    if row > cursor.row {
        text.push_str(&format!("\x1b[{}A", row - cursor.row));
    }
    text.push('\r');
    if cursor.column > 0 {
        text.push_str(&format!("\x1b[{}C", cursor.column));
    }
    Frame {
        text,
        cursor_row: cursor.row,
    }
}

/// Draws the line as it is accepted, without suggestion or rows below,
/// and moves to the start of the row after it.
pub fn finish(prompt: &str, line: &str, columns: usize, previous_row: usize) -> String {
    let view = View {
        prompt,
        line,
        cursor: line.len(),
        suggestion: "",
        below: &[],
    };
    let mut text = draw(&view, columns, previous_row).text;
    text.push_str("\r\n");
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    fn view<'a>(prompt: &'a str, line: &'a str, cursor: usize) -> View<'a> {
        View {
            prompt,
            line,
            cursor,
            suggestion: "",
            below: &[],
        }
    }

    #[test]
    fn counts_text_without_escapes() {
        assert_eq!(visible("\x1b[1;32mada\x1b[0m@box %"), "ada@box %");
        assert_eq!(visible("\x1b]0;title\x07> "), "> ");
        assert_eq!(cut("\x1b[7mabcdef\x1b[0m", 3), "\x1b[7mabc");
    }

    #[test]
    fn draws_the_line_and_puts_the_cursor_back() {
        let f = draw(&view("% ", "ls -l", 2), 80, 0);
        assert_eq!(f.text, "\r\x1b[J% ls -l\r\x1b[4C");
        assert_eq!(f.cursor_row, 0);
        // A suggestion is dimmed, and the cursor stays before it.
        let mut v = view("% ", "ls", 2);
        v.suggestion = " -l";
        let f = draw(&v, 80, 2);
        assert_eq!(f.text, "\r\x1b[2A\x1b[J% ls\x1b[2m -l\x1b[0m\r\x1b[4C");
    }

    #[test]
    fn counts_rows_where_the_line_wraps() {
        // 4 + 6 characters on 5 columns: two rows, the cursor at the end
        // on the third.
        let f = draw(&view("ab% ", "cdefgh", 6), 5, 0);
        assert_eq!(f.text, "\r\x1b[Jab% cdefgh\r\n\r");
        assert_eq!(f.cursor_row, 2);
        let f = draw(&view("ab% ", "cdefgh", 2), 5, 0);
        assert_eq!(f.text, "\r\x1b[Jab% cdefgh\r\n\x1b[1A\r\x1b[1C");
        assert_eq!(f.cursor_row, 1);
    }

    #[test]
    fn draws_rows_below_and_comes_back() {
        let below = ["one".to_string(), "\x1b[7mtwo three\x1b[0m".to_string()];
        let mut v = view("% ", "x", 1);
        v.below = &below;
        let f = draw(&v, 6, 0);
        assert_eq!(
            f.text,
            "\r\x1b[J% x\r\none\x1b[0m\r\n\x1b[7mtwo t\x1b[0m\x1b[2A\r\x1b[3C"
        );
        assert_eq!(finish("% ", "x", 6, 0), "\r\x1b[J% x\r\x1b[3C\r\n");
    }

    #[test]
    fn starts_rows_at_line_feeds() {
        let f = draw(&view("% ", "if x {\n  y", 10), 80, 0);
        assert_eq!(f.text, "\r\x1b[J% if x {\r\n  y\r\x1b[3C");
        assert_eq!(f.cursor_row, 1);
    }
}
