//! The bytes that keys send to the program, as the terminfo entry
//! `xterm-256color` describes them, and pasted text.

use crate::emulator::screen::Modes;
use alloc::format;
use alloc::vec::Vec;

/// A key that is not text, or a character.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Key {
    Char(char),
    Enter,
    Tab,
    Backspace,
    Escape,
    Up,
    Down,
    Right,
    Left,
    Home,
    End,
    Insert,
    Delete,
    PageUp,
    PageDown,
    /// F1 to F12.
    Function(u8),
}

/// The modifiers held with a key.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Modifiers {
    pub shift: bool,
    pub alt: bool,
    pub ctrl: bool,
}

impl Modifiers {
    /// xterm's modifier parameter: 1 plus Shift 1, Alt 2 and Ctrl 4.
    fn parameter(self) -> u8 {
        1 + u8::from(self.shift) + 2 * u8::from(self.alt) + 4 * u8::from(self.ctrl)
    }

    fn any(self) -> bool {
        self.shift || self.alt || self.ctrl
    }
}

/// What `key` with `mods` sends while the screen has `modes`.
pub fn encode(key: Key, mods: Modifiers, modes: &Modes) -> Vec<u8> {
    let m = mods.parameter();
    // The cursor keys' final bytes, which application mode sends after
    // ESC O and modifiers after CSI 1;m.
    let cursor = |last: u8| -> Vec<u8> {
        match (mods.any(), modes.app_cursor) {
            (true, _) => format!("\x1b[1;{m}{}", char::from(last)).into_bytes(),
            (false, true) => alloc::vec![0x1B, b'O', last],
            (false, false) => alloc::vec![0x1B, b'[', last],
        }
    };
    // Keys that send CSI n ~.
    let tilde = |n: u8| -> Vec<u8> {
        match mods.any() {
            true => format!("\x1b[{n};{m}~").into_bytes(),
            false => format!("\x1b[{n}~").into_bytes(),
        }
    };
    let alt = |mut bytes: Vec<u8>| {
        if mods.alt {
            bytes.insert(0, 0x1B);
        }
        bytes
    };
    match key {
        Key::Char(c) => alt(character(c, mods.ctrl)),
        Key::Enter if modes.newline => alt(b"\r\n".to_vec()),
        Key::Enter => alt(b"\r".to_vec()),
        Key::Tab if mods.shift => b"\x1b[Z".to_vec(),
        Key::Tab => alt(b"\t".to_vec()),
        Key::Backspace if mods.ctrl => alt(alloc::vec![0x08]),
        Key::Backspace => alt(alloc::vec![0x7F]),
        Key::Escape => alt(alloc::vec![0x1B]),
        Key::Up => cursor(b'A'),
        Key::Down => cursor(b'B'),
        Key::Right => cursor(b'C'),
        Key::Left => cursor(b'D'),
        Key::Home => cursor(b'H'),
        Key::End => cursor(b'F'),
        Key::Insert => tilde(2),
        Key::Delete => tilde(3),
        Key::PageUp => tilde(5),
        Key::PageDown => tilde(6),
        Key::Function(n @ 1..=4) => {
            let last = b'P' + (n - 1);
            match mods.any() {
                true => format!("\x1b[1;{m}{}", char::from(last)).into_bytes(),
                false => alloc::vec![0x1B, b'O', last],
            }
        }
        Key::Function(n @ 5..=12) => tilde([15, 17, 18, 19, 20, 21, 23, 24][usize::from(n - 5)]),
        Key::Function(_) => Vec::new(),
    }
}

/// The bytes of character `c`: with Ctrl, the control character of an
/// ASCII letter or symbol that has one.
fn character(c: char, ctrl: bool) -> Vec<u8> {
    if ctrl {
        match c {
            'a'..='z' | 'A'..='Z' => return alloc::vec![(c as u8) & 0x1F],
            '@' | ' ' | '2' => return alloc::vec![0],
            '[' | '3' => return alloc::vec![0x1B],
            '\\' | '4' => return alloc::vec![0x1C],
            ']' | '5' => return alloc::vec![0x1D],
            '^' | '6' => return alloc::vec![0x1E],
            '_' | '/' | '7' => return alloc::vec![0x1F],
            '?' | '8' => return alloc::vec![0x7F],
            _ => {}
        }
    }
    let mut buffer = [0u8; 4];
    c.encode_utf8(&mut buffer).as_bytes().to_vec()
}

/// What pasting `text` sends: bracketed when the program asked for it.
/// Nothing in the console pastes yet.
#[allow(dead_code)]
pub fn paste(text: &[u8], modes: &Modes) -> Vec<u8> {
    match modes.bracketed_paste {
        true => [b"\x1b[200~", text, b"\x1b[201~"].concat(),
        false => text.to_vec(),
    }
}
