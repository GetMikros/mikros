//! The parser's cursor over the text.
//!
//! After each node, the cursor stands on the first character of the
//! next line that holds content, or at the end, so that the caller
//! reads that line's indentation from its column.

use crate::{DEPTH_MAX, Error, ErrorKind, Node, Value};

pub(crate) struct Parser<'a> {
    pub(crate) text: &'a str,
    pub(crate) pos: usize,
    /// The line of `pos`, from 0, and the offset where it starts.
    pub(crate) line: usize,
    pub(crate) line_start: usize,
    pub(crate) depth: usize,
}

/// A position to go back to.
#[derive(Clone, Copy)]
pub(crate) struct Mark {
    pos: usize,
    line: usize,
    line_start: usize,
}

pub(crate) fn is_space(b: Option<u8>) -> bool {
    matches!(b, Some(b' ' | b'\t'))
}

/// Whether `b` ends a token: a space, a line break or the end.
pub(crate) fn is_break_or_space(b: Option<u8>) -> bool {
    matches!(b, None | Some(b' ' | b'\t' | b'\n'))
}

impl<'a> Parser<'a> {
    pub(crate) fn new(text: &'a str) -> Parser<'a> {
        Parser {
            text,
            pos: 0,
            line: 0,
            line_start: 0,
            depth: 0,
        }
    }

    pub(crate) fn peek(&self) -> Option<u8> {
        self.peek_at(0)
    }

    pub(crate) fn peek_at(&self, n: usize) -> Option<u8> {
        self.text.as_bytes().get(self.pos + n).copied()
    }

    pub(crate) fn column(&self) -> usize {
        self.pos - self.line_start
    }

    pub(crate) fn error(&self, kind: ErrorKind) -> Error {
        Error {
            line: self.line + 1,
            column: self.column() + 1,
            kind,
        }
    }

    pub(crate) fn mark(&self) -> Mark {
        Mark {
            pos: self.pos,
            line: self.line,
            line_start: self.line_start,
        }
    }

    pub(crate) fn reset(&mut self, mark: Mark) {
        self.pos = mark.pos;
        self.line = mark.line;
        self.line_start = mark.line_start;
    }

    /// Moves past one character.
    pub(crate) fn bump(&mut self) {
        match self.text[self.pos..].chars().next() {
            Some('\n') => {
                self.pos += 1;
                self.line += 1;
                self.line_start = self.pos;
            }
            Some(c) => self.pos += c.len_utf8(),
            None => {}
        }
    }

    pub(crate) fn skip_spaces(&mut self) -> bool {
        let start = self.pos;
        while is_space(self.peek()) {
            self.bump();
        }
        self.pos != start
    }

    fn skip_comment(&mut self) {
        while !matches!(self.peek(), None | Some(b'\n')) {
            self.bump();
        }
    }

    /// Node starts begin a new level of nesting.
    pub(crate) fn enter(&mut self) -> Result<(), Error> {
        self.depth += 1;
        if self.depth > DEPTH_MAX {
            return Err(self.error(ErrorKind::TooDeep));
        }
        Ok(())
    }

    pub(crate) fn node(&self, value: Value, line: usize, column: usize) -> Node {
        Node {
            value,
            line: line + 1,
            column: column + 1,
        }
    }

    /// Moves to the first character of the next line with content,
    /// skipping blank lines and comments, and returns its column, or
    /// `None` at the end. A cursor already on content stays.
    pub(crate) fn skip_to_content(&mut self) -> Result<Option<usize>, Error> {
        loop {
            let at_line_start = self.pos == self.line_start;
            let mut tab = false;
            while is_space(self.peek()) {
                tab |= at_line_start && self.peek() == Some(b'\t');
                self.bump();
            }
            match self.peek() {
                None => return Ok(None),
                Some(b'\n') => self.bump(),
                Some(b'#') if at_line_start || is_space(self.peek_back()) => {
                    self.skip_comment();
                }
                Some(_) if tab => return Err(self.error(ErrorKind::Indentation)),
                Some(_) => return Ok(Some(self.column())),
            }
        }
    }

    pub(crate) fn peek_back(&self) -> Option<u8> {
        self.pos
            .checked_sub(1)
            .and_then(|at| self.text.as_bytes().get(at).copied())
    }

    /// Ends the line after a value: spaces, a comment after a space,
    /// then a line break or the end. Then moves to the next content.
    pub(crate) fn end_line(&mut self) -> Result<(), Error> {
        self.skip_spaces();
        if self.peek() == Some(b'#') && (self.column() == 0 || is_space(self.peek_back())) {
            self.skip_comment();
        }
        match self.peek() {
            None | Some(b'\n') => {
                self.skip_to_content()?;
                Ok(())
            }
            _ => Err(self.error(ErrorKind::Syntax)),
        }
    }

    /// Whether the rest of the line is blank or a comment.
    pub(crate) fn rest_is_blank(&mut self) -> bool {
        let mark = self.mark();
        let spaced = self.skip_spaces();
        let blank = match self.peek() {
            None | Some(b'\n') => true,
            Some(b'#') => spaced || mark.pos == mark.line_start,
            _ => false,
        };
        self.reset(mark);
        blank
    }

    /// Whether the cursor stands on `---` or `...` in column 0.
    pub(crate) fn at_marker(&self) -> bool {
        self.column() == 0
            && (self.text[self.pos..].starts_with("---")
                || self.text[self.pos..].starts_with("..."))
            && is_break_or_space(self.peek_at(3))
    }

    /// Whether the cursor stands on a block sequence entry.
    pub(crate) fn at_entry(&self) -> bool {
        self.peek() == Some(b'-') && is_break_or_space(self.peek_at(1))
    }
}

pub(crate) fn is_flow(b: Option<u8>) -> bool {
    matches!(b, Some(b',' | b'[' | b']' | b'{' | b'}'))
}
