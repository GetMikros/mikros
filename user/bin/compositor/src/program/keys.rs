//! The keyboards of the seat, whose devices `mouse` reads, for the keys
//! that switch screens: Ctrl, Alt and a function key whose keysym is that
//! of F2 to F7, or `XF86Switch_VT_2` to `XF86Switch_VT_7`, which xkb's PC
//! layouts give them while Ctrl and Alt are held, ask the session manager
//! for the screen of a text console (MIKSPEC-0066.R12). Screen 1 is the
//! desktop's own. Clients get no keys yet. The keymap is compiled from
//! the system's keyboard layout (MIKSPEC-0066.R6).

use crate::keyboard;
use mikros_evdev::codes::KEY_A;
use mikros_rt::interfaces::mikros::input::DeviceInfo;
use mikros_rt::interfaces::mikros::session::SeatAsyncServer;
use std::rc::Rc;

/// The keysyms of F1 and of `XF86Switch_VT_1`; those of the next keys
/// follow them.
const F1: u32 = 0xFFBE;
const SWITCH_VT_1: u32 = 0x1008_FE01;
/// The desktop's screen, and the number of screens.
const DESKTOP: u8 = 1;
const SCREENS: u32 = 7;

/// The screen, 1 to 7, that `keysym` shows with Ctrl and Alt.
fn screen_key(keysym: u32) -> Option<u8> {
    [F1, SWITCH_VT_1]
        .into_iter()
        .find_map(|first| keysym.checked_sub(first).filter(|&i| i < SCREENS))
        .map(|i| i as u8 + 1)
}

/// The names of the system's keyboard layout, or xkbcommon's defaults
/// when its file is absent or invalid.
fn layout() -> mikros_xkb::Names {
    let layout = match std::fs::read(keyboard::PATH) {
        Ok(text) => match keyboard::parse(&text) {
            Ok((layout, ignored)) => {
                for note in ignored {
                    mikros_rt::log!(Warning, "compositor: {}: {note}", keyboard::PATH);
                }
                layout
            }
            Err(e) => {
                mikros_rt::log!(Error, "compositor: {}: {e}", keyboard::PATH);
                keyboard::Layout::default()
            }
        },
        Err(_) => keyboard::Layout::default(),
    };
    mikros_xkb::Names {
        model: layout.model,
        layout: layout.layout,
        variant: layout.variant,
        options: layout.options,
    }
}

/// A keyboard of the seat, with its keymap's state, which asks through
/// `seat` for the screens whose keys are pressed.
pub struct Keys {
    keymap: mikros_xkb::Keymap,
    seat: Rc<SeatAsyncServer>,
}

impl Keys {
    /// The keyboard of the device of `info`, the instance `instance`, or
    /// None for a device without letter keys or a keymap that cannot be
    /// compiled.
    pub fn new(info: &DeviceInfo, instance: &str, seat: Rc<SeatAsyncServer>) -> Option<Keys> {
        if info.keys[usize::from(KEY_A / 8)] & (1 << (KEY_A % 8)) == 0 {
            return None;
        }
        let names = layout();
        let keymap = mikros_xkb::Keymap::new(&names).or_else(|| {
            mikros_rt::log!(Error, "compositor: the keymap {names:?} cannot be compiled");
            mikros_xkb::Keymap::new(&mikros_xkb::Names::default())
        });
        let Some(keymap) = keymap else {
            mikros_rt::log!(Error, "compositor: the default keymap cannot be compiled");
            return None;
        };
        mikros_rt::log!(Info, "compositor: reading the keyboard {instance}");
        Some(Keys { keymap, seat })
    }

    /// Takes the key event of `code` with `value`, 0 for a release, 1 for
    /// a press and 2 for a repeat.
    pub fn key(&mut self, code: u16, value: i32) {
        if value == 2 {
            return;
        }
        let pressed = value == 1;
        let keymap = &mut self.keymap;
        let switch = pressed && keymap.modifier("Control") && keymap.modifier("Mod1");
        if let Some(screen) =
            screen_key(keymap.keysym(code)).filter(|&screen| switch && screen != DESKTOP)
        {
            let _ = self.seat.server().send_on_switch_requested(screen);
        }
        keymap.update(code, pressed);
    }
}
