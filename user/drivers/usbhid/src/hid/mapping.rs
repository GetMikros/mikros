//! Turning the input reports of a keyboard, mouse or pointer
//! application collection into evdev events, and its LEDs into output
//! reports.

use crate::hid::descriptor::{
    BUTTON, CONSUMER, Descriptor, Field, GENERIC_DESKTOP, KEYBOARD, Kind, LED, usage,
};
use alloc::vec;
use alloc::vec::Vec;
use mikros_evdev::codes::*;
use mikros_evdev::{KEY_BYTES, Keys, set_bit};

/// The Generic Desktop usages of the applications that become devices.
const POINTER: u16 = 0x01;
const MOUSE: u16 = 0x02;
const KEYBOARD_APPLICATION: u16 = 0x06;
/// Generic Desktop axes, and the Consumer page's AC Pan.
const X: u16 = 0x30;
const Y: u16 = 0x31;
const WHEEL: u16 = 0x38;
const AC_PAN: u16 = 0x0238;
/// The keyboard usage of a report with more keys down than it can
/// carry.
const ERROR_ROLL_OVER: u16 = 0x01;
/// The LED usages and their evdev LED codes.
const LEDS: [(u16, u16); 3] = [(0x01, LED_NUML), (0x02, LED_CAPSL), (0x03, LED_SCROLLL)];
/// The buttons 1 to 5.
const BUTTONS: [u16; 5] = [BTN_LEFT, BTN_RIGHT, BTN_MIDDLE, BTN_SIDE, BTN_EXTRA];

