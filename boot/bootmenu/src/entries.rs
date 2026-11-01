//! Reading `\EFI\Mikros\entries.yaml`.
//!
//! The parser reads lines of `key: value` pairs, with the `entries` key
//! holding a block list of maps. Lines nested under keys it does not know are
//! skipped without being parsed.

pub mod syntax;

use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core::fmt;
use syntax::{Line, Pair, content_lines, is_item, pair, value};

/// The contents of the entries file.
#[derive(Debug, PartialEq, Eq)]
pub struct Config {
    pub default: Option<String>,
    pub timeout: Option<u32>,
    pub entries: Vec<Entry>,
    pub detect: bool,
}

/// One boot entry.
#[derive(Debug, PartialEq, Eq)]
pub struct Entry {
    pub id: String,
    pub name: String,
    pub detail: Option<String>,
    pub color: Color,
    /// Path of the icon on the boot volume, with `\` separators and a leading
    /// `\`.
    pub icon: Option<String>,
    /// Path of the EFI image on the boot volume, with `\` separators and a
    /// leading `\`.
    pub path: String,
    pub args: Option<String>,
}

/// A tile color.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Color {
    pub r: u8,
    pub g: u8,
    pub b: u8,
}

impl Color {
    pub const DEFAULT_TILE: Color = Color {
        r: 0x4b,
        g: 0x55,
        b: 0x63,
    };
}

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

impl Config {
    /// The configuration used when there is no entries file. The caller sets
    /// `timeout` to 5 when it adds detected systems.
    pub fn for_missing_file() -> Config {
        Config {
            default: None,
            timeout: Some(0),
            entries: alloc::vec![Entry {
                id: "mikros".to_string(),
                name: "Mikros".to_string(),
                detail: None,
                color: Color::DEFAULT_TILE,
                icon: None,
                path: "\\EFI\\Mikros\\mikrosboot.efi".to_string(),
                args: None,
            }],
            detect: true,
        }
    }

    /// The index of the entry to focus when the menu opens.
    pub fn default_index(&self) -> usize {
        self.default
            .as_ref()
            .and_then(|id| self.entries.iter().position(|e| &e.id == id))
            .unwrap_or(0)
    }

    /// Parses and checks the contents of an entries file.
    pub fn parse(bytes: &[u8]) -> Result<Config, Error> {
        let bytes = bytes.strip_prefix(b"\xef\xbb\xbf").unwrap_or(bytes);
        let text = core::str::from_utf8(bytes).map_err(|_| Error {
            line: None,
            kind: ErrorKind::NotUtf8,
        })?;
        let lines = content_lines(text)?;
        Parser {
            lines: &lines,
            next: 0,
        }
        .config()
    }
}

fn error(line: usize, kind: ErrorKind) -> Error {
    Error {
        line: Some(line),
        kind,
    }
}

/// A scalar value and the line it is on.
struct Value {
    text: String,
    line: usize,
}

struct Parser<'a, 'b> {
    lines: &'b [Line<'a>],
    next: usize,
}

const ENTRY_KEYS: [&str; 7] = ["id", "name", "detail", "color", "icon", "path", "args"];

