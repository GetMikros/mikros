//! How printf reads the values of its arguments: integers and
//! floating-point numbers as C's `strtol` and `strtod` read them, or the
//! code of a quoted character, its escapes, and `%q`'s quoting for the
//! shell.

/// The integer that `argument` gives: in decimal, in hexadecimal after
/// `0x`, in octal after `0`, or the code of the character after a quote,
/// with a message when it is not wholly a number.
pub fn integer(argument: &str) -> (i128, Option<String>) {
    let text = argument.trim_start();
    if let Some(rest) = text.strip_prefix(['\'', '"']) {
        return (rest.chars().next().map_or(0, |c| c as i128), None);
    }
    let (negative, unsigned) = match text.as_bytes().first() {
        Some(b'-') => (true, &text[1..]),
        Some(b'+') => (false, &text[1..]),
        _ => (false, text),
    };
    let bytes = unsigned.as_bytes();
    let (base, start) = match bytes {
        [b'0', b'x' | b'X', c, ..] if c.is_ascii_hexdigit() => (16, 2),
        [b'0', ..] => (8, 0),
        _ => (10, 0),
    };
    let length = bytes[start..]
        .iter()
        .take_while(|c| (**c as char).is_digit(base))
        .count();
    if length == 0 {
        return (0, Some(format!("'{argument}': expected a numeric value")));
    }
    let mut value: i128 = 0;
    for &c in &bytes[start..start + length] {
        let digit = (c as char).to_digit(base).unwrap_or(0);
        value = value
            .saturating_mul(base.into())
            .saturating_add(digit.into());
    }
    if negative {
        value = -value;
    }
    match start + length == bytes.len() {
        true => (value, None),
        false => (
            value,
            Some(format!("'{argument}': value not completely converted")),
        ),
    }
}

/// The floating-point number that `argument` gives, or the code of the
/// character after a quote, with a message when it is not wholly a
/// number.
pub fn float(argument: &str) -> (f64, Option<String>) {
    let text = argument.trim_start();
    if let Some(rest) = text.strip_prefix(['\'', '"']) {
        return (
            rest.chars().next().map_or(0.0, |c| f64::from(c as u32)),
            None,
        );
    }
    let length = number_length(text.as_bytes());
    let value = text[..length].parse::<f64>().ok();
    match (value, length == text.len()) {
        (None, _) => (0.0, Some(format!("'{argument}': expected a numeric value"))),
        (Some(value), true) => (value, None),
        (Some(value), false) => (
            value,
            Some(format!("'{argument}': value not completely converted")),
        ),
    }
}

/// The length of the floating-point number that starts `text`.
fn number_length(text: &[u8]) -> usize {
    let mut i = usize::from(matches!(text.first(), Some(b'+' | b'-')));
    let lower = text[i..].to_ascii_lowercase();
    for word in [&b"infinity"[..], b"inf", b"nan"] {
        if lower.starts_with(word) {
            return i + word.len();
        }
    }
    let count =
        |text: &[u8], at: usize| text[at..].iter().take_while(|c| c.is_ascii_digit()).count();
    let whole = count(text, i);
    i += whole;
    let mut fraction = 0;
    if text.get(i) == Some(&b'.') {
        fraction = count(text, i + 1);
        if whole + fraction > 0 {
            i += 1 + fraction;
        }
    }
    if whole + fraction == 0 {
        return 0;
    }
    if matches!(text.get(i), Some(b'e' | b'E')) {
        let mut at = i + 1;
        at += usize::from(matches!(text.get(at), Some(b'+' | b'-')));
        let exponent = count(text, at);
        if exponent > 0 {
            i = at + exponent;
        }
    }
    i
}

