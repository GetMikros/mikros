//! The text of the line being edited and the cursor in it, with the
//! moves and changes of the Emacs-like key map. Words are runs of
//! letters, digits and `_`, except for the kill before the cursor of
//! Ctrl-W, whose words end at blanks.

/// The line and the cursor, a byte offset at a character boundary.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Line {
    text: String,
    cursor: usize,
}

fn word_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

impl Line {
    /// `text` with the cursor at its end.
    pub fn new(text: &str) -> Line {
        Line {
            text: text.into(),
            cursor: text.len(),
        }
    }

    pub fn text(&self) -> &str {
        &self.text
    }

    pub fn cursor(&self) -> usize {
        self.cursor
    }

    /// Replaces the text, with the cursor at its end.
    pub fn set(&mut self, text: &str) {
        *self = Line::new(text);
    }

    /// Replaces the bytes from `start` to the cursor with `text`, leaving
    /// the cursor after it.
    pub fn replace_before(&mut self, start: usize, text: &str) {
        self.text.replace_range(start..self.cursor, text);
        self.cursor = start + text.len();
    }

    pub fn insert(&mut self, text: &str) {
        self.text.insert_str(self.cursor, text);
        self.cursor += text.len();
    }

    fn before(&self) -> Option<char> {
        self.text[..self.cursor].chars().next_back()
    }

    fn after(&self) -> Option<char> {
        self.text[self.cursor..].chars().next()
    }

    pub fn left(&mut self) {
        if let Some(c) = self.before() {
            self.cursor -= c.len_utf8();
        }
    }

    pub fn right(&mut self) {
        if let Some(c) = self.after() {
            self.cursor += c.len_utf8();
        }
    }

    /// Moves the cursor to the line above in a text of several lines, to
    /// the same column or the end of that line; `false` on the first.
    pub fn up(&mut self) -> bool {
        let start = self.line_start();
        if start == 0 {
            return false;
        }
        let column = self.text[start..self.cursor].chars().count();
        let above = self.text[..start - 1].rfind('\n').map_or(0, |i| i + 1);
        self.cursor = self.column_in(above, start - 1, column);
        true
    }

    /// Moves the cursor to the line below as `up` does; `false` on the
    /// last line.
    pub fn down(&mut self) -> bool {
        let end = self.line_end();
        if end == self.text.len() {
            return false;
        }
        let column = self.text[self.line_start()..self.cursor].chars().count();
        let below = end + 1;
        let below_end = self.text[below..]
            .find('\n')
            .map_or(self.text.len(), |i| below + i);
        self.cursor = self.column_in(below, below_end, column);
        true
    }

    /// Moves the cursor to the end of the first line.
    pub fn first_line_end(&mut self) {
        self.cursor = self.text.find('\n').unwrap_or(self.text.len());
    }

    /// Where the line that holds the cursor starts.
    fn line_start(&self) -> usize {
        self.text[..self.cursor].rfind('\n').map_or(0, |i| i + 1)
    }

    /// Where the line that holds the cursor ends, before its line feed.
    fn line_end(&self) -> usize {
        self.text[self.cursor..]
            .find('\n')
            .map_or(self.text.len(), |i| self.cursor + i)
    }

    /// The offset `column` characters into the line from `start` to `end`,
    /// or `end` when the line is shorter.
    fn column_in(&self, start: usize, end: usize, column: usize) -> usize {
        self.text[start..end]
            .char_indices()
            .nth(column)
            .map_or(end, |(i, _)| start + i)
    }

    /// Moves the cursor to the start of its line.
    pub fn home(&mut self) {
        self.cursor = self.line_start();
    }

    /// Moves the cursor to the end of its line.
    pub fn end(&mut self) {
        self.cursor = self.line_end();
    }

    pub fn at_end(&self) -> bool {
        self.cursor == self.text.len()
    }

    /// Where the word before the cursor starts, past what is not a word.
    fn word_start(&self, word: fn(char) -> bool) -> usize {
        let mut at = self.cursor;
        let mut chars = self.text[..at].chars().rev().peekable();
        while let Some(&c) = chars.peek().filter(|&&c| !word(c)) {
            at -= c.len_utf8();
            chars.next();
        }
        while let Some(&c) = chars.peek().filter(|&&c| word(c)) {
            at -= c.len_utf8();
            chars.next();
        }
        at
    }

    /// Where the word after the cursor ends, past what is not a word.
    fn word_end(&self) -> usize {
        let mut at = self.cursor;
        let mut chars = self.text[at..].chars().peekable();
        while let Some(&c) = chars.peek().filter(|&&c| !word_char(c)) {
            at += c.len_utf8();
            chars.next();
        }
        while let Some(&c) = chars.peek().filter(|&&c| word_char(c)) {
            at += c.len_utf8();
            chars.next();
        }
        at
    }

    pub fn word_left(&mut self) {
        self.cursor = self.word_start(word_char);
    }

    pub fn word_right(&mut self) {
        self.cursor = self.word_end();
    }

    /// Removes the character before the cursor.
    pub fn backspace(&mut self) {
        let end = self.cursor;
        self.left();
        self.text.replace_range(self.cursor..end, "");
    }