impl<'a> Parser<'a, '_> {
    fn peek(&self) -> Option<&Line<'a>> {
        self.lines.get(self.next)
    }

    /// Skips the lines nested under a key at column `indent`, including list
    /// items at the same column.
    fn skip_nested(&mut self, indent: usize) {
        while let Some(line) = self.peek() {
            let item = line.text == "-" || line.text.starts_with("- ");
            if line.indent > indent || (line.indent == indent && item) {
                self.next += 1;
            } else {
                break;
            }
        }
    }

    fn config(mut self) -> Result<Config, Error> {
        let mut config = Config {
            default: None,
            timeout: None,
            entries: Vec::new(),
            detect: true,
        };
        let mut default_line = 0;
        let mut seen: Vec<&str> = Vec::new();
        let top = self.peek().map_or(0, |line| line.indent);

        while let Some(line) = self.peek() {
            if line.indent != top {
                return Err(error(line.number, ErrorKind::Indentation));
            }
            let pair = pair(line.text, line.number)?;
            self.next += 1;
            if seen.contains(&pair.key) {
                return Err(error(
                    pair.line,
                    ErrorKind::DuplicateKey(pair.key.to_string()),
                ));
            }
            seen.push(pair.key);
            match pair.key {
                "entries" => {
                    if pair.value.is_some() {
                        return Err(error(pair.line, ErrorKind::ExpectedList));
                    }
                    config.entries = self.entries(top)?;
                }
                "default" => {
                    default_line = pair.line;
                    config.default = Some(scalar(pair)?.text);
                }
                "timeout" => {
                    let value = scalar(pair)?;
                    let digits =
                        !value.text.is_empty() && value.text.bytes().all(|b| b.is_ascii_digit());
                    let timeout = digits.then(|| value.text.parse().ok()).flatten();
                    let timeout =
                        timeout.ok_or_else(|| error(value.line, ErrorKind::InvalidTimeout))?;
                    config.timeout = Some(timeout);
                }
                "detect" => {
                    let value = scalar(pair)?;
                    config.detect = match value.text.as_str() {
                        "true" => true,
                        "false" => false,
                        _ => return Err(error(value.line, ErrorKind::InvalidDetect)),
                    };
                }
                _ => self.skip_nested(top),
            }
        }

        if let Some(id) = &config.default
            && !config.entries.iter().any(|e| &e.id == id)
        {
            return Err(error(default_line, ErrorKind::UnknownDefault(id.clone())));
        }
        Ok(config)
    }

    /// Reads the list items under the `entries` key at column `parent`.
    fn entries(&mut self, parent: usize) -> Result<Vec<Entry>, Error> {
        let mut entries: Vec<Entry> = Vec::new();
        let dash = match self.peek() {
            Some(line) if line.indent > parent || line.indent == parent && is_item(line.text) => {
                line.indent
            }
            _ => return Ok(entries),
        };

        while let Some(line) = self.peek() {
            if line.indent < dash || (line.indent == dash && !is_item(line.text)) {
                break;
            }
            if line.indent > dash {
                return Err(error(line.number, ErrorKind::Indentation));
            }
            let (entry, start) = self.entry(dash)?;
            if entries.iter().any(|e| e.id == entry.id) {
                return Err(error(start, ErrorKind::DuplicateId(entry.id)));
            }
            entries.push(entry);
        }
        Ok(entries)
    }

    /// Reads one list item whose `-` is at column `dash`, and returns it with
    /// the number of its first line.
    fn entry(&mut self, dash: usize) -> Result<(Entry, usize), Error> {
        let first = &self.lines[self.next];
        let start = first.number;
        let after_dash = &first.text[1..];
        let content = after_dash.trim_start_matches(' ');
        if content.is_empty() {
            return Err(error(start, ErrorKind::Syntax("list item has no key")));
        }
        let column = dash + 1 + (after_dash.len() - content.len());
        let mut pairs: Vec<Pair<'a>> = Vec::new();
        let mut text = content;
        let mut number = start;

        loop {
            let pair = pair(text, number)?;
            self.next += 1;
            if pairs.iter().any(|p| p.key == pair.key) {
                return Err(error(
                    pair.line,
                    ErrorKind::DuplicateKey(pair.key.to_string()),
                ));
            }
            if ENTRY_KEYS.contains(&pair.key) {
                if let Some(line) = self.peek()
                    && line.indent > column
                {
                    let key = pair.key.to_string();
                    return Err(error(line.number, ErrorKind::ExpectedValue(key)));
                }
            } else {
                self.skip_nested(column);
            }
            pairs.push(pair);

            match self.peek() {
                Some(line) if line.indent == column => {
                    text = line.text;
                    number = line.number;
                }
                Some(line) if line.indent > dash && line.indent != column => {
                    return Err(error(line.number, ErrorKind::Indentation));
                }
                _ => break,
            }
        }
        Ok((entry_from_pairs(pairs, start)?, start))
    }
}

fn entry_from_pairs(pairs: Vec<Pair<'_>>, start: usize) -> Result<Entry, Error> {
    let mut id = None;
    let mut name = None;
    let mut detail = None;
    let mut color = Color::DEFAULT_TILE;
    let mut icon = None;
    let mut path = None;
    let mut args = None;

    for pair in pairs {
        if !ENTRY_KEYS.contains(&pair.key) {
            continue;
        }
        let key = pair.key;
        let value = scalar(pair)?;
        match key {
            "id" => {
                let valid = value
                    .text
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_');
                if !valid {
                    return Err(error(value.line, ErrorKind::InvalidId));
                }
                id = Some(value.text);
            }
            "name" => name = Some(value.text),
            "detail" => detail = Some(value.text),
            "color" => {
                color = parse_color(&value.text)
                    .ok_or_else(|| error(value.line, ErrorKind::InvalidColor))?;
            }
            "icon" => icon = Some(normalize_path(&value.text)),
            "path" => path = Some(normalize_path(&value.text)),
            _ => args = Some(value.text),
        }
    }

    let missing = |key| error(start, ErrorKind::MissingKey(key));
    Ok(Entry {
        id: id.ok_or_else(|| missing("id"))?,
        name: name.ok_or_else(|| missing("name"))?,
        detail,
        color,
        icon,
        path: path.ok_or_else(|| missing("path"))?,
        args,
    })
}

/// The parsed value of `pair`, which must be present and not empty.
fn scalar(pair: Pair<'_>) -> Result<Value, Error> {
    let text = match pair.value {
        Some(raw) => value(raw, pair.line)?,
        None => String::new(),
    };
    if text.is_empty() {
        return Err(error(
            pair.line,
            ErrorKind::EmptyValue(pair.key.to_string()),
        ));
    }
    Ok(Value {
        text,
        line: pair.line,
    })
}

fn parse_color(text: &str) -> Option<Color> {
    let hex = text.strip_prefix('#')?;
    if hex.len() != 6 || !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    let byte = |i: usize| u8::from_str_radix(&hex[i..i + 2], 16).ok();
    Some(Color {
        r: byte(0)?,
        g: byte(2)?,
        b: byte(4)?,
    })
}

/// Converts a path to use `\` separators with one leading `\`.
pub fn normalize_path(path: &str) -> String {
    let mut out = String::from("\\");
    out.extend(
        path.trim_start_matches(['/', '\\'])
            .chars()
            .map(|c| if c == '/' { '\\' } else { c }),
    );
    out
}

#[cfg(test)]
mod tests;
