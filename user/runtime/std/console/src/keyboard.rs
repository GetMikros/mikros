//! Keyboards: evdev key events through the xkb keymap into the bytes
//! that the terminal gets, and key repeat, which the console makes since
//! drivers do not.

use crate::xkb::Keymap;
use mikros_vt::keys::{Key, Modifiers, encode};
use mikros_vt::screen::Modes;

/// How long a key is held before it repeats, and how often it then does,
/// in nanoseconds of the monotonic clock.
pub const REPEAT_DELAY: i64 = 500_000_000;
pub const REPEAT_INTERVAL: i64 = 33_000_000;

/// The xkb keysyms of the keys that send sequences rather than text.
fn special(keysym: u32) -> Option<Key> {
    Some(match keysym {
        0xFF0D | 0xFF8D => Key::Enter,
        0xFF09 | 0xFE20 => Key::Tab,
        0xFF08 => Key::Backspace,
        0xFF1B => Key::Escape,
        0xFF52 => Key::Up,
        0xFF54 => Key::Down,
        0xFF53 => Key::Right,
        0xFF51 => Key::Left,
        0xFF50 => Key::Home,
        0xFF57 => Key::End,
        0xFF63 => Key::Insert,
        0xFFFF => Key::Delete,
        0xFF55 => Key::PageUp,
        0xFF56 => Key::PageDown,
        0xFFBE..=0xFFC9 => Key::Function((keysym - 0xFFBE + 1) as u8),
        _ => return None,
    })
}

/// The keysym of F12, which with Ctrl and Alt shows the log, and the one
/// that xkb's PC layouts give F12 while Ctrl and Alt are held.
pub const F12: u32 = 0xFFC9;
const SWITCH_VT_12: u32 = 0x1008_FE0C;

/// What a key press means for the console.
pub enum Pressed {
    /// Bytes for the terminal.
    Bytes(Vec<u8>),
    /// Ctrl, Alt and F12, which show the log while held.
    LogView,
    Nothing,
}

pub struct Keyboard {
    keymap: Keymap,
    /// The key that repeats while held, and when it next does.
    repeat: Option<(u16, i64)>,
    /// The F12 key held since it was pressed with Ctrl and Alt.
    log_key: Option<u16>,
}

impl Keyboard {
    pub fn new(keymap: Keymap) -> Keyboard {
        Keyboard {
            keymap,
            repeat: None,
            log_key: None,
        }
    }

    fn modifiers(&self) -> Modifiers {
        Modifiers {
            shift: self.keymap.modifier("Shift"),
            alt: self.keymap.modifier("Mod1"),
            ctrl: self.keymap.modifier("Control"),
        }
    }

    /// What pressing evdev key `code` means while the screen has `modes`,
    /// before the keymap takes the press in.
    fn meaning(&self, code: u16, modes: &Modes) -> Pressed {
        let mods = self.modifiers();
        let keysym = self.keymap.keysym(code);
        if matches!(keysym, F12 | SWITCH_VT_12) && mods.ctrl && mods.alt {
            return Pressed::LogView;
        }
        if let Some(key) = special(keysym) {
            return Pressed::Bytes(encode(key, mods, modes));
        }
        let text = self.keymap.text(code);
        let mut chars = text.chars();
        match (chars.next(), chars.next()) {
            // With Ctrl, xkb gives the control character, which encode
            // makes again from the letter; Alt adds ESC.
            (Some(c), None) => {
                let c = match mods.ctrl && c.is_ascii_control() {
                    true => ((c as u8) | 0x60) as char,
                    false => c,
                };
                Pressed::Bytes(encode(Key::Char(c), mods, modes))
            }
            (Some(_), Some(_)) => Pressed::Bytes(text.into_bytes()),
            (None, _) => Pressed::Nothing,
        }
    }

    /// Takes in a press (`value` 1) or release (0) of evdev key `code` at
    /// `now`, and returns what it means.
    pub fn key(&mut self, code: u16, value: i32, now: i64, modes: &Modes) -> Pressed {
        let pressed = value != 0;
        let meaning = if pressed {
            self.meaning(code, modes)
        } else {
            Pressed::Nothing
        };
        self.keymap.update(code, pressed);
        match (pressed, &meaning) {
            (true, Pressed::Bytes(_)) if self.keymap.repeats(code) => {
                self.repeat = Some((code, now + REPEAT_DELAY));
            }
            (true, Pressed::LogView) => self.log_key = Some(code),
            (false, _) if self.repeat.is_some_and(|(c, _)| c == code) => self.repeat = None,
            (false, _) if self.log_key == Some(code) => self.log_key = None,
            _ => {}
        }
        meaning
    }

    /// Whether Ctrl, Alt and F12 are held, which shows the log.
    pub fn log_view(&self) -> bool {
        let mods = self.modifiers();
        self.log_key.is_some() && mods.ctrl && mods.alt
    }

    /// When the held key next repeats.
    pub fn next_repeat(&self) -> Option<i64> {
        self.repeat.map(|(_, at)| at)
    }

    /// The bytes of the held key's repeat once it is due at `now`.
    pub fn repeat(&mut self, now: i64, modes: &Modes) -> Option<Vec<u8>> {
        let (code, at) = self.repeat?;
        if now < at {
            return None;
        }
        self.repeat = Some((code, now + REPEAT_INTERVAL));
        match self.meaning(code, modes) {
            Pressed::Bytes(bytes) => Some(bytes),
            _ => None,
        }
    }
}
