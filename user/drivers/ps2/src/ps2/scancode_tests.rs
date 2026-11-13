extern crate std;

use crate::ps2::scancode::{Change, Decoder};
use mikros_evdev::bit;
use mikros_evdev::codes::*;
use std::vec;
use std::vec::Vec;

fn decode(bytes: &[u8]) -> Vec<Change> {
    let mut d = Decoder::new();
    let mut out = Vec::new();
    for &b in bytes {
        d.feed(b, |c| out.push(c));
    }
    out
}

#[test]
fn make_and_break_codes() {
    assert_eq!(
        decode(&[0x1C, 0xF0, 0x1C]),
        vec![(KEY_A, true), (KEY_A, false)]
    );
    assert_eq!(decode(&[0x5A]), vec![(KEY_ENTER, true)]);
    assert_eq!(
        decode(&[0x83, 0xF0, 0x83]),
        vec![(KEY_F7, true), (KEY_F7, false)]
    );
}

#[test]
fn extended_codes() {
    assert_eq!(
        decode(&[0xE0, 0x75, 0xE0, 0xF0, 0x75]),
        vec![(KEY_UP, true), (KEY_UP, false)]
    );
    assert_eq!(decode(&[0xE0, 0x14]), vec![(KEY_RIGHTCTRL, true)]);
    assert_eq!(decode(&[0xE0, 0x7D]), vec![(KEY_PAGEUP, true)]);
    // The keypad's Enter differs from the main one only by the prefix.
    assert_eq!(decode(&[0xE0, 0x5A]), vec![(KEY_KPENTER, true)]);
}

#[test]
fn print_screen_ignores_the_fake_shift() {
    assert_eq!(
        decode(&[0xE0, 0x12, 0xE0, 0x7C, 0xE0, 0xF0, 0x7C, 0xE0, 0xF0, 0x12]),
        vec![(KEY_SYSRQ, true), (KEY_SYSRQ, false)]
    );
}

#[test]
fn pause_is_pressed_and_released_at_once() {
    assert_eq!(
        decode(&[0xE1, 0x14, 0x77, 0xE1, 0xF0, 0x14, 0xF0, 0x77, 0x1C]),
        vec![(KEY_PAUSE, true), (KEY_PAUSE, false), (KEY_A, true)]
    );
}

#[test]
fn typematic_repeats_are_not_new_presses() {
    assert_eq!(
        decode(&[0x1C, 0x1C, 0x1C, 0xF0, 0x1C, 0x1C]),
        vec![(KEY_A, true), (KEY_A, false), (KEY_A, true)]
    );
}

#[test]
fn unknown_codes_and_stray_breaks_are_ignored() {
    assert_eq!(
        decode(&[0x02, 0xF0, 0x1C, 0xE0, 0x01, 0x1C]),
        vec![(KEY_A, true)]
    );
}

#[test]
fn the_key_bitmap_covers_the_table() {
    let keys = Decoder::keys();
    for key in [
        KEY_A, KEY_ENTER, KEY_1, KEY_PAUSE, KEY_PAGEUP, KEY_SYSRQ, KEY_F7,
    ] {
        assert!(bit(&keys, key), "{key}");
    }
    assert!(!bit(&keys, BTN_LEFT));
}
