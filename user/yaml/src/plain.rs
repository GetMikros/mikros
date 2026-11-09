//! Plain scalars.

use crate::parse::{Parser, is_break_or_space, is_flow, is_space};
use crate::{Error, ErrorKind};
use alloc::string::String;

impl<'a> Parser<'a> {
    /// The part of a plain scalar on the cursor's line, without
    /// trailing spaces. It ends before `: `, ` #` and the line break,
    /// and in a flow collection also before `,[]{}`.
    pub(crate) fn plain_line(&mut self, flow: bool) -> Result<&'a str, Error> {
        let first = self.peek();
        let next = self.peek_at(1);
        let ends_indicator = |b: Option<u8>| is_break_or_space(b) || (flow && is_flow(b));
        let starts = match first {
            Some(b'-' | b'?' | b':') => !ends_indicator(next),
            Some(
                b',' | b'[' | b']' | b'{' | b'}' | b'#' | b'&' | b'*' | b'!' | b'|' | b'>' | b'\''
                | b'"' | b'%' | b'@' | b'`',
            ) => false,
            None | Some(b'\n') => false,
            _ => true,
        };
        if !starts {
            return Err(self.error(ErrorKind::Syntax));
        }
        Ok(self.plain_text(flow))
    }

    /// Like `plain_line`, for a line that continues a plain scalar,
    /// which may start with any character.
    pub(crate) fn plain_text(&mut self, flow: bool) -> &'a str {
        let ends_indicator = |b: Option<u8>| is_break_or_space(b) || (flow && is_flow(b));
        let text = self.text;
        let start = self.pos;
        let mut end = self.pos;
        loop {
            match self.peek() {
                None | Some(b'\n') => break,
                Some(b':') if ends_indicator(self.peek_at(1)) => break,
                Some(b'#') if is_space(self.peek_back()) => break,
                b if flow && is_flow(b) => break,
                Some(b' ' | b'\t') => self.bump(),
                Some(_) => {
                    self.bump();
                    end = self.pos;
                }
            }
        }
        &text[start..end]
    }

    /// The rest of a plain scalar in a block, whose first line was
    /// `first`: the lines after it indented past `parent`, folded.
    pub(crate) fn plain_rest(&mut self, parent: isize, first: String) -> Result<String, Error> {
        let mut text = first;
        loop {
            // The cursor is after the line's text.
            self.skip_spaces();
            if self.peek() != Some(b'\n') {
                break;
            }
            let mark = self.mark();
            let mut breaks = 0;
            while self.peek() == Some(b'\n') {
                self.bump();
                breaks += 1;
                self.skip_spaces();
            }
            let continues = self.peek().is_some()
                && self.peek() != Some(b'#')
                && self.column() as isize > parent
                && !self.at_marker();
            if !continues {
                self.reset(mark);
                break;
            }
            let line = self.plain_text(false);
            if self.peek() == Some(b':') {
                return Err(self.error(ErrorKind::Syntax));
            }
            if breaks == 1 {
                text.push(' ');
            } else {
                (1..breaks).for_each(|_| text.push('\n'));
            }
            text.push_str(line);
        }
        self.end_line()?;
        Ok(text)
    }
}