/// Writes the escape that starts `text`, a backslash, onto `out`, and
/// returns its length and whether it is `\c`. In the escapes of `%b`'s
/// argument, `in_argument`, an octal number may start with a 0 that
/// does not count among its three digits.
pub fn escape(text: &[u8], in_argument: bool, out: &mut Vec<u8>) -> Result<(usize, bool), String> {
    let Some(&c) = text.get(1) else {
        out.push(b'\\');
        return Ok((1, false));
    };
    let simple = match c {
        b'\\' => Some(b'\\'),
        b'a' => Some(0x07),
        b'b' => Some(0x08),
        b'e' => Some(0x1b),
        b'f' => Some(0x0c),
        b'n' => Some(b'\n'),
        b'r' => Some(b'\r'),
        b't' => Some(b'\t'),
        b'v' => Some(0x0b),
        b'"' => Some(b'"'),
        b'\'' => Some(b'\''),
        _ => None,
    };
    if let Some(byte) = simple {
        out.push(byte);
        return Ok((2, false));
    }
    match c {
        b'c' => Ok((2, true)),
        b'x' => {
            let length = text[2..]
                .iter()
                .take(2)
                .take_while(|c| c.is_ascii_hexdigit())
                .count();
            if length == 0 {
                return Err("missing hexadecimal number in escape".into());
            }
            out.push(value(&text[2..2 + length], 16) as u8);
            Ok((2 + length, false))
        }
        b'u' | b'U' => {
            let wanted = if c == b'u' { 4 } else { 8 };
            let hex = text
                .get(2..2 + wanted)
                .filter(|h| h.iter().all(u8::is_ascii_hexdigit));
            let Some(hex) = hex else {
                return Err("missing hexadecimal number in escape".into());
            };
            let Some(character) = char::from_u32(value(hex, 16)) else {
                let shown = String::from_utf8_lossy(&text[..2 + wanted]);
                return Err(format!("invalid universal character name {shown}"));
            };
            out.extend_from_slice(character.encode_utf8(&mut [0; 4]).as_bytes());
            Ok((2 + wanted, false))
        }
        b'0'..=b'7' => {
            let skip = usize::from(in_argument && c == b'0');
            let start = 1 + skip;
            let length = text[start..]
                .iter()
                .take(3)
                .take_while(|c| (b'0'..=b'7').contains(*c))
                .count();
            out.push(value(&text[start..start + length], 8) as u8);
            Ok((start + length, false))
        }
        _ => {
            out.extend_from_slice(&[b'\\', c]);
            Ok((2, false))
        }
    }
}

fn value(digits: &[u8], base: u32) -> u32 {
    digits.iter().fold(0, |v, &d| {
        v.wrapping_mul(base)
            .wrapping_add((d as char).to_digit(base).unwrap_or(0))
    })
}

/// Writes `argument` with its escapes onto `out`, as `%b` does, and
/// returns whether it met `\c`, which ends the output.
pub fn escaped(argument: &[u8], out: &mut Vec<u8>) -> Result<bool, String> {
    let mut i = 0;
    while i < argument.len() {
        if argument[i] != b'\\' {
            out.push(argument[i]);
            i += 1;
            continue;
        }
        let (length, stop) = escape(&argument[i..], true, out)?;
        if stop {
            return Ok(true);
        }
        i += length;
    }
    Ok(false)
}

/// Whether the shell reads `c` as itself where it is, at the start of
/// a word when `first`.
fn plain(c: char, first: bool) -> bool {
    match c {
        '#' | '~' => !first,
        c if c.is_control() => false,
        c => !" \t\n!\"$&'()*;<=>?[\\]^`{|}".contains(c),
    }
}

