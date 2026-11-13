//! Scan code set 2: turning the bytes a keyboard sends into key presses
//! and releases with evdev key codes, without the repeated make codes of
//! a held key.

use mikros_evdev::codes::*;
use mikros_evdev::{KEY_BYTES, Keys, bit, clear_bit, set_bit};

/// The prefix of extended codes, of break codes, and of the Pause key's
/// sequence.
const EXTENDED: u8 = 0xE0;
const BREAK: u8 = 0xF0;
const PAUSE: u8 = 0xE1;
/// The bytes that follow `PAUSE` in the Pause key's sequence.
const PAUSE_REST: u8 = 7;
/// Codes that some keyboards send around extended keys as if Shift
/// were pressed or released; they stand for no key of their own.
const FAKE_SHIFTS: [u8; 2] = [0x12, 0x59];

/// The key codes of the codes without a prefix, by code.
#[rustfmt::skip]
const PLAIN: &[(u8, u16)] = &[
    (0x01, KEY_F9), (0x03, KEY_F5), (0x04, KEY_F3), (0x05, KEY_F1),
    (0x06, KEY_F2), (0x07, KEY_F12), (0x09, KEY_F10), (0x0A, KEY_F8),
    (0x0B, KEY_F6), (0x0C, KEY_F4), (0x0D, KEY_TAB), (0x0E, KEY_GRAVE),
    (0x11, KEY_LEFTALT), (0x12, KEY_LEFTSHIFT), (0x13, KEY_KATAKANAHIRAGANA),
    (0x14, KEY_LEFTCTRL), (0x15, KEY_Q), (0x16, KEY_1), (0x1A, KEY_Z),
    (0x1B, KEY_S), (0x1C, KEY_A), (0x1D, KEY_W), (0x1E, KEY_2),
    (0x21, KEY_C), (0x22, KEY_X), (0x23, KEY_D), (0x24, KEY_E),
    (0x25, KEY_4), (0x26, KEY_3), (0x29, KEY_SPACE), (0x2A, KEY_V),
    (0x2B, KEY_F), (0x2C, KEY_T), (0x2D, KEY_R), (0x2E, KEY_5),
    (0x31, KEY_N), (0x32, KEY_B), (0x33, KEY_H), (0x34, KEY_G),
    (0x35, KEY_Y), (0x36, KEY_6), (0x3A, KEY_M), (0x3B, KEY_J),
    (0x3C, KEY_U), (0x3D, KEY_7), (0x3E, KEY_8), (0x41, KEY_COMMA),
    (0x42, KEY_K), (0x43, KEY_I), (0x44, KEY_O), (0x45, KEY_0),
    (0x46, KEY_9), (0x49, KEY_DOT), (0x4A, KEY_SLASH), (0x4B, KEY_L),
    (0x4C, KEY_SEMICOLON), (0x4D, KEY_P), (0x4E, KEY_MINUS), (0x51, KEY_RO),
    (0x52, KEY_APOSTROPHE), (0x54, KEY_LEFTBRACE), (0x55, KEY_EQUAL),
    (0x58, KEY_CAPSLOCK), (0x59, KEY_RIGHTSHIFT), (0x5A, KEY_ENTER),
    (0x5B, KEY_RIGHTBRACE), (0x5D, KEY_BACKSLASH), (0x61, KEY_102ND),
    (0x64, KEY_HENKAN), (0x66, KEY_BACKSPACE), (0x67, KEY_MUHENKAN),
    (0x69, KEY_KP1), (0x6A, KEY_YEN), (0x6B, KEY_KP4), (0x6C, KEY_KP7),
    (0x70, KEY_KP0), (0x71, KEY_KPDOT), (0x72, KEY_KP2), (0x73, KEY_KP5),
    (0x74, KEY_KP6), (0x75, KEY_KP8), (0x76, KEY_ESC), (0x77, KEY_NUMLOCK),
    (0x78, KEY_F11), (0x79, KEY_KPPLUS), (0x7A, KEY_KP3), (0x7B, KEY_KPMINUS),
    (0x7C, KEY_KPASTERISK), (0x7D, KEY_KP9), (0x7E, KEY_SCROLLLOCK),
    (0x83, KEY_F7),
];

