//! Reading YAML files into a tree of nodes, and reading typed values
//! from it.
//!
//! The reader takes a single document whose root is a map, with block
//! and flow collections, every scalar style and comments. It rejects
//! anchors, aliases, tags, directives, explicit keys, keys that are not
//! scalars, keys repeated in one map, and more than one document. Values
//! keep their text; their type is chosen by the reader of each key
//! through `Node::string`, `Node::boolean`, `Node::integer`,
//! `Node::list` and `Node::map`.

#![no_std]

extern crate alloc;

mod block;
mod parse;
mod plain;

use alloc::string::String;
use alloc::vec::Vec;

/// The largest file the reader takes, in bytes.
pub const SIZE_MAX: usize = 16 << 20;
/// The deepest nesting of collections the reader takes.
pub const DEPTH_MAX: usize = 32;

/// Why a file is invalid, and where: the line and column, from 1, of
/// the first error.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Error {
    pub line: usize,
    pub column: usize,
    pub kind: ErrorKind,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ErrorKind {
    /// The file is larger than `SIZE_MAX`.
    TooLarge,
    /// The file holds a null byte.
    NullByte,
    /// The file is not UTF-8.
    NotUtf8,
    /// Collections nest deeper than `DEPTH_MAX`.
    TooDeep,
    /// An anchor, alias, tag, directive or explicit key.
    Unsupported,
    /// A second document.
    MultipleDocuments,
    /// The root is not a map.
    RootNotMap,
    /// A key is a collection.
    NonScalarKey,
    /// A key appears twice in one map.
    DuplicateKey,
    /// A line is indented where it may not be, or with a tab.
    Indentation,
    /// An escape sequence that a double-quoted scalar may not hold.
    BadEscape,
    /// A quoted scalar or flow collection that the file ends inside.
    Unterminated,
    /// Any other text that is not valid where it stands.
    Syntax,
    /// A value of another type than the one asked for.
    WrongType,
    /// An integer outside the range asked for.
    OutOfRange,
}

/// How a scalar was written.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Style {
    /// Plain, or an empty value.
    Plain,
    /// Single- or double-quoted.
    Quoted,
    /// Literal or folded.
    Block,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Value {
    Scalar(String, Style),
    List(Vec<Node>),
    Map(Vec<Entry>),
}

/// A value, with the line and column, from 1, where it starts.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Node {
    pub value: Value,
    pub line: usize,
    pub column: usize,
}

/// One key of a map and its value.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    pub key: String,
    pub line: usize,
    pub column: usize,
    pub value: Node,
}

/// Reads `bytes` as a YAML file whose root is a map.
pub fn parse(bytes: &[u8]) -> Result<Node, Error> {
    let at_start = |kind| Error {
        line: 1,
        column: 1,
        kind,
    };
    if bytes.len() > SIZE_MAX {
        return Err(at_start(ErrorKind::TooLarge));
    }
    let bytes = bytes.strip_prefix(b"\xef\xbb\xbf").unwrap_or(bytes);
    if let Some(at) = bytes.iter().position(|&b| b == 0) {
        return Err(error_at(bytes, at, ErrorKind::NullByte));
    }
    let text = match core::str::from_utf8(bytes) {
        Ok(text) => text,
        Err(e) => return Err(error_at(bytes, e.valid_up_to(), ErrorKind::NotUtf8)),
    };
    let text = text.replace("\r\n", "\n");
    parse::Parser::new(&text).document()
}

/// An error of `kind` at byte `at` of `bytes`.
fn error_at(bytes: &[u8], at: usize, kind: ErrorKind) -> Error {
    let before = &bytes[..at];
    let line_start = before
        .iter()
        .rposition(|&b| b == b'\n')
        .map_or(0, |i| i + 1);
    Error {
        line: before.iter().filter(|&&b| b == b'\n').count() + 1,
        column: at - line_start + 1,
        kind,
    }
}
