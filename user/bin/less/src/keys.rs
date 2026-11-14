//! The keys that less takes, from the bytes that the terminal sends for
//! them.

/// A key.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Key {
    Char(char),
    Enter,
    Backspace,
    Escape,
    Up,
    Down,
    PageUp,
    PageDown,
    Home,
    End,
}

/// The keys in `bytes`. An escape sequence of another key is left out,
/// and an ESC at the end of `bytes` is `Escape`.
pub fn decode(bytes: &[u8]) -> Vec<Key> {
    let mut keys = Vec::new();
    let mut at = 0;
    while at < bytes.len() {
        let rest = &bytes[at..];
        let (key, len) = match rest[0] {
            b'\r' | b'\n' => (Some(Key::Enter), 1),
            0x7f | 0x08 => (Some(Key::Backspace), 1),
            0x1b => escape(rest),
            first => {
                let len = utf8_len(first).min(rest.len());
                let text = std::str::from_utf8(&rest[..len]).ok();
                (text.and_then(|t| t.chars().next()).map(Key::Char), len)
            }
        };
        keys.extend(key);
        at += len;
    }
    keys
}

/// The length of the UTF-8 sequence that starts with `first`.
fn utf8_len(first: u8) -> usize {
    match first {
        0xf0.. => 4,
        0xe0.. => 3,
        0xc0.. => 2,
        _ => 1,
    }
}

/// The key of the escape sequence at the start of `bytes`, and its
/// length.
fn escape(bytes: &[u8]) -> (Option<Key>, usize) {
    match bytes.get(1) {
        Some(b'[') => {
            let Some(end) = bytes[2..].iter().position(|b| (0x40..=0x7e).contains(b)) else {
                return (None, bytes.len());
            };
            let key = match &bytes[2..=2 + end] {
                b"A" => Some(Key::Up),
                b"B" => Some(Key::Down),
                b"H" | b"1~" | b"7~" => Some(Key::Home),
                b"F" | b"4~" | b"8~" => Some(Key::End),
                b"5~" => Some(Key::PageUp),
                b"6~" => Some(Key::PageDown),
                _ => None,
            };
            (key, 3 + end)
        }
        Some(b'O') => {
            let key = match bytes.get(2) {
                Some(b'A') => Some(Key::Up),
                Some(b'B') => Some(Key::Down),
                Some(b'H') => Some(Key::Home),
                Some(b'F') => Some(Key::End),
                _ => None,
            };
            (key, bytes.len().min(3))
        }
        _ => (Some(Key::Escape), 1),
    }
}

#[cfg(test)]
mod tests {
    use super::Key::*;
    use super::*;

    #[test]
    fn reads_characters_and_control_keys() {
        assert_eq!(decode(b"q"), [Char('q')]);
        assert_eq!(
            decode(" G/ä".as_bytes()),
            [Char(' '), Char('G'), Char('/'), Char('ä')]
        );
        assert_eq!(
            decode(b"\r\n\x7f\x08"),
            [Enter, Enter, Backspace, Backspace]
        );
        assert_eq!(decode(b"\x1b"), [Escape]);
        assert_eq!(decode(b"\x1bq"), [Escape, Char('q')]);
    }

    #[test]
    fn reads_the_sequences_of_keys() {
        assert_eq!(decode(b"\x1b[A\x1b[B\x1bOA\x1bOB"), [Up, Down, Up, Down]);
        assert_eq!(decode(b"\x1b[5~\x1b[6~"), [PageUp, PageDown]);
        assert_eq!(decode(b"\x1b[H\x1b[1~\x1bOH"), [Home, Home, Home]);
        assert_eq!(decode(b"\x1b[F\x1b[4~\x1bOF"), [End, End, End]);
    }

    #[test]
    fn leaves_out_other_sequences() {
        assert_eq!(decode(b"\x1b[C\x1b[1;5Dq"), [Char('q')]);
        assert_eq!(decode(b"\x1b[2"), []);
    }
}
