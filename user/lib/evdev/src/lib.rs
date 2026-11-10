//! Input events in the evdev model: their types and codes, the bitmaps
//! that describe what a device reports, the state of a device's keys,
//! LEDs and absolute axes as its events leave it, and a stream that
//! sends events in groups ending with `SYN_REPORT` and starts again with
//! `SYN_DROPPED` after groups were lost.

#![no_std]

extern crate alloc;

pub mod codes;
#[cfg(test)]
mod tests;

use alloc::vec::Vec;
use codes::{EV_ABS, EV_KEY, EV_LED, EV_SYN, KEY_MAX, SYN_DROPPED, SYN_REPORT};

/// The number of bytes of a key bitmap.
pub const KEY_BYTES: usize = (KEY_MAX as usize + 1) / 8;

/// One event: when the driver received the report it came from, in
/// monotonic clock nanoseconds, and its type, code and value.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Event {
    pub timestamp: i64,
    pub kind: u16,
    pub code: u16,
    pub value: i32,
}

impl Event {
    pub const fn new(timestamp: i64, kind: u16, code: u16, value: i32) -> Event {
        Event {
            timestamp,
            kind,
            code,
            value,
        }
    }
}

/// A bitmap of key codes, bit `n` standing for code `n`, numbered from
/// bit 0 of byte 0.
pub type Keys = [u8; KEY_BYTES];

/// Sets the bit of `code` in `bits`; codes beyond the bitmap are left
/// out.
pub fn set_bit(bits: &mut [u8], code: u16) {
    if let Some(byte) = bits.get_mut(usize::from(code / 8)) {
        *byte |= 1 << (code % 8);
    }
}

/// Clears the bit of `code` in `bits`.
pub fn clear_bit(bits: &mut [u8], code: u16) {
    if let Some(byte) = bits.get_mut(usize::from(code / 8)) {
        *byte &= !(1 << (code % 8));
    }
}

/// Whether the bit of `code` is set in `bits`.
pub fn bit(bits: &[u8], code: u16) -> bool {
    bits.get(usize::from(code / 8))
        .is_some_and(|byte| byte & (1 << (code % 8)) != 0)
}

/// The keys held down, the LEDs lit and the values of the absolute axes
/// of a device, after the events it has reported.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct State {
    pub keys: Keys,
    pub leds: u16,
    /// Each absolute axis the device has, with its value.
    pub absolute: Vec<(u16, i32)>,
}

impl State {
    /// The state of a device with these absolute axes, all at 0.
    pub fn new(axes: &[u16]) -> State {
        State {
            keys: [0; KEY_BYTES],
            leds: 0,
            absolute: axes.iter().map(|&code| (code, 0)).collect(),
        }
    }

    /// Takes in `event`.
    pub fn apply(&mut self, event: &Event) {
        match event.kind {
            EV_KEY if event.value == 0 => clear_bit(&mut self.keys, event.code),
            EV_KEY => set_bit(&mut self.keys, event.code),
            EV_LED if event.code < 16 => {
                let bit = 1 << event.code;
                if event.value == 0 {
                    self.leds &= !bit;
                } else {
                    self.leds |= bit;
                }
            }
            EV_ABS => {
                if let Some(axis) = self.absolute.iter_mut().find(|(c, _)| *c == event.code) {
                    axis.1 = event.value;
                }
            }
            _ => {}
        }
    }
}

/// The events of one report, ended with `SYN_REPORT`.
pub fn group(timestamp: i64, events: impl IntoIterator<Item = Event>) -> Vec<Event> {
    let mut group: Vec<Event> = events.into_iter().collect();
    group.push(Event::new(timestamp, EV_SYN, SYN_REPORT, 0));
    group
}

/// Sends groups of events to a reader, each group whole. A group that
/// cannot be sent is lost, along with those after it, until one can be
/// sent again; before that one, the reader is sent `SYN_DROPPED` alone.
#[derive(Debug, Default)]
pub struct Stream {
    dropped: bool,
}

impl Stream {
    pub const fn new() -> Stream {
        Stream { dropped: false }
    }

    /// Whether groups were lost since the last `SYN_DROPPED` was sent.
    pub fn dropped(&self) -> bool {
        self.dropped
    }

    /// Sends `group` with `send`, which fails when it cannot queue it.
    pub fn send<E>(&mut self, group: &[Event], mut send: impl FnMut(&[Event]) -> Result<(), E>) {
        let Some(last) = group.last() else {
            return;
        };
        if self.dropped {
            let dropped = Event::new(last.timestamp, EV_SYN, SYN_DROPPED, 0);
            if send(&[dropped]).is_err() {
                return;
            }
            self.dropped = false;
        }
        if send(group).is_err() {
            self.dropped = true;
        }
    }
}
