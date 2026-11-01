//! Reading `\EFI\Mikros\entries.yaml`.

pub mod syntax;

use alloc::string::String;
use core::fmt;

/// A problem with the entries file.
#[derive(Debug, PartialEq, Eq)]
pub struct Error {
    /// The 1-based line number, for problems at a place in the file.
    pub line: Option<usize>,
    pub kind: ErrorKind,
}

#[derive(Debug, PartialEq, Eq)]
pub enum ErrorKind {
    NotUtf8,
    Tab,
    Indentation,
    Syntax(&'static str),
    DuplicateKey(String),
    EmptyValue(String),
    ExpectedList,
    ExpectedValue(String),
    MissingKey(&'static str),
    InvalidId,
    DuplicateId(String),
    InvalidColor,
    InvalidTimeout,
    InvalidDetect,
    UnknownDefault(String),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if let Some(line) = self.line {
            write!(f, "line {line}: ")?;
        }
        match &self.kind {
            ErrorKind::NotUtf8 => write!(f, "the file is not valid UTF-8"),
            ErrorKind::Tab => write!(f, "tab in indentation"),
            ErrorKind::Indentation => write!(f, "unexpected indentation"),
            ErrorKind::Syntax(what) => write!(f, "{what}"),
            ErrorKind::DuplicateKey(key) => write!(f, "`{key}` appears twice"),
            ErrorKind::EmptyValue(key) => write!(f, "`{key}` is empty"),
            ErrorKind::ExpectedList => write!(f, "`entries` must hold a list"),
            ErrorKind::ExpectedValue(key) => write!(f, "`{key}` must hold a single value"),
            ErrorKind::MissingKey(key) => write!(f, "entry has no `{key}`"),
            ErrorKind::InvalidId => {
                write!(f, "`id` may only hold ASCII letters, digits, `-` and `_`")
            }
            ErrorKind::DuplicateId(id) => write!(f, "entry ID `{id}` is used twice"),
            ErrorKind::InvalidColor => write!(f, "`color` must have the form \"#rrggbb\""),
            ErrorKind::InvalidTimeout => write!(f, "`timeout` must be a whole number"),
            ErrorKind::InvalidDetect => write!(f, "`detect` must be `true` or `false`"),
            ErrorKind::UnknownDefault(id) => write!(f, "`default` names no entry `{id}`"),
        }
    }
}

fn error(line: usize, kind: ErrorKind) -> Error {
    Error {
        line: Some(line),
        kind,
    }
}
