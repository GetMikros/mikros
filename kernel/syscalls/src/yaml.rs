//! A reader for Mikros YAML files (MIKSPEC-0082).
//!
//! yaml-rust2 scans and parses the text. This module rejects what Mikros
//! files may not use, keeps the style of every scalar so that types come
//! from the reader of each key rather than from the text, and reports
//! errors with their line and column.

use std::fmt;
use yaml_rust2::parser::{Event, MarkedEventReceiver, Parser};
use yaml_rust2::scanner::{Marker, Scanner, TScalarStyle, TokenType};

/// The largest file accepted, in bytes.
pub const MAX_SIZE: usize = 16 << 20;
/// The deepest nesting of collections accepted.
pub const MAX_DEPTH: usize = 32;

/// An error at a position in the file. Lines and columns count from 1.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Error {
    pub line: usize,
    pub column: usize,
    pub message: String,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}: {}", self.line, self.column, self.message)
    }
}

impl std::error::Error for Error {}

/// A position in the file. Lines and columns count from 1.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Position {
    pub line: usize,
    pub column: usize,
}

impl Position {
    fn of(mark: Marker) -> Position {
        Position {
            line: mark.line(),
            column: mark.col() + 1,
        }
    }

    /// An error at this position.
    pub fn error(self, message: impl Into<String>) -> Error {
        Error {
            line: self.line,
            column: self.column,
            message: message.into(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Node {
    /// A scalar's text, and whether it was plain rather than quoted or a
    /// block scalar.
    Scalar {
        text: String,
        plain: bool,
    },
    List(Vec<Value>),
    Map(Vec<(Value, Value)>),
}

/// A node and where it starts.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Value {
    pub node: Node,
    pub at: Position,
}

impl Value {
    /// Whether the value stands for an absent key: the plain scalar
    /// `null` or `~`, or nothing.
    pub fn is_absent(&self) -> bool {
        matches!(&self.node, Node::Scalar { text, plain: true } if matches!(text.as_str(), "" | "~" | "null"))
    }

    /// The value as a string: the text of any scalar.
    pub fn as_str(&self) -> Result<&str, Error> {
        match &self.node {
            Node::Scalar { text, .. } => Ok(text),
            _ => Err(self.at.error("expected a string")),
        }
    }

    /// The value as a boolean: the plain scalar `true` or `false`.
    pub fn as_bool(&self) -> Result<bool, Error> {
        match &self.node {
            Node::Scalar { text, plain: true } if text == "true" => Ok(true),
            Node::Scalar { text, plain: true } if text == "false" => Ok(false),
            _ => Err(self.at.error("expected `true` or `false`")),
        }
    }

    /// The value as an integer from `min` to `max`: a plain scalar of
    /// decimal digits with an optional leading `-`, or `0x` and
    /// hexadecimal digits.
    pub fn as_int(&self, min: i128, max: i128) -> Result<i128, Error> {
        let bad = || {
            self.at
                .error(format!("expected an integer from {min} to {max}"))
        };
        let Node::Scalar { text, plain: true } = &self.node else {
            return Err(bad());
        };
        let digits = |s: &str, radix: u32| !s.is_empty() && s.chars().all(|c| c.is_digit(radix));
        let value = if let Some(hex) = text.strip_prefix("0x") {
            digits(hex, 16)
                .then(|| i128::from_str_radix(hex, 16).ok())
                .flatten()
        } else {
            let unsigned = text.strip_prefix('-').unwrap_or(text);
            digits(unsigned, 10).then(|| text.parse().ok()).flatten()
        };
        value.filter(|v| (min..=max).contains(v)).ok_or_else(bad)
    }

    /// The value as a list.
    pub fn as_list(&self) -> Result<&[Value], Error> {
        match &self.node {
            Node::List(items) => Ok(items),
            _ => Err(self.at.error("expected a list")),
        }
    }

    /// The value as a map, read key by key.
    pub fn as_map(&self) -> Result<Map<'_>, Error> {
        match &self.node {
            Node::Map(entries) => Ok(Map {
                entries,
                used: vec![false; entries.len()],
                at: self.at,
            }),
            _ => Err(self.at.error("expected a map")),
        }
    }
}

/// A map being read. Keys are compared as text.
pub struct Map<'v> {
    entries: &'v [(Value, Value)],
    used: Vec<bool>,
    at: Position,
}

impl<'v> Map<'v> {
    /// Where the map starts.
    pub fn at(&self) -> Position {
        self.at
    }

    /// The value of `key`, or `None` when it is missing or absent.
    pub fn get(&mut self, key: &str) -> Option<&'v Value> {
        let i = self
            .entries
            .iter()
            .position(|(k, _)| k.as_str().is_ok_and(|k| k == key))?;
        self.used[i] = true;
        let value = &self.entries[i].1;
        (!value.is_absent()).then_some(value)
    }

    /// The value of `key`, or an error when it is missing or absent.
    pub fn require(&mut self, key: &str) -> Result<&'v Value, Error> {
        let at = self.at;
        self.get(key)
            .ok_or_else(|| at.error(format!("missing key `{key}`")))
    }

    /// The keys that were not read, with their positions, to be ignored
    /// and reported.
    pub fn unknown(&self) -> Vec<(String, Position)> {
        self.entries
            .iter()
            .zip(&self.used)
            .filter(|(_, used)| !**used)
            .map(|((k, _), _)| (k.as_str().unwrap_or("").to_owned(), k.at))
            .collect()
    }
}

enum Frame {
    List(Vec<Value>, Position),
    Map(Vec<(Value, Value)>, Option<Value>, Position),
}

