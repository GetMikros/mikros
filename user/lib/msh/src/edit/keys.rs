//! The keys that the bytes a terminal sends stand for, as the terminfo
//! entry `xterm-256color` describes them: text, control characters,
//! Alt as an escape before a key, and the escape sequences of the
//! cursor and editing keys, with or without xterm's modifiers.

/// A key, with the modifiers that the line editor tells apart.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Key {
    Char(char),
    /// A control character, by its letter: `Ctrl('a')` for 0x01.
    Ctrl(char),
    /// A character typed with Alt.
    Alt(char),
    Enter,
    Tab,
    /// Shift-Tab.
    BackTab,
    Backspace,
    /// Alt-Backspace.
    AltBackspace,
    Delete,
    Escape,
    Up,
    Down,
    Left,
    Right,
    /// The cursor keys with Ctrl or Alt.
    WordLeft,
    WordRight,
    Home,
    End,
    /// A sequence that names no key the editor uses.
    Unknown,
}

/// Turns bytes into keys, keeping an incomplete escape sequence or
/// UTF-8 character until the rest arrives.
#[derive(Default)]
pub struct Decoder {
    pending: Vec<u8>,
}

impl Decoder {
    pub fn new() -> Decoder {
        Decoder::default()
    }

    /// The keys that `bytes`, after what was pending, complete.
    pub fn feed(&mut self, bytes: &[u8]) -> Vec<Key> {
        self.pending.extend_from_slice(bytes);
        let mut keys = Vec::new();
        let mut at = 0;
        while at < self.pending.len() {
            match decode(&self.pending[at..]) {
                Some((key, used)) => {
                    keys.push(key);
                    at += used;
                }
                None => break,
            }
        }
        self.pending.drain(..at);
        keys
    }

    /// Whether bytes wait for more, as a lone escape does.
    pub fn waiting(&self) -> bool {
        !self.pending.is_empty()
    }

    /// The keys of what waits, once no more bytes came: an escape that
    /// no sequence followed is the Escape key.
    pub fn flush(&mut self) -> Vec<Key> {
        let pending = std::mem::take(&mut self.pending);
        match pending.split_first() {
            None => Vec::new(),
            Some((0x1B, rest)) => {
                let mut keys = vec![Key::Escape];
                keys.extend(self.feed(rest));
                keys.extend(self.flush());
                keys
            }
            // A cut UTF-8 character.
            Some(_) => vec![Key::Unknown],
        }
    }
}

/// The first key of `bytes` and how many bytes it takes, or `None` when
/// they end inside it.
fn decode(bytes: &[u8]) -> Option<(Key, usize)> {
    let first = *bytes.first()?;
    let key = match first {
        b'\r' | b'\n' => Key::Enter,
        b'\t' => Key::Tab,
        0x7F | 0x08 => Key::Backspace,
        0x1B => return escape(bytes),
        0x00 => Key::Ctrl('@'),
        0x01..=0x1A => Key::Ctrl(char::from(b'a' + first - 1)),
        0x1C..=0x1F => Key::Unknown,
        0x20..=0x7E => Key::Char(char::from(first)),
        _ => return utf8(bytes),
    };
    Some((key, 1))
}

/// The character that starts `bytes`, a multi-byte UTF-8 sequence.
fn utf8(bytes: &[u8]) -> Option<(Key, usize)> {
    let len = match bytes[0] {
        0xC0..=0xDF => 2,
        0xE0..=0xEF => 3,
        0xF0..=0xF7 => 4,
        _ => return Some((Key::Unknown, 1)),
    };
    if bytes.len() < len {
        return None;
    }
    match std::str::from_utf8(&bytes[..len]) {
        Ok(s) => Some((Key::Char(s.chars().next()?), len)),
        Err(_) => Some((Key::Unknown, 1)),
    }
}

/// The key of an escape sequence, `bytes` starting with ESC.
fn escape(bytes: &[u8]) -> Option<(Key, usize)> {
    let second = *bytes.get(1)?;
    match second {
        b'[' => csi(bytes),
        b'O' => {
            let last = *bytes.get(2)?;
            Some((cursor(last, 1), 3))
        }
        0x7F | 0x08 => Some((Key::AltBackspace, 2)),
        0x20..=0x7E => Some((Key::Alt(char::from(second)), 2)),
        // ESC ESC: the first is Escape.
        _ => Some((Key::Escape, 1)),
    }
}