/// The evdev key codes of the keyboard page's usages.
#[rustfmt::skip]
const KEYS: &[(u16, u16)] = &[
    (0x04, KEY_A), (0x05, KEY_B), (0x06, KEY_C), (0x07, KEY_D), (0x08, KEY_E),
    (0x09, KEY_F), (0x0A, KEY_G), (0x0B, KEY_H), (0x0C, KEY_I), (0x0D, KEY_J),
    (0x0E, KEY_K), (0x0F, KEY_L), (0x10, KEY_M), (0x11, KEY_N), (0x12, KEY_O),
    (0x13, KEY_P), (0x14, KEY_Q), (0x15, KEY_R), (0x16, KEY_S), (0x17, KEY_T),
    (0x18, KEY_U), (0x19, KEY_V), (0x1A, KEY_W), (0x1B, KEY_X), (0x1C, KEY_Y),
    (0x1D, KEY_Z), (0x1E, KEY_1), (0x1F, KEY_2), (0x20, KEY_3), (0x21, KEY_4),
    (0x22, KEY_5), (0x23, KEY_6), (0x24, KEY_7), (0x25, KEY_8), (0x26, KEY_9),
    (0x27, KEY_0), (0x28, KEY_ENTER), (0x29, KEY_ESC), (0x2A, KEY_BACKSPACE),
    (0x2B, KEY_TAB), (0x2C, KEY_SPACE), (0x2D, KEY_MINUS), (0x2E, KEY_EQUAL),
    (0x2F, KEY_LEFTBRACE), (0x30, KEY_RIGHTBRACE), (0x31, KEY_BACKSLASH),
    (0x32, KEY_BACKSLASH), (0x33, KEY_SEMICOLON), (0x34, KEY_APOSTROPHE),
    (0x35, KEY_GRAVE), (0x36, KEY_COMMA), (0x37, KEY_DOT), (0x38, KEY_SLASH),
    (0x39, KEY_CAPSLOCK), (0x3A, KEY_F1), (0x3B, KEY_F2), (0x3C, KEY_F3),
    (0x3D, KEY_F4), (0x3E, KEY_F5), (0x3F, KEY_F6), (0x40, KEY_F7), (0x41, KEY_F8),
    (0x42, KEY_F9), (0x43, KEY_F10), (0x44, KEY_F11), (0x45, KEY_F12),
    (0x46, KEY_SYSRQ), (0x47, KEY_SCROLLLOCK), (0x48, KEY_PAUSE), (0x49, KEY_INSERT),
    (0x4A, KEY_HOME), (0x4B, KEY_PAGEUP), (0x4C, KEY_DELETE), (0x4D, KEY_END),
    (0x4E, KEY_PAGEDOWN), (0x4F, KEY_RIGHT), (0x50, KEY_LEFT), (0x51, KEY_DOWN),
    (0x52, KEY_UP), (0x53, KEY_NUMLOCK), (0x54, KEY_KPSLASH), (0x55, KEY_KPASTERISK),
    (0x56, KEY_KPMINUS), (0x57, KEY_KPPLUS), (0x58, KEY_KPENTER), (0x59, KEY_KP1),
    (0x5A, KEY_KP2), (0x5B, KEY_KP3), (0x5C, KEY_KP4), (0x5D, KEY_KP5), (0x5E, KEY_KP6),
    (0x5F, KEY_KP7), (0x60, KEY_KP8), (0x61, KEY_KP9), (0x62, KEY_KP0), (0x63, KEY_KPDOT),
    (0x64, KEY_102ND), (0x65, KEY_COMPOSE), (0x66, KEY_POWER), (0x67, KEY_KPEQUAL),
    (0x68, KEY_F13), (0x69, KEY_F14), (0x6A, KEY_F15), (0x6B, KEY_F16), (0x6C, KEY_F17),
    (0x6D, KEY_F18), (0x6E, KEY_F19), (0x6F, KEY_F20), (0x70, KEY_F21), (0x71, KEY_F22),
    (0x72, KEY_F23), (0x73, KEY_F24), (0x74, KEY_OPEN), (0x75, KEY_HELP), (0x76, KEY_PROPS),
    (0x77, KEY_FRONT), (0x78, KEY_STOP), (0x79, KEY_AGAIN), (0x7A, KEY_UNDO), (0x7B, KEY_CUT),
    (0x7C, KEY_COPY), (0x7D, KEY_PASTE), (0x7E, KEY_FIND), (0x7F, KEY_MUTE),
    (0x80, KEY_VOLUMEUP), (0x81, KEY_VOLUMEDOWN), (0x85, KEY_KPCOMMA), (0x87, KEY_RO),
    (0x88, KEY_KATAKANAHIRAGANA), (0x89, KEY_YEN), (0x8A, KEY_HENKAN), (0x8B, KEY_MUHENKAN),
    (0x8C, KEY_KPJPCOMMA), (0x90, KEY_HANGEUL), (0x91, KEY_HANJA), (0x92, KEY_KATAKANA),
    (0x93, KEY_HIRAGANA), (0x94, KEY_ZENKAKUHANKAKU), (0xE0, KEY_LEFTCTRL),
    (0xE1, KEY_LEFTSHIFT), (0xE2, KEY_LEFTALT), (0xE3, KEY_LEFTMETA), (0xE4, KEY_RIGHTCTRL),
    (0xE5, KEY_RIGHTSHIFT), (0xE6, KEY_RIGHTALT), (0xE7, KEY_RIGHTMETA),
];

/// The kinds of application collections that become input devices.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Application {
    Keyboard,
    Mouse,
    Pointer,
}

/// The applications of `descriptor` that become devices, by index.
pub fn applications(descriptor: &Descriptor) -> Vec<(usize, Application)> {
    let mut found = Vec::new();
    for (at, &u) in descriptor.applications.iter().enumerate() {
        let kind = match u {
            _ if u == usage(GENERIC_DESKTOP, KEYBOARD_APPLICATION) => Application::Keyboard,
            _ if u == usage(GENERIC_DESKTOP, MOUSE) => Application::Mouse,
            _ if u == usage(GENERIC_DESKTOP, POINTER) => Application::Pointer,
            _ => continue,
        };
        found.push((at, kind));
    }
    found
}

/// What a usage becomes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Target {
    Key(u16),
    Relative(u16),
    Absolute(u16),
}

fn target(field: &Field, u: u32) -> Option<Target> {
    let (page, id) = ((u >> 16) as u16, u as u16);
    match page {
        KEYBOARD => KEYS
            .iter()
            .find(|&&(k, _)| k == id)
            .map(|&(_, key)| Target::Key(key)),
        BUTTON => match id {
            1..=5 => Some(Target::Key(BUTTONS[usize::from(id) - 1])),
            _ => None,
        },
        GENERIC_DESKTOP => {
            let axis = match id {
                X => REL_X,
                Y => REL_Y,
                WHEEL => return Some(Target::Relative(REL_WHEEL)),
                _ => return None,
            };
            // ABS_X and ABS_Y have the codes of REL_X and REL_Y.
            Some(if field.relative() {
                Target::Relative(axis)
            } else {
                Target::Absolute(axis)
            })
        }
        CONSUMER if id == AC_PAN => Some(Target::Relative(REL_HWHEEL)),
        _ => None,
    }
}