/// The key codes of the codes after `EXTENDED`, by code.
#[rustfmt::skip]
const EXTENDED_KEYS: &[(u8, u16)] = &[
    (0x10, KEY_SEARCH), (0x11, KEY_RIGHTALT), (0x14, KEY_RIGHTCTRL),
    (0x15, KEY_PREVIOUSSONG), (0x18, KEY_BOOKMARKS), (0x1F, KEY_LEFTMETA),
    (0x20, KEY_REFRESH), (0x21, KEY_VOLUMEDOWN), (0x23, KEY_MUTE),
    (0x27, KEY_RIGHTMETA), (0x28, KEY_STOP), (0x2B, KEY_CALC),
    (0x2F, KEY_COMPOSE), (0x30, KEY_FORWARD), (0x32, KEY_VOLUMEUP),
    (0x34, KEY_PLAYPAUSE), (0x37, KEY_POWER), (0x38, KEY_BACK),
    (0x3A, KEY_HOMEPAGE), (0x3B, KEY_STOPCD), (0x3F, KEY_SLEEP),
    (0x40, KEY_COMPUTER), (0x48, KEY_MAIL), (0x4A, KEY_KPSLASH),
    (0x4D, KEY_NEXTSONG), (0x50, KEY_MEDIA), (0x5A, KEY_KPENTER),
    (0x5E, KEY_WAKEUP), (0x69, KEY_END), (0x6B, KEY_LEFT),
    (0x6C, KEY_HOME), (0x70, KEY_INSERT), (0x71, KEY_DELETE),
    (0x72, KEY_DOWN), (0x74, KEY_RIGHT), (0x75, KEY_UP),
    (0x7A, KEY_PAGEDOWN), (0x7C, KEY_SYSRQ), (0x7D, KEY_PAGEUP),
];

/// The key code of `code`, after `EXTENDED` when `extended`.
pub fn key(code: u8, extended: bool) -> Option<u16> {
    let table = if extended { EXTENDED_KEYS } else { PLAIN };
    table.iter().find(|&&(c, _)| c == code).map(|&(_, key)| key)
}

/// A key press, `true`, or release, `false`, of a key code.
pub type Change = (u16, bool);

/// Where the decoder is in a sequence of bytes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Prefix {
    None,
    Extended,
    Break,
    ExtendedBreak,
    /// In the Pause sequence, with this many bytes still to come.
    Pause(u8),
}

/// Decodes the bytes of one keyboard.
pub struct Decoder {
    prefix: Prefix,
    /// The keys held down, whose make codes are repeats.
    down: Keys,
}

impl Default for Decoder {
    fn default() -> Self {
        Decoder::new()
    }
}

impl Decoder {
    pub const fn new() -> Decoder {
        Decoder {
            prefix: Prefix::None,
            down: [0; KEY_BYTES],
        }
    }

    /// Takes in `byte` and calls `change` for each key it presses or
    /// releases.
    pub fn feed(&mut self, byte: u8, mut change: impl FnMut(Change)) {
        let (code, extended, pressed) = match (self.prefix, byte) {
            (Prefix::Pause(1), _) => {
                self.prefix = Prefix::None;
                // The Pause key has no break code.
                change((KEY_PAUSE, true));
                change((KEY_PAUSE, false));
                return;
            }
            (Prefix::Pause(n), _) => {
                self.prefix = Prefix::Pause(n - 1);
                return;
            }
            (Prefix::None, EXTENDED) => {
                self.prefix = Prefix::Extended;
                return;
            }
            (Prefix::None, PAUSE) => {
                self.prefix = Prefix::Pause(PAUSE_REST);
                return;
            }
            (Prefix::None, BREAK) => {
                self.prefix = Prefix::Break;
                return;
            }
            (Prefix::Extended, BREAK) => {
                self.prefix = Prefix::ExtendedBreak;
                return;
            }
            (Prefix::None, code) => (code, false, true),
            (Prefix::Extended, code) => (code, true, true),
            (Prefix::Break, code) => (code, false, false),
            (Prefix::ExtendedBreak, code) => (code, true, false),
        };
        self.prefix = Prefix::None;
        if extended && FAKE_SHIFTS.contains(&code) {
            return;
        }
        let Some(key) = key(code, extended) else {
            return;
        };
        match (pressed, bit(&self.down, key)) {
            (true, true) => {}
            (true, false) => {
                set_bit(&mut self.down, key);
                change((key, true));
            }
            (false, true) => {
                clear_bit(&mut self.down, key);
                change((key, false));
            }
            // A release of a key that was not down, after a reset.
            (false, false) => {}
        }
    }

    /// Every key code that some code stands for, for the device's key
    /// bitmap.
    pub fn keys() -> Keys {
        let mut keys = [0; KEY_BYTES];
        for &(_, key) in PLAIN.iter().chain(EXTENDED_KEYS.iter()) {
            set_bit(&mut keys, key);
        }
        set_bit(&mut keys, KEY_PAUSE);
        keys
    }
}