    /// Removes the character at the cursor.
    pub fn delete(&mut self) {
        if let Some(c) = self.after() {
            self.text
                .replace_range(self.cursor..self.cursor + c.len_utf8(), "");
        }
    }

    /// Removes and returns the bytes from `from` to `to`, around the
    /// cursor, which ends at `from`.
    fn cut(&mut self, from: usize, to: usize) -> String {
        let cut = self.text[from..to].to_owned();
        self.text.replace_range(from..to, "");
        self.cursor = from;
        cut
    }

    /// Ctrl-K: the rest of the line.
    pub fn kill_to_end(&mut self) -> String {
        // At the end of a line, the line feed, which joins the next.
        let end = match self.line_end() {
            end if end == self.cursor && end < self.text.len() => end + 1,
            end => end,
        };
        self.cut(self.cursor, end)
    }

    /// Ctrl-U: the line before the cursor.
    pub fn kill_to_start(&mut self) -> String {
        self.cut(self.line_start(), self.cursor)
    }

    /// Ctrl-W: back to the blank before the cursor's word.
    pub fn kill_blank_word(&mut self) -> String {
        let start = self.word_start(|c| !c.is_whitespace());
        self.cut(start, self.cursor)
    }

    /// Alt-Backspace: back to the start of the word before the cursor.
    pub fn kill_word_back(&mut self) -> String {
        let start = self.word_start(word_char);
        self.cut(start, self.cursor)
    }

    /// Alt-D: to the end of the word after the cursor.
    pub fn kill_word(&mut self) -> String {
        let (from, to) = (self.cursor, self.word_end());
        let cut = self.cut(from, to);
        self.cursor = from;
        cut
    }

    /// Ctrl-T: swaps the characters before and at the cursor, or the
    /// last two at the end of the line, and moves past them.
    pub fn transpose(&mut self) {
        if self.at_end() {
            self.left();
        }
        let (Some(a), Some(b)) = (self.before(), self.after()) else {
            return;
        };
        let start = self.cursor - a.len_utf8();
        let end = self.cursor + b.len_utf8();
        self.text.replace_range(start..end, &format!("{b}{a}"));
        self.cursor = end;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(text: &str, cursor: usize) -> Line {
        Line {
            text: text.into(),
            cursor,
        }
    }

    #[test]
    fn moves_by_characters_and_words() {
        let mut l = Line::new("ls -la /usr/bin");
        l.word_left();
        assert_eq!(l.cursor(), 12);
        l.word_left();
        assert_eq!(l.cursor(), 8);
        l.word_left();
        l.word_left();
        assert_eq!(l.cursor(), 0);
        l.word_right();
        assert_eq!(l.cursor(), 2);
        l.right();
        l.right();
        assert_eq!(l.cursor(), 4);
        let mut l = Line::new("é€");
        l.left();
        assert_eq!(l.cursor(), 2);
        l.home();
        l.right();
        assert_eq!(l.cursor(), 2);
    }

    #[test]
    fn moves_and_kills_within_the_line_of_several() {
        let mut l = at("ab\ncdef\ng", 5);
        l.home();
        assert_eq!(l.cursor(), 3);
        l.end();
        assert_eq!(l.cursor(), 7);
        assert!(l.up());
        assert_eq!(l.cursor(), 2);
        assert!(!l.up());
        assert!(l.down() && l.down());
        assert_eq!(l.cursor(), 9);
        assert!(!l.down());
        let mut l = at("ab\ncdef\ng", 5);
        assert_eq!(l.kill_to_end(), "ef");
        assert_eq!(l.kill_to_end(), "\n");
        assert_eq!(l.kill_to_start(), "cd");
        assert_eq!((l.text(), l.cursor()), ("ab\ng", 3));
    }

    #[test]
    fn inserts_and_deletes() {
        let mut l = at("lsl", 2);
        l.insert(" -");
        assert_eq!((l.text(), l.cursor()), ("ls -l", 4));
        l.backspace();
        l.delete();
        assert_eq!((l.text(), l.cursor()), ("ls ", 3));
        let mut l = at("aé", 3);
        l.backspace();
        assert_eq!(l.text(), "a");
    }

    #[test]
    fn kills_parts_of_the_line() {
        let mut l = at("cat /usr/share/x.txt | wc", 14);
        assert_eq!(l.kill_word_back(), "share");
        assert_eq!(l.kill_blank_word(), "/usr/");
        assert_eq!(l.kill_to_end(), "/x.txt | wc");
        assert_eq!(l.kill_to_start(), "cat ");
        assert_eq!(l.text(), "");
        let mut l = at("one two", 3);
        assert_eq!(l.kill_word(), " two");
        assert_eq!((l.text(), l.cursor()), ("one", 3));
    }

    #[test]
    fn transposes_characters() {
        let mut l = at("sl", 1);
        l.transpose();
        assert_eq!((l.text(), l.cursor()), ("ls", 2));
        l.transpose();
        assert_eq!(l.text(), "sl");
        let mut l = at("x", 0);
        l.transpose();
        assert_eq!(l.text(), "x");
    }
}