/// An absolute axis and its logical range.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Axis {
    pub code: u16,
    pub minimum: i32,
    pub maximum: i32,
}

/// What a device reports, for its `DeviceInfo`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Capabilities {
    pub event_types: u32,
    pub keys: Keys,
    pub relative_axes: u16,
    pub absolute_axes: Vec<Axis>,
    pub leds: u16,
}

/// The input device of one application collection.
pub struct Device {
    fields: Vec<Field>,
    report_ids: bool,
    /// The previous values of each variable field, and the keys of each
    /// array field.
    values: Vec<Vec<Option<i32>>>,
    held: Vec<Vec<u16>>,
    /// The LED output report: its ID, its length with the ID byte, and
    /// the field and value index of each LED.
    leds: Option<LedReport>,
}

/// An LED output report: its ID, its length with the ID byte, and the
/// evdev LED code, field and value index of each LED.
type LedReport = (u8, usize, Vec<(u16, usize, u32)>);

impl Device {
    /// The device of application `at` of `descriptor`.
    pub fn new(descriptor: &Descriptor, at: usize) -> Device {
        let fields: Vec<Field> = descriptor
            .fields
            .iter()
            .filter(|f| f.application == Some(at) && !f.constant())
            .cloned()
            .collect();
        let values = fields
            .iter()
            .map(|f| vec![None; f.count.min(1024) as usize])
            .collect();
        let held = fields.iter().map(|_| Vec::new()).collect();
        let mut device = Device {
            fields,
            report_ids: descriptor.report_ids,
            values,
            held,
            leds: None,
        };
        device.leds = device.find_leds(descriptor);
        device
    }

    fn find_leds(&self, descriptor: &Descriptor) -> Option<LedReport> {
        let mut report = None;
        let mut leds = Vec::new();
        for (at, field) in self.fields.iter().enumerate() {
            if field.kind != Kind::Output || !field.variable() {
                continue;
            }
            for n in 0..field.count.min(1024) {
                let Some(u) = field.variable_usage(n) else {
                    continue;
                };
                let Some(&(_, code)) = LEDS.iter().find(|&&(id, _)| u == usage(LED, id)) else {
                    continue;
                };
                // The LEDs of the first output report that has any.
                if *report.get_or_insert(field.report_id) == field.report_id {
                    leds.push((code, at, n));
                }
            }
        }
        let id = report?;
        let len = descriptor.report_len_of(Kind::Output, id)?;
        Some((id, len, leds))
    }

    pub fn capabilities(&self) -> Capabilities {
        let mut c = Capabilities {
            event_types: 1 << EV_SYN,
            keys: [0; KEY_BYTES],
            relative_axes: 0,
            absolute_axes: Vec::new(),
            leds: 0,
        };
        for field in self.fields.iter().filter(|f| f.kind == Kind::Input) {
            let n = if field.variable() {
                field.count
            } else {
                u32::MAX
            };
            let mut add = |t| match t {
                Target::Key(key) => {
                    set_bit(&mut c.keys, key);
                    c.event_types |= 1 << EV_KEY;
                }
                Target::Relative(axis) => {
                    c.relative_axes |= 1 << axis;
                    c.event_types |= 1 << EV_REL;
                }
                Target::Absolute(axis) => {
                    if !c.absolute_axes.iter().any(|a| a.code == axis) {
                        c.absolute_axes.push(Axis {
                            code: axis,
                            minimum: field.logical_minimum,
                            maximum: field.logical_maximum,
                        });
                    }
                    c.event_types |= 1 << EV_ABS;
                }
            };
            for span in &field.usages {
                // A variable field uses at most `count` usages.
                let last = span
                    .last
                    .min(span.first.saturating_add(n.saturating_sub(1)));
                for u in span.first..=last.min(span.first.saturating_add(0xFFFF)) {
                    if let Some(t) = target(field, u) {
                        add(t);
                    }
                }
            }
        }
        if let Some((_, _, leds)) = &self.leds {
            for &(code, _, _) in leds {
                c.leds |= 1 << code;
            }
            c.event_types |= 1 << EV_LED;
        }
        c
    }

