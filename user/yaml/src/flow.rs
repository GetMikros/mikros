//! Flow collections: `[a, b]` and `{key: value}`.

use crate::parse::{Parser, is_break_or_space, is_flow, is_space};
use crate::{Entry, Error, ErrorKind, Node, Style, Value};
use alloc::string::String;
use alloc::vec::Vec;

impl Parser<'_> {
    /// The flow collection at the cursor, which moves past its end.
    pub(crate) fn flow(&mut self) -> Result<Node, Error> {
        self.enter()?;
        let (line, column) = (self.line, self.column());
        let open = self.peek();
        self.bump();
        let value = if open == Some(b'[') {
            Value::List(self.flow_list()?)
        } else {
            Value::Map(self.flow_map()?)
        };
        self.depth -= 1;
        Ok(self.node(value, line, column))
    }

    fn flow_list(&mut self) -> Result<Vec<Node>, Error> {
        let mut items = Vec::new();
        loop {
            self.flow_space()?;
            if self.peek() == Some(b']') {
                self.bump();
                return Ok(items);
            }
            let (line, column) = (self.line, self.column());
            let item = self.flow_node()?;
            self.flow_space()?;
            // `[key: value]` holds a map with one entry.
            let item = if self.peek() == Some(b':') {
                let Value::Scalar(key, _) = item.value else {
                    return Err(Error {
                        line: item.line,
                        column: item.column,
                        kind: ErrorKind::NonScalarKey,
                    });
                };
                self.bump();
                let value = self.flow_value()?;
                let entry = Entry {
                    key,
                    line: item.line,
                    column: item.column,
                    value,
                };
                self.node(Value::Map(alloc::vec![entry]), line, column)
            } else {
                item
            };
            items.push(item);
            if !self.flow_next(b']')? {
                return Ok(items);
            }
        }
    }

    fn flow_map(&mut self) -> Result<Vec<Entry>, Error> {
        let mut entries: Vec<Entry> = Vec::new();
        loop {
            self.flow_space()?;
            if self.peek() == Some(b'}') {
                self.bump();
                return Ok(entries);
            }
            let (line, column) = (self.line + 1, self.column() + 1);
            let key = self.flow_node()?;
            let Value::Scalar(key, _) = key.value else {
                return Err(Error {
                    line,
                    column,
                    kind: ErrorKind::NonScalarKey,
                });
            };
            if entries.iter().any(|e| e.key == key) {
                return Err(Error {
                    line,
                    column,
                    kind: ErrorKind::DuplicateKey,
                });
            }
            self.flow_space()?;
            let value = if self.peek() == Some(b':') {
                self.bump();
                self.flow_value()?
            } else {
                self.empty()
            };
            entries.push(Entry {
                key,
                line,
                column,
                value,
            });
            if !self.flow_next(b'}')? {
                return Ok(entries);
            }
        }
    }

    /// The value after a colon in a flow collection, empty when the
    /// entry ends there.
    fn flow_value(&mut self) -> Result<Node, Error> {
        self.flow_space()?;
        match self.peek() {
            Some(b',' | b']' | b'}') => Ok(self.empty()),
            _ => self.flow_node(),
        }
    }

    fn empty(&self) -> Node {
        self.node(
            Value::Scalar(String::new(), Style::Plain),
            self.line,
            self.column(),
        )
    }

    /// After an entry: a comma, and then `true`, or the closing
    /// bracket `close`, and then `false`.
    fn flow_next(&mut self, close: u8) -> Result<bool, Error> {
        self.flow_space()?;
        match self.peek() {
            Some(b',') => {
                self.bump();
                Ok(true)
            }
            Some(b) if b == close => {
                self.bump();
                Ok(false)
            }
            _ => Err(self.error(ErrorKind::Syntax)),
        }
    }

    /// Skips spaces, line breaks and comments inside a flow collection.
    fn flow_space(&mut self) -> Result<(), Error> {
        loop {
            match self.peek() {
                None => return Err(self.error(ErrorKind::Unterminated)),
                Some(b' ' | b'\t' | b'\n') => self.bump(),
                Some(b'#') if self.column() == 0 || is_space(self.peek_back_byte()) => {
                    while !matches!(self.peek(), None | Some(b'\n')) {
                        self.bump();
                    }
                }
                Some(_) if self.at_marker() => return Err(self.error(ErrorKind::Unterminated)),
                Some(_) => return Ok(()),
            }
        }
    }

    fn peek_back_byte(&self) -> Option<u8> {
        self.pos
            .checked_sub(1)
            .and_then(|at| self.text.as_bytes().get(at).copied())
            .map(|b| if b == b'\n' { b' ' } else { b })
    }

    /// A node inside a flow collection.
    fn flow_node(&mut self) -> Result<Node, Error> {
        let (line, column) = (self.line, self.column());
        match self.peek() {
            Some(b'[' | b'{') => self.flow(),
            Some(b'\'' | b'"') => {
                let text = self.quoted()?;
                Ok(self.node(Value::Scalar(text, Style::Quoted), line, column))
            }
            Some(b'?') if is_break_or_space(self.peek_at(1)) => {
                Err(self.error(ErrorKind::Unsupported))
            }
            Some(b'&' | b'*' | b'!') => Err(self.error(ErrorKind::Unsupported)),
            _ => {
                let mut text = String::from(self.plain_line(true)?);
                // A plain scalar goes on over line breaks, folded.
                loop {
                    let mark = self.mark();
                    let mut breaks = 0;
                    while matches!(self.peek(), Some(b' ' | b'\t' | b'\n')) {
                        breaks += usize::from(self.peek() == Some(b'\n'));
                        self.bump();
                    }
                    let next = self.peek();
                    let ends = next.is_none()
                        || is_flow(next)
                        || next == Some(b'#')
                        || (next == Some(b':') && is_break_or_space(self.peek_at(1)))
                        || self.at_marker();
                    if breaks == 0 || ends {
                        self.reset(mark);
                        break;
                    }
                    if breaks == 1 {
                        text.push(' ');
                    } else {
                        (1..breaks).for_each(|_| text.push('\n'));
                    }
                    text.push_str(self.plain_text(true));
                }
                Ok(self.node(Value::Scalar(text, Style::Plain), line, column))
            }
        }
    }
}