/// The key of `ESC [ params final`.
fn csi(bytes: &[u8]) -> Option<(Key, usize)> {
    let end = bytes[2..].iter().position(|b| (0x40..=0x7E).contains(b))? + 2;
    let params: Vec<u32> = std::str::from_utf8(&bytes[2..end])
        .unwrap_or("")
        .split(';')
        .map(|p| p.parse().unwrap_or(1))
        .collect();
    let modifier = params.get(1).copied().unwrap_or(1);
    let key = match bytes[end] {
        b'Z' => Key::BackTab,
        b'~' => match params.first() {
            Some(1 | 7) => Key::Home,
            Some(4 | 8) => Key::End,
            Some(3) => Key::Delete,
            _ => Key::Unknown,
        },
        last => cursor(last, modifier),
    };
    Some((key, end + 1))
}

/// The cursor key with final byte `last` and xterm's modifier parameter
/// `modifier`: 1 plus Shift 1, Alt 2 and Ctrl 4.
fn cursor(last: u8, modifier: u32) -> Key {
    let word = modifier > 1 && (modifier - 1) & 6 != 0;
    match (last, word) {
        (b'A', _) => Key::Up,
        (b'B', _) => Key::Down,
        (b'C', false) => Key::Right,
        (b'D', false) => Key::Left,
        (b'C', true) => Key::WordRight,
        (b'D', true) => Key::WordLeft,
        (b'H', _) => Key::Home,
        (b'F', _) => Key::End,
        _ => Key::Unknown,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn keys(bytes: &[u8]) -> Vec<Key> {
        Decoder::new().feed(bytes)
    }

    #[test]
    fn reads_text_and_control_characters() {
        assert_eq!(
            keys(b"a\x01\r\t\x7f\x08\x17"),
            [
                Key::Char('a'),
                Key::Ctrl('a'),
                Key::Enter,
                Key::Tab,
                Key::Backspace,
                Key::Backspace,
                Key::Ctrl('w'),
            ]
        );
        assert_eq!(keys("é€".as_bytes()), [Key::Char('é'), Key::Char('€')]);
    }

    #[test]
    fn reads_cursor_and_editing_keys() {
        assert_eq!(
            keys(b"\x1b[A\x1b[B\x1b[C\x1b[D\x1bOH\x1bOF\x1b[H\x1b[F"),
            [
                Key::Up,
                Key::Down,
                Key::Right,
                Key::Left,
                Key::Home,
                Key::End,
                Key::Home,
                Key::End,
            ]
        );
        assert_eq!(
            keys(b"\x1b[3~\x1b[1~\x1b[4~\x1b[Z\x1b[1;5D\x1b[1;3C\x1b[1;2D\x1b[5~"),
            [
                Key::Delete,
                Key::Home,
                Key::End,
                Key::BackTab,
                Key::WordLeft,
                Key::WordRight,
                Key::Left,
                Key::Unknown,
            ]
        );
        assert_eq!(
            keys(b"\x1bb\x1bf\x1b\x7f"),
            [Key::Alt('b'), Key::Alt('f'), Key::AltBackspace]
        );
    }

    #[test]
    fn waits_for_the_rest_of_a_sequence() {
        let mut d = Decoder::new();
        assert_eq!(d.feed(b"x\x1b["), [Key::Char('x')]);
        assert!(d.waiting());
        assert_eq!(d.feed(b"1;5"), []);
        assert_eq!(d.feed(b"C"), [Key::WordRight]);
        assert_eq!(d.feed(&"€".as_bytes()[..2]), []);
        assert_eq!(d.feed(&"€".as_bytes()[2..]), [Key::Char('€')]);
        // A lone escape is the Escape key once nothing follows it.
        assert_eq!(d.feed(b"\x1b"), []);
        assert_eq!(d.flush(), [Key::Escape]);
        assert!(!d.waiting());
    }
}
