//! The document, block collections, and the scalars that start them.

use crate::parse::{Parser, is_break_or_space};
use crate::{Entry, Error, ErrorKind, Node, Style, Value};
use alloc::string::String;
use alloc::vec::Vec;

impl Parser<'_> {
    pub(crate) fn document(mut self) -> Result<Node, Error> {
        if self.skip_to_content()?.is_none() {
            return Err(self.error(ErrorKind::RootNotMap));
        }
        if self.column() == 0 && self.peek() == Some(b'%') {
            return Err(self.error(ErrorKind::Unsupported));
        }
        if self.at_marker() && self.peek() == Some(b'-') {
            self.pos += 3;
            self.skip_spaces();
            self.skip_to_content()?;
        }
        let (line, column) = (self.line, self.column());
        let root = match self.peek() {
            None => None,
            _ if self.at_marker() => None,
            _ => Some(self.node_here(-1, true)?),
        };
        if self.skip_to_content()?.is_some() {
            if self.text[self.pos..].starts_with("...") && self.at_marker() {
                self.pos += 3;
                self.skip_spaces();
                if self.skip_to_content()?.is_some() {
                    return Err(self.error(ErrorKind::MultipleDocuments));
                }
            } else if self.at_marker() {
                return Err(self.error(ErrorKind::MultipleDocuments));
            } else {
                return Err(self.error(ErrorKind::Indentation));
            }
        }
        match root {
            Some(
                root @ Node {
                    value: Value::Map(_),
                    ..
                },
            ) => Ok(root),
            _ => Err(Error {
                line: line + 1,
                column: column + 1,
                kind: ErrorKind::RootNotMap,
            }),
        }
    }

    /// The node that starts at the cursor, inside a node indented to
    /// `parent`. A block collection may start here only when `block`.
    pub(crate) fn node_here(&mut self, parent: isize, block: bool) -> Result<Node, Error> {
        let (line, column) = (self.line, self.column());
        let node = match self.peek() {
            Some(b'-') if self.at_entry() => {
                if !block {
                    return Err(self.error(ErrorKind::Syntax));
                }
                self.block_list(column)?
            }
            Some(b'[' | b'{') => {
                let node = self.flow()?;
                self.skip_spaces();
                if self.peek() == Some(b':') && is_break_or_space(self.peek_at(1)) {
                    return Err(Error {
                        line: line + 1,
                        column: column + 1,
                        kind: ErrorKind::NonScalarKey,
                    });
                }
                self.end_line()?;
                node
            }
            Some(b'|' | b'>') => self.block_scalar(parent)?,
            Some(b'?') if is_break_or_space(self.peek_at(1)) => {
                return Err(self.error(ErrorKind::Unsupported));
            }
            Some(b'&' | b'*' | b'!') => return Err(self.error(ErrorKind::Unsupported)),
            _ => {
                let (text, style) = self.scalar_start()?;
                let key_line = self.line;
                self.skip_spaces();
                if self.peek() == Some(b':') && is_break_or_space(self.peek_at(1)) {
                    if !block || key_line != line {
                        return Err(self.error(ErrorKind::Syntax));
                    }
                    let first = Entry {
                        key: text,
                        line: line + 1,
                        column: column + 1,
                        value: self.node(Value::Scalar(String::new(), Style::Plain), line, column),
                    };
                    self.block_map(column, first)?
                } else if style == Style::Plain {
                    let text = self.plain_rest(parent, text)?;
                    self.node(Value::Scalar(text, style), line, column)
                } else {
                    self.end_line()?;
                    self.node(Value::Scalar(text, style), line, column)
                }
            }
        };
        Ok(node)
    }

    /// The value after `key:` or `-` with nothing after it on the line:
    /// a node on a later line indented past `parent`, or, in a map,
    /// a sequence indented as far as the map; otherwise nothing.
    fn block_value(&mut self, parent: usize, in_map: bool) -> Result<Node, Error> {
        let (line, column) = (self.line, self.column());
        let empty = self.node(Value::Scalar(String::new(), Style::Plain), line, column);
        let Some(indent) = self.skip_to_content()? else {
            return Ok(empty);
        };
        if self.at_marker() {
            return Ok(empty);
        }
        if indent > parent || (in_map && indent == parent && self.at_entry()) {
            return self.node_here(parent as isize, true);
        }
        Ok(empty)
    }

    /// A block mapping indented to `indent`, whose first key, at the
    /// cursor's line, was read as `first`.
    fn block_map(&mut self, indent: usize, first: Entry) -> Result<Node, Error> {
        self.enter()?;
        let (line, column) = (first.line - 1, first.column - 1);
        let mut entries: Vec<Entry> = Vec::new();
        let mut next = Some(first);
        loop {
            let mut entry = match next.take() {
                Some(entry) => entry,
                None => self.key()?,
            };
            if entries.iter().any(|e| e.key == entry.key) {
                return Err(Error {
                    line: entry.line,
                    column: entry.column,
                    kind: ErrorKind::DuplicateKey,
                });
            }
            // The cursor is on the colon.
            self.bump();
            entry.value = if self.rest_is_blank() {
                self.skip_spaces();
                self.block_value(indent, true)?
            } else {
                self.skip_spaces();
                self.node_here(indent as isize, false)?
            };
            entries.push(entry);
            match self.peek() {
                None => break,
                _ if self.at_marker() => break,
                _ if self.column() == indent => continue,
                _ if self.column() < indent => break,
                _ => return Err(self.error(ErrorKind::Indentation)),
            }
        }
        self.depth -= 1;
        Ok(self.node(Value::Map(entries), line, column))
    }

    /// A key of a block mapping at the cursor, which stops on its colon.
    fn key(&mut self) -> Result<Entry, Error> {
        let (line, column) = (self.line, self.column());
        match self.peek() {
            Some(b'[' | b'{') => return Err(self.error(ErrorKind::NonScalarKey)),
            Some(b'?') if is_break_or_space(self.peek_at(1)) => {
                return Err(self.error(ErrorKind::Unsupported));
            }
            Some(b'&' | b'*' | b'!') => return Err(self.error(ErrorKind::Unsupported)),
            Some(b'|' | b'>') => return Err(self.error(ErrorKind::NonScalarKey)),
            _ if self.at_entry() => return Err(self.error(ErrorKind::Syntax)),
            _ => {}
        }
        let (key, _) = self.scalar_start()?;
        self.skip_spaces();
        if self.line != line || self.peek() != Some(b':') || !is_break_or_space(self.peek_at(1)) {
            return Err(self.error(ErrorKind::Syntax));
        }
        Ok(Entry {
            key,
            line: line + 1,
            column: column + 1,
            value: self.node(Value::Scalar(String::new(), Style::Plain), line, column),
        })
    }

    /// A block sequence indented to `indent`, at its first `-`.
    fn block_list(&mut self, indent: usize) -> Result<Node, Error> {
        self.enter()?;
        let (line, column) = (self.line, self.column());
        let mut items = Vec::new();
        loop {
            // The cursor is on the dash.
            self.bump();
            let item = if self.rest_is_blank() {
                self.skip_spaces();
                self.block_value(indent, false)?
            } else {
                self.skip_spaces();
                self.node_here(indent as isize, true)?
            };
            items.push(item);
            match self.peek() {
                None => break,
                _ if self.at_marker() => break,
                _ if self.column() == indent && self.at_entry() => continue,
                _ if self.column() <= indent => break,
                _ => return Err(self.error(ErrorKind::Indentation)),
            }
        }
        self.depth -= 1;
        Ok(self.node(Value::List(items), line, column))
    }

    /// A quoted scalar, or the first line of a plain scalar, at the
    /// cursor.
    pub(crate) fn scalar_start(&mut self) -> Result<(String, Style), Error> {
        match self.peek() {
            Some(b'\'' | b'"') => Ok((self.quoted()?, Style::Quoted)),
            _ => Ok((self.plain_line(false)?.into(), Style::Plain)),
        }
    }
}
