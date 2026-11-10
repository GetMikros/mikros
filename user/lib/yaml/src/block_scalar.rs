//! Literal (`|`) and folded (`>`) scalars.

use crate::parse::Parser;
use crate::{Error, ErrorKind, Node, Style, Value};
use alloc::string::String;
use alloc::vec::Vec;

#[derive(Clone, Copy, PartialEq)]
enum Chomp {
    Strip,
    Clip,
    Keep,
}

impl Parser<'_> {
    /// The block scalar at the cursor, inside a node indented to
    /// `parent`.
    pub(crate) fn block_scalar(&mut self, parent: isize) -> Result<Node, Error> {
        let (line, column) = (self.line, self.column());
        let folded = self.peek() == Some(b'>');
        self.bump();
        let mut chomp = None;
        let mut indicator = None;
        for _ in 0..2 {
            match self.peek() {
                Some(b'-') if chomp.is_none() => chomp = Some(Chomp::Strip),
                Some(b'+') if chomp.is_none() => chomp = Some(Chomp::Keep),
                Some(b @ b'1'..=b'9') if indicator.is_none() => {
                    indicator = Some(usize::from(b - b'0'))
                }
                _ => break,
            }
            self.bump();
        }
        let chomp = chomp.unwrap_or(Chomp::Clip);
        self.header_end()?;
        // An indicator counts from the parent's indentation.
        let mut indent = indicator.map(|i| usize::try_from(parent).unwrap_or(0) + i);
        let mut lines: Vec<&str> = Vec::new();
        let text = self.text;
        // The cursor is at the start of a line.
        loop {
            let mark = self.mark();
            let start = self.pos;
            while self.peek() == Some(b' ') {
                self.bump();
            }
            let spaces = self.pos - start;
            match self.peek() {
                None => {
                    if spaces > indent.unwrap_or(usize::MAX) {
                        lines.push(&text[start + indent.unwrap_or(0)..self.pos]);
                    }
                    break;
                }
                Some(b'\n') => {
                    let extra = indent.filter(|&i| spaces > i);
                    lines.push(extra.map_or("", |i| &text[start + i..self.pos]));
                    self.bump();
                    continue;
                }
                _ => {}
            }
            let need = *indent.get_or_insert(spaces);
            if spaces < need || (spaces as isize) <= parent || self.at_marker() {
                self.reset(mark);
                break;
            }
            while !matches!(self.peek(), None | Some(b'\n')) {
                self.bump();
            }
            lines.push(&text[start + need..self.pos]);
            if self.peek().is_none() {
                break;
            }
            self.bump();
        }
        self.skip_to_content()?;
        let trailing = lines.iter().rev().take_while(|l| l.is_empty()).count();
        let body = &lines[..lines.len() - trailing];
        let mut value = if folded { fold(body) } else { body.join("\n") };
        match chomp {
            Chomp::Strip => {}
            Chomp::Clip if body.is_empty() => {}
            Chomp::Clip => value.push('\n'),
            Chomp::Keep => {
                let breaks = trailing + usize::from(!body.is_empty());
                (0..breaks).for_each(|_| value.push('\n'));
            }
        }
        Ok(self.node(Value::Scalar(value, Style::Block), line, column))
    }

    /// The rest of a block scalar's header line: spaces and a comment.
    fn header_end(&mut self) -> Result<(), Error> {
        let spaced = self.skip_spaces();
        if self.peek() == Some(b'#') && spaced {
            while !matches!(self.peek(), None | Some(b'\n')) {
                self.bump();
            }
        }
        match self.peek() {
            None => Ok(()),
            Some(b'\n') => {
                self.bump();
                Ok(())
            }
            _ => Err(self.error(ErrorKind::Syntax)),
        }
    }
}

/// Joins the lines of a folded scalar: a single break between two
/// lines of text becomes a space, each blank line a break, and lines
/// that start with a space keep their breaks.
fn fold(lines: &[&str]) -> String {
    let mut text = String::new();
    let mut blanks = 0;
    let mut previous: Option<&str> = None;
    for &line in lines {
        if line.is_empty() {
            blanks += 1;
            continue;
        }
        let indented = |l: &str| l.starts_with([' ', '\t']);
        match previous {
            None => (0..blanks).for_each(|_| text.push('\n')),
            Some(p) if !indented(p) && !indented(line) && blanks == 0 => text.push(' '),
            Some(p) if !indented(p) && !indented(line) => {
                (0..blanks).for_each(|_| text.push('\n'));
            }
            Some(_) => (0..=blanks).for_each(|_| text.push('\n')),
        }
        text.push_str(line);
        previous = Some(line);
        blanks = 0;
    }
    text
}
