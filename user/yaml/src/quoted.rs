//! Single- and double-quoted scalars.

use crate::ErrorKind;
use crate::parse::{Parser, is_space};
use alloc::string::String;

impl Parser<'_> {
    /// The character at the cursor, which the caller moves past.
    fn char(&self) -> Option<char> {
        self.text[self.pos..].chars().next()
    }

    /// The quoted scalar at the cursor, which moves past its closing
    /// quote. Line breaks inside it are folded.
    pub(crate) fn quoted(&mut self) -> Result<String, crate::Error> {
        let quote = self.peek();
        self.bump();
        let mut text = String::new();
        loop {
            match self.peek() {
                None => return Err(self.error(ErrorKind::Unterminated)),
                Some(b'\'') if quote == Some(b'\'') && self.peek_at(1) == Some(b'\'') => {
                    text.push('\'');
                    self.bump();
                    self.bump();
                }
                b if b == quote => {
                    self.bump();
                    return Ok(text);
                }
                Some(b'\\') if quote == Some(b'"') => self.escape(&mut text)?,
                Some(b' ' | b'\t') => {
                    let start = self.pos;
                    self.skip_spaces();
                    if self.peek() != Some(b'\n') {
                        text.push_str(&self.text[start..self.pos]);
                    }
                }
                Some(b'\n') => {
                    if self.at_document_marker_after_break() {
                        return Err(self.error(ErrorKind::Unterminated));
                    }
                    self.fold(&mut text);
                }
                Some(_) => {
                    text.extend(self.char());
                    self.bump();
                }
            }
        }
    }

    /// Whether the line after the cursor's line break starts with a
    /// document marker, which may not stand inside a quoted scalar.
    fn at_document_marker_after_break(&mut self) -> bool {
        let mark = self.mark();
        self.bump();
        let marker = self.at_marker();
        self.reset(mark);
        marker
    }

    /// Folds the line break at the cursor and the blank lines after it:
    /// one break becomes a space, and each blank line a line break.
    fn fold(&mut self, text: &mut String) {
        let mut breaks = 0;
        while self.peek() == Some(b'\n') {
            self.bump();
            breaks += 1;
            while is_space(self.peek()) {
                self.bump();
            }
        }
        if breaks == 1 {
            text.push(' ');
        } else {
            (1..breaks).for_each(|_| text.push('\n'));
        }
    }

    /// The escape sequence at the cursor in a double-quoted scalar.
    fn escape(&mut self, text: &mut String) -> Result<(), crate::Error> {
        let at = self.mark();
        self.bump();
        let simple = match self.peek() {
            Some(b'0') => Some('\0'),
            Some(b'a') => Some('\x07'),
            Some(b'b') => Some('\x08'),
            Some(b't' | b'\t') => Some('\t'),
            Some(b'n') => Some('\n'),
            Some(b'v') => Some('\x0b'),
            Some(b'f') => Some('\x0c'),
            Some(b'r') => Some('\r'),
            Some(b'e') => Some('\x1b'),
            Some(b' ') => Some(' '),
            Some(b'"') => Some('"'),
            Some(b'/') => Some('/'),
            Some(b'\\') => Some('\\'),
            Some(b'N') => Some('\u{85}'),
            Some(b'_') => Some('\u{a0}'),
            Some(b'L') => Some('\u{2028}'),
            Some(b'P') => Some('\u{2029}'),
            _ => None,
        };
        if let Some(c) = simple {
            self.bump();
            text.push(c);
            return Ok(());
        }
        let digits = match self.peek() {
            Some(b'x') => 2,
            Some(b'u') => 4,
            Some(b'U') => 8,
            Some(b'\n') => {
                // An escaped line break joins the lines without a space,
                // but blank lines after it still count.
                self.bump();
                while is_space(self.peek()) {
                    self.bump();
                }
                if self.peek() == Some(b'\n') {
                    let mut rest = String::new();
                    self.fold(&mut rest);
                    text.extend(rest.chars().filter(|&c| c == '\n'));
                    text.push('\n');
                }
                return Ok(());
            }
            _ => {
                self.reset(at);
                return Err(self.error(ErrorKind::BadEscape));
            }
        };
        self.bump();
        let hex = self.text.get(self.pos..self.pos + digits);
        let c = hex
            .filter(|h| h.bytes().all(|b| b.is_ascii_hexdigit()))
            .and_then(|h| u32::from_str_radix(h, 16).ok())
            .and_then(char::from_u32);
        let Some(c) = c else {
            self.reset(at);
            return Err(self.error(ErrorKind::BadEscape));
        };
        self.pos += digits;
        text.push(c);
        Ok(())
    }
}
