//! The line-level syntax of the entries file: indentation, `key: value`
//! pairs, list item markers, comments and scalar values.

use super::{Error, ErrorKind, error};
use alloc::string::{String, ToString};
use alloc::vec::Vec;

/// A line that holds more than whitespace and a comment.
#[derive(Debug)]
pub struct Line<'a> {
    pub number: usize,
    pub indent: usize,
    /// The line after its indentation.
    pub text: &'a str,
}

/// Splits `text` into its content lines, skipping blank and comment-only
/// lines.
pub fn content_lines(text: &str) -> Result<Vec<Line<'_>>, Error> {
    let mut lines = Vec::new();
    for (index, raw) in text.split('\n').enumerate() {
        let number = index + 1;
        let raw = raw.strip_suffix('\r').unwrap_or(raw);
        let rest = raw.trim_start_matches(' ');
        let indent = raw.len() - rest.len();
        if rest.starts_with('\t') {
            if rest.trim_start().is_empty() || rest.trim_start().starts_with('#') {
                continue;
            }
            return Err(error(number, ErrorKind::Tab));
        }
        if rest.trim_end().is_empty() || rest.starts_with('#') {
            continue;
        }
        lines.push(Line {
            number,
            indent,
            text: rest,
        });
    }
    Ok(lines)
}

/// One `key: value` pair. `value` is the unparsed text after the colon, or
/// `None` when only a comment or nothing follows it.
#[derive(Debug)]
pub struct Pair<'a> {
    pub key: &'a str,
    pub value: Option<&'a str>,
    pub line: usize,
}

/// Whether `text` starts a block list item.
pub fn is_item(text: &str) -> bool {
    text == "-" || text.starts_with("- ")
}

/// Splits `text` into a key and the value after `: `.
pub fn pair(text: &str, line: usize) -> Result<Pair<'_>, Error> {
    let syntax = |what| error(line, ErrorKind::Syntax(what));
    let split = text
        .char_indices()
        .find(|&(i, c)| c == ':' && text[i + 1..].chars().next().is_none_or(|n| n == ' '));
    let Some((colon, _)) = split else {
        return Err(syntax("expected `key: value`"));
    };
    let key = &text[..colon];
    let first = key.chars().next().unwrap_or(' ');
    if key.is_empty() || key.ends_with(' ') || "-?:,[]{}#&*!|>'\"%@`".contains(first) {
        return Err(syntax("invalid key"));
    }
    let rest = text[colon + 1..].trim_start_matches(is_blank);
    let value = (!rest.is_empty() && !rest.starts_with('#')).then_some(rest);
    Ok(Pair { key, value, line })
}

/// Whether `c` is a space or a tab, which a comment may follow.
fn is_blank(c: char) -> bool {
    c == ' ' || c == '\t'
}

/// `text` up to its comment, which starts at a `#` after a space or a
/// tab.
fn before_comment(text: &str) -> &str {
    let start = text
        .char_indices()
        .find(|&(i, c)| c == '#' && text[..i].ends_with(is_blank));
    start.map_or(text, |(i, _)| &text[..i])
}

/// Parses a value that starts at the beginning of `text`, which is
/// neither blank nor a comment.
pub fn value(text: &str, line: usize) -> Result<String, Error> {
    let syntax = |what| error(line, ErrorKind::Syntax(what));
    let (value, rest) = match text.as_bytes()[0] {
        b'"' => double_quoted(&text[1..]).ok_or_else(|| syntax("invalid double-quoted string"))?,
        b'\'' => single_quoted(&text[1..]).ok_or_else(|| syntax("unterminated quoted string"))?,
        _ => {
            let plain = before_comment(text).trim_end_matches(is_blank);
            let first = plain.as_bytes()[0];
            let indicator = b"[]{},&*!|>%@`".contains(&first)
                || (b"-?:".contains(&first) && plain[1..].starts_with(' '));
            if indicator || plain.contains(": ") || plain.ends_with(':') {
                return Err(syntax("invalid plain value"));
            }
            return Ok(plain.to_string());
        }
    };
    if !before_comment(rest).trim_start_matches(is_blank).is_empty() {
        return Err(syntax("unexpected text after quoted string"));
    }
    Ok(value)
}

/// Reads a double-quoted string after its opening quote. Returns the string
/// and the text after the closing quote.
fn double_quoted(text: &str) -> Option<(String, &str)> {
    let mut out = String::new();
    let mut chars = text.char_indices();
    while let Some((i, c)) = chars.next() {
        match c {
            '"' => return Some((out, &text[i + 1..])),
            '\\' => out.push(match chars.next()?.1 {
                'n' => '\n',
                't' => '\t',
                '"' => '"',
                '\\' => '\\',
                '/' => '/',
                _ => return None,
            }),
            c => out.push(c),
        }
    }
    None
}

/// Reads a single-quoted string after its opening quote.
fn single_quoted(text: &str) -> Option<(String, &str)> {
    let mut out = String::new();
    let mut rest = text;
    loop {
        let quote = rest.find('\'')?;
        out.push_str(&rest[..quote]);
        rest = &rest[quote + 1..];
        match rest.strip_prefix('\'') {
            Some(after) => {
                out.push('\'');
                rest = after;
            }
            None => return Some((out, rest)),
        }
    }
}

#[cfg(test)]
mod tests;