/// `text` quoted for the shell, as `%q` writes it: as it is when the
/// shell reads it so, in quotes otherwise, and with control characters
/// as `$'\n'`.
pub fn quote(text: &str) -> String {
    if text.is_empty() {
        return "''".into();
    }
    if text.chars().enumerate().all(|(i, c)| plain(c, i == 0)) {
        return text.into();
    }
    let mut out = String::new();
    let mut run = String::new();
    let mut controls = String::new();
    let flush_run = |run: &mut String, out: &mut String| {
        if run.is_empty() {
            return;
        }
        let needs = run
            .chars()
            .enumerate()
            .any(|(i, c)| !plain(c, i == 0 && out.is_empty()));
        if !needs {
            out.push_str(run);
        } else if run.contains('\'') && !run.contains(['"', '$', '`', '\\', '!']) {
            out.push('"');
            out.push_str(run);
            out.push('"');
        } else {
            out.push('\'');
            out.push_str(&run.replace('\'', "'\\''"));
            out.push('\'');
        }
        run.clear();
    };
    let flush_controls = |controls: &mut String, out: &mut String| {
        if !controls.is_empty() {
            out.push_str("$'");
            out.push_str(controls);
            out.push('\'');
            controls.clear();
        }
    };
    for c in text.chars() {
        if c.is_control() {
            flush_run(&mut run, &mut out);
            let escaped = match c {
                '\x07' => "\\a".to_string(),
                '\x08' => "\\b".into(),
                '\t' => "\\t".into(),
                '\n' => "\\n".into(),
                '\x0b' => "\\v".into(),
                '\x0c' => "\\f".into(),
                '\r' => "\\r".into(),
                '\x1b' => "\\E".into(),
                c if (c as u32) < 0x100 => format!("\\{:03o}", c as u32),
                c => format!("\\u{:04X}", c as u32),
            };
            controls.push_str(&escaped);
        } else {
            flush_controls(&mut controls, &mut out);
            run.push(c);
        }
    }
    flush_run(&mut run, &mut out);
    flush_controls(&mut controls, &mut out);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_integers() {
        assert_eq!(integer("42"), (42, None));
        assert_eq!(integer(" -0x1f"), (-31, None));
        assert_eq!(integer("010"), (8, None));
        assert_eq!(integer("'A"), (65, None));
        assert_eq!(integer("\"é"), (233, None));
        let (value, error) = integer("12x");
        assert_eq!(value, 12);
        assert_eq!(
            error.as_deref(),
            Some("'12x': value not completely converted")
        );
        let (value, error) = integer("abc");
        assert_eq!(value, 0);
        assert_eq!(error.as_deref(), Some("'abc': expected a numeric value"));
    }

    #[test]
    fn reads_floats() {
        assert_eq!(float("1.5"), (1.5, None));
        assert_eq!(float("-.5e1"), (-5.0, None));
        assert_eq!(float("inf").0, f64::INFINITY);
        assert_eq!(float("'a"), (97.0, None));
        assert_eq!(float("2e").0, 2.0);
        assert!(float("2e").1.is_some());
        assert!(float("x").1.is_some());
    }

    #[test]
    fn reads_escapes() {
        let read = |text: &[u8], in_argument| {
            let mut out = Vec::new();
            let read = escape(text, in_argument, &mut out).unwrap();
            (out, read)
        };
        assert_eq!(read(b"\\n", false), (b"\n".to_vec(), (2, false)));
        assert_eq!(read(b"\\\\", false), (b"\\".to_vec(), (2, false)));
        assert_eq!(read(b"\\101x", false), (b"A".to_vec(), (4, false)));
        assert_eq!(read(b"\\0101", true), (b"A".to_vec(), (5, false)));
        assert_eq!(read(b"\\0101", false), (b"\x08".to_vec(), (4, false)));
        assert_eq!(read(b"\\x41g", false), (b"A".to_vec(), (4, false)));
        assert_eq!(read(b"\\c", false), (Vec::new(), (2, true)));
        assert_eq!(read(b"\\z", false), (b"\\z".to_vec(), (2, false)));
        let mut out = Vec::new();
        assert!(escape(b"\\u12", false, &mut out).is_err());
        assert!(escape(b"\\ud800", false, &mut out).is_err());
        assert_eq!(escaped(b"a\\tb\\cc", &mut out), Ok(true));
        assert_eq!(out, b"a\tb");
    }

    #[test]
    fn quotes_for_the_shell() {
        assert_eq!(quote("abc-1.txt"), "abc-1.txt");
        assert_eq!(quote(""), "''");
        assert_eq!(quote("a b"), "'a b'");
        assert_eq!(quote("it's"), "\"it's\"");
        assert_eq!(quote("it's $x"), "'it'\\''s $x'");
        assert_eq!(quote("a\nb"), "a$'\\n'b");
        assert_eq!(quote("~x"), "'~x'");
        assert_eq!(quote("x~"), "x~");
    }
}