    /// The events of input report `report`, with its report ID byte when
    /// the descriptor has report IDs, as (type, code, value).
    pub fn decode(&mut self, report: &[u8]) -> Vec<(u16, u16, i32)> {
        let (id, data) = match (self.report_ids, report) {
            (true, [id, data @ ..]) => (*id, data),
            (true, []) => return Vec::new(),
            (false, data) => (0, data),
        };
        let mine = |f: &Field| f.kind == Kind::Input && f.report_id == id;
        // A keyboard that could not tell its keys reports ErrorRollOver.
        let roll_over = usage(KEYBOARD, ERROR_ROLL_OVER);
        let rolled_over = self
            .fields
            .iter()
            .filter(|f| mine(f) && !f.variable())
            .any(|f| (0..f.count).any(|n| array_usage(f, data, n) == Some(roll_over)));
        if rolled_over {
            return Vec::new();
        }
        let mut events = Vec::new();
        for (at, field) in self.fields.iter().enumerate() {
            if !mine(field) {
                continue;
            }
            if field.variable() {
                for n in 0..field.count.min(1024) {
                    let Some(value) = field.value(data, n) else {
                        continue;
                    };
                    let Some(t) = field.variable_usage(n).and_then(|u| target(field, u)) else {
                        continue;
                    };
                    let previous = self.values[at][n as usize].replace(value);
                    match t {
                        Target::Key(key) => {
                            let down = value != 0;
                            if previous.is_some_and(|p| p != 0) != down {
                                events.push((EV_KEY, key, i32::from(down)));
                            }
                        }
                        Target::Relative(axis) if value != 0 => events.push((EV_REL, axis, value)),
                        Target::Relative(_) => {}
                        Target::Absolute(axis) if previous != Some(value) => {
                            events.push((EV_ABS, axis, value))
                        }
                        Target::Absolute(_) => {}
                    }
                }
            } else {
                let keys: Vec<u16> = (0..field.count.min(1024))
                    .filter_map(|n| array_usage(field, data, n))
                    .filter_map(|u| match target(field, u) {
                        Some(Target::Key(key)) => Some(key),
                        _ => None,
                    })
                    .collect();
                let held = &self.held[at];
                for &key in held.iter().filter(|k| !keys.contains(k)) {
                    events.push((EV_KEY, key, 0));
                }
                for (n, &key) in keys.iter().enumerate() {
                    if !held.contains(&key) && !keys[..n].contains(&key) {
                        events.push((EV_KEY, key, 1));
                    }
                }
                self.held[at] = keys;
            }
        }
        events
    }

    /// The output report that lights the evdev LED bits `leds`, with
    /// its report ID byte when the descriptor has report IDs, or None
    /// when the device has no LEDs.
    pub fn led_report(&self, leds: u16) -> Option<Vec<u8>> {
        let (id, len, map) = self.leds.as_ref()?;
        let mut report = vec![0u8; *len];
        let skip = usize::from(self.report_ids);
        if self.report_ids {
            report[0] = *id;
        }
        for &(code, at, n) in map {
            if leds & (1 << code) == 0 {
                continue;
            }
            let field = &self.fields[at];
            // An LED is on with its logical maximum, usually 1.
            let value = field.logical_maximum.max(1) as u32;
            let start = field.offset + field.size * n;
            for bit in 0..field.size.min(32) {
                if value & (1 << bit) != 0 {
                    let at = (start + bit) as usize;
                    if let Some(byte) = report.get_mut(skip + at / 8) {
                        *byte |= 1 << (at % 8);
                    }
                }
            }
        }
        Some(report)
    }
}

/// The usage that value `n` of array field `field` stands for, if any.
fn array_usage(field: &Field, data: &[u8], n: u32) -> Option<u32> {
    let value = field.value(data, n)?;
    if value < field.logical_minimum || value > field.logical_maximum {
        return None;
    }
    let index = u32::try_from(i64::from(value) - i64::from(field.logical_minimum)).ok()?;
    field.nth_usage(index)
}
