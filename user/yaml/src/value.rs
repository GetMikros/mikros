//! Reading typed values from nodes.

use crate::{Entry, Error, ErrorKind, Node, Style, Value};

impl Node {
    fn error(&self, kind: ErrorKind) -> Error {
        Error {
            line: self.line,
            column: self.column,
            kind,
        }
    }

    /// Whether the value counts as absent: the plain scalars `null`
    /// and `~`, or nothing.
    pub fn is_absent(&self) -> bool {
        matches!(&self.value, Value::Scalar(text, Style::Plain) if matches!(text.as_str(), "" | "null" | "~"))
    }

    /// The text of any scalar.
    pub fn string(&self) -> Result<&str, Error> {
        match &self.value {
            Value::Scalar(text, _) => Ok(text),
            _ => Err(self.error(ErrorKind::WrongType)),
        }
    }

    /// The plain scalar `true` or `false`.
    pub fn boolean(&self) -> Result<bool, Error> {
        match &self.value {
            Value::Scalar(text, Style::Plain) if text == "true" => Ok(true),
            Value::Scalar(text, Style::Plain) if text == "false" => Ok(false),
            _ => Err(self.error(ErrorKind::WrongType)),
        }
    }

    /// A plain scalar of decimal digits with an optional leading `-`,
    /// or `0x` and hexadecimal digits, from `min` to `max`.
    pub fn integer(&self, min: i64, max: i64) -> Result<i64, Error> {
        let Value::Scalar(text, Style::Plain) = &self.value else {
            return Err(self.error(ErrorKind::WrongType));
        };
        let digits = |s: &str, radix: u32| !s.is_empty() && s.chars().all(|c| c.is_digit(radix));
        let value = if let Some(hex) = text.strip_prefix("0x") {
            if !digits(hex, 16) {
                return Err(self.error(ErrorKind::WrongType));
            }
            i64::from_str_radix(hex, 16).ok()
        } else {
            if !digits(text.strip_prefix('-').unwrap_or(text), 10) {
                return Err(self.error(ErrorKind::WrongType));
            }
            text.parse().ok()
        };
        match value {
            Some(value) if (min..=max).contains(&value) => Ok(value),
            _ => Err(self.error(ErrorKind::OutOfRange)),
        }
    }

    /// The items of a sequence.
    pub fn list(&self) -> Result<&[Node], Error> {
        match &self.value {
            Value::List(items) => Ok(items),
            _ => Err(self.error(ErrorKind::WrongType)),
        }
    }

    /// The entries of a mapping.
    pub fn map(&self) -> Result<Map<'_>, Error> {
        match &self.value {
            Value::Map(entries) => Ok(Map(entries)),
            _ => Err(self.error(ErrorKind::WrongType)),
        }
    }
}

/// The entries of a mapping.
#[derive(Clone, Copy, Debug)]
pub struct Map<'a>(pub &'a [Entry]);

impl<'a> Map<'a> {
    /// The value of `key`, unless it is missing or absent.
    pub fn get(&self, key: &str) -> Option<&'a Node> {
        self.0
            .iter()
            .find(|e| e.key == key)
            .map(|e| &e.value)
            .filter(|v| !v.is_absent())
    }

    /// The entries whose key is not among `known`, for logging.
    pub fn unknown<'k>(&self, known: &'k [&'k str]) -> impl Iterator<Item = &'a Entry> + 'k
    where
        'a: 'k,
    {
        self.0.iter().filter(|e| !known.contains(&e.key.as_str()))
    }
}
