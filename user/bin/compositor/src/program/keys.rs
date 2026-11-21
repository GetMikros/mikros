//! The keyboards of the seat, whose devices `mouse` reads. They share one
//! keymap and its state, compiled from the system's keyboard layout
//! (MIKSPEC-0066.R6) when the first keyboard comes, and again whenever
//! the layout changes (MIKSPEC-0066.R29), whose text the clients'
//! keyboards get. Ctrl, Alt and a function key whose keysym is
//! that of F2 to F7, or `XF86Switch_VT_2` to `XF86Switch_VT_7`, which
//! xkb's PC layouts give them while Ctrl and Alt are held, ask the
//! session manager for the screen of a text console (MIKSPEC-0066.R12):
//! no client gets the press or the release of that key. Screen 1 is the
//! desktop's own. Every other key goes to the window with the keyboard
//! focus. When a device stops, the keys that it held are released.

use super::Compositor;
use crate::keyboard;
use crate::server::Modifiers;
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

/// The layout of the keyboard settings, or xkbcommon's defaults when
/// their file is absent or invalid, which the theme loop reports.
fn read_layout() -> keyboard::Layout {
    let text = std::fs::read(keyboard::PATH).ok();
    let settings = text.and_then(|text| keyboard::parse(&text).ok());
    settings.map(|(s, _)| s.layout).unwrap_or_default()
}

/// The keymap of `layout`, or the default keymap when xkbcommon cannot
/// compile it.
fn compile(layout: &keyboard::Layout) -> Option<mikros_xkb::Keymap> {
    let names = mikros_xkb::Names {
        model: layout.model.clone(),
        layout: layout.layout.clone(),
        variant: layout.variant.clone(),
        options: layout.options.clone(),
    };
    let keymap = mikros_xkb::Keymap::new(&names).or_else(|| {
        mikros_rt::log!(Error, "compositor: the keymap {names:?} cannot be compiled");
        mikros_xkb::Keymap::new(&mikros_xkb::Names::default())
    });
    if keymap.is_none() {
        mikros_rt::log!(Error, "compositor: the default keymap cannot be compiled");
    }
    keymap
}

/// The modifiers of `keymap` as the server takes them.
fn modifiers(keymap: &mikros_xkb::Keymap) -> Modifiers {
    let m = keymap.modifiers();
    Modifiers {
        depressed: m.depressed,
        latched: m.latched,
        locked: m.locked,
        group: m.group,
    }
}

/// A keyboard of the seat, which asks through `seat` for the screens
/// whose keys are pressed: the keys that it holds, and those of them that
/// switched screens.
pub struct Keys {
    seat: Rc<SeatAsyncServer>,
    held: Vec<u16>,
    switched: Vec<u16>,
}

impl Compositor {
    /// Compiles the keymap, once, and gives the server its text. False
    /// when it cannot be compiled.
    fn compile_keymap(&self) -> bool {
        if self.keymap.borrow().is_some() {
            return true;
        }
        let layout = read_layout();
        let Some(keymap) = compile(&layout) else {
            return false;
        };
        self.give_keymap(&keymap);
        *self.keymap.borrow_mut() = Some(keymap);
        *self.layout.borrow_mut() = Some(layout);
        true
    }

    /// Compiles the keymap again when `layout` is not the one compiled,
    /// with the keys that are held pressed in its state, and gives the
    /// server its text (MIKSPEC-0066.R29). Nothing is compiled before the
    /// first keyboard came.
    pub(super) fn relayout(&self, layout: &keyboard::Layout) {
        if self
            .layout
            .borrow()
            .as_ref()
            .is_none_or(|compiled| compiled == layout)
        {
            return;
        }
        *self.layout.borrow_mut() = Some(layout.clone());
        let Some(mut keymap) = compile(layout) else {
            return;
        };
        for key in self.server.borrow().keys_held() {
            keymap.update(key as u16, true);
        }
        self.give_keymap(&keymap);
        *self.keymap.borrow_mut() = Some(keymap);
        self.flush_all();
    }

    /// Gives the server the text of `keymap` and its modifiers.
    fn give_keymap(&self, keymap: &mikros_xkb::Keymap) {
        let Some(text) = keymap.text_v1() else {
            mikros_rt::log!(Error, "compositor: the keymap cannot be written");
            return;
        };
        let mut store = self.store.borrow_mut();
        let mut importer = super::imports::Importer {
            renderer: &self.renderer,
            store: &mut store,
        };
        let mut server = self.server.borrow_mut();
        server.set_keymap(&mut importer, text, modifiers(keymap));
    }
}

impl Keys {
    /// The keyboard of the device of `info`, the instance `instance`, or
    /// None for a device without letter keys or when no keymap can be
    /// compiled.
    pub fn new(
        compositor: &Compositor,
        info: &DeviceInfo,
        instance: &str,
        seat: Rc<SeatAsyncServer>,
    ) -> Option<Keys> {
        if info.keys[usize::from(KEY_A / 8)] & (1 << (KEY_A % 8)) == 0 {
            return None;
        }
        if !compositor.compile_keymap() {
            return None;
        }
        mikros_rt::log!(Info, "compositor: reading the keyboard {instance}");
        Some(Keys {
            seat,
            held: Vec::new(),
            switched: Vec::new(),
        })
    }

    /// Takes the key event of `code` with `value`, 0 for a release, 1 for
    /// a press and 2 for a repeat, at `time` in milliseconds.
    pub fn key(&mut self, compositor: &Compositor, code: u16, value: i32, time: u32) {
        let pressed = match value {
            0 if self.held.contains(&code) => false,
            1 if !self.held.contains(&code) => true,
            _ => return,
        };
        let mut keymap = compositor.keymap.borrow_mut();
        let Some(keymap) = keymap.as_mut() else {
            return;
        };
        let switch = pressed && keymap.modifier("Control") && keymap.modifier("Mod1");
        if let Some(screen) =
            screen_key(keymap.keysym(code)).filter(|&screen| switch && screen != DESKTOP)
        {
            let _ = self.seat.server().send_on_switch_requested(screen);
            self.switched.push(code);
        }
        keymap.update(code, pressed);
        match pressed {
            true => self.held.push(code),
            false => self.held.retain(|&k| k != code),
        }
        if !pressed && self.switched.contains(&code) {
            self.switched.retain(|&k| k != code);
            return;
        }
        if self.switched.contains(&code) {
            return;
        }
        let modifiers = modifiers(keymap);
        let mut server = compositor.server.borrow_mut();
        server.keyboard_key(u32::from(code), pressed, time, modifiers);
    }

    /// Releases the keys that the device holds, at `time`, as when it
    /// stops.
    pub fn release_all(&mut self, compositor: &Compositor, time: u32) {
        for code in self.held.clone() {
            self.key(compositor, code, 0, time);
        }
    }
}