/// Builds the tree from parser events, refusing what Mikros files may
/// not contain.
#[derive(Default)]
struct Builder {
    stack: Vec<Frame>,
    root: Option<Value>,
    documents: usize,
    error: Option<Error>,
}

impl Builder {
    fn fail(&mut self, at: Position, message: &str) {
        if self.error.is_none() {
            self.error = Some(at.error(message));
        }
    }

    fn open(&mut self, frame: Frame, at: Position, anchor: usize, tagged: bool) {
        if anchor != 0 {
            self.fail(at, "anchors are not allowed");
        } else if tagged {
            self.fail(at, "tags are not allowed");
        } else if self.stack.len() >= MAX_DEPTH {
            self.fail(
                at,
                &format!("collections are nested more than {MAX_DEPTH} deep"),
            );
        } else {
            self.stack.push(frame);
        }
    }

    fn add(&mut self, value: Value) {
        match self.stack.last_mut() {
            None => self.root = Some(value),
            Some(Frame::List(items, _)) => items.push(value),
            Some(Frame::Map(entries, key @ None, _)) => {
                let at = value.at;
                let Node::Scalar { text, .. } = &value.node else {
                    return self.fail(at, "keys must be scalars");
                };
                if entries
                    .iter()
                    .any(|(k, _)| k.as_str().is_ok_and(|k| k == text))
                {
                    let message = format!("the key `{text}` appears twice");
                    return self.fail(at, &message);
                }
                *key = Some(value);
            }
            Some(Frame::Map(entries, key, _)) => {
                if let Some(key) = key.take() {
                    entries.push((key, value));
                }
            }
        }
    }
}

impl MarkedEventReceiver for Builder {
    fn on_event(&mut self, event: Event, mark: Marker) {
        if self.error.is_some() {
            return;
        }
        let at = Position::of(mark);
        match event {
            Event::DocumentStart => {
                self.documents += 1;
                if self.documents > 1 {
                    self.fail(at, "only one document is allowed");
                }
            }
            Event::Alias(_) => self.fail(at, "aliases are not allowed"),
            Event::Scalar(text, style, anchor, tag) => {
                if anchor != 0 {
                    return self.fail(at, "anchors are not allowed");
                }
                if tag.is_some() {
                    return self.fail(at, "tags are not allowed");
                }
                let plain = style == TScalarStyle::Plain;
                self.add(Value {
                    node: Node::Scalar { text, plain },
                    at,
                });
            }
            Event::SequenceStart(anchor, tag) => {
                self.open(Frame::List(Vec::new(), at), at, anchor, tag.is_some())
            }
            Event::MappingStart(anchor, tag) => {
                self.open(Frame::Map(Vec::new(), None, at), at, anchor, tag.is_some())
            }
            Event::SequenceEnd | Event::MappingEnd => {
                let value = match self.stack.pop() {
                    Some(Frame::List(items, at)) => Value {
                        node: Node::List(items),
                        at,
                    },
                    Some(Frame::Map(entries, _, at)) => Value {
                        node: Node::Map(entries),
                        at,
                    },
                    None => return,
                };
                self.add(value);
            }
            Event::Nothing | Event::StreamStart | Event::StreamEnd | Event::DocumentEnd => {}
        }
    }
}

/// The position of byte `index` of `text`.
fn position_of(text: &[u8], index: usize) -> Position {
    let before = &text[..index];
    let line_start = before
        .iter()
        .rposition(|&b| b == b'\n')
        .map_or(0, |i| i + 1);
    Position {
        line: before.iter().filter(|&&b| b == b'\n').count() + 1,
        column: String::from_utf8_lossy(&before[line_start..])
            .chars()
            .count()
            + 1,
    }
}

/// Reads a Mikros YAML file, whose root must be a map.
pub fn parse(bytes: &[u8]) -> Result<Value, Error> {
    let start = Position { line: 1, column: 1 };
    if bytes.len() > MAX_SIZE {
        return Err(start.error(format!("the file is larger than {MAX_SIZE} bytes")));
    }
    let text = std::str::from_utf8(bytes)
        .map_err(|e| position_of(bytes, e.valid_up_to()).error("the file is not UTF-8"))?;
    if let Some(i) = text.find('\0') {
        return Err(position_of(bytes, i).error("the file contains a null byte"));
    }
    let text = text.strip_prefix('\u{FEFF}').unwrap_or(text);
    let chars: Vec<char> = text.chars().collect();

    // The parser does not report directives or explicit keys, so the
    // tokens are checked first.
    let mut scanner = Scanner::new(text.chars());
    for token in scanner.by_ref() {
        let at = Position::of(token.0);
        match token.1 {
            TokenType::VersionDirective(..) | TokenType::TagDirective(..) => {
                return Err(at.error("directives are not allowed"));
            }
            TokenType::Key => {
                let i = token.0.index();
                let explicit = chars.get(i) == Some(&'?')
                    && chars.get(i + 1).is_none_or(|c| c.is_whitespace());
                if explicit {
                    return Err(at.error("explicit keys are not allowed"));
                }
            }
            _ => {}
        }
    }
    if let Some(e) = scanner.get_error() {
        return Err(Position::of(*e.marker()).error(e.info()));
    }

    let mut builder = Builder::default();
    Parser::new_from_str(text)
        .load(&mut builder, true)
        .map_err(|e| Position::of(*e.marker()).error(e.info()))?;
    if let Some(e) = builder.error {
        return Err(e);
    }
    match builder.root {
        Some(
            root @ Value {
                node: Node::Map(_), ..
            },
        ) => Ok(root),
        Some(other) => Err(other.at.error("the root must be a map")),
        None => Err(start.error("the root must be a map")),
    }
}

#[cfg(test)]
mod tests;
