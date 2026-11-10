use crate::descriptor::parse;
use crate::descriptor_tests::{KEYBOARD, TABLET};
use crate::mapping::*;
use alloc::vec;
use mikros_evdev::bit;
use mikros_evdev::codes::*;

fn keyboard() -> Device {
    let d = parse(KEYBOARD).unwrap();
    assert_eq!(applications(&d), [(0, Application::Keyboard)]);
    Device::new(&d, 0, Application::Keyboard)
}

#[test]
fn keyboard_capabilities() {
    let c = keyboard().capabilities();
    assert_eq!(c.event_types, 1 << EV_SYN | 1 << EV_KEY | 1 << EV_LED);
    for key in [
        KEY_A,
        KEY_Z,
        KEY_ENTER,
        KEY_LEFTCTRL,
        KEY_RIGHTMETA,
        KEY_102ND,
        KEY_COMPOSE,
    ] {
        assert!(bit(&c.keys, key), "{key}");
    }
    assert!(!bit(&c.keys, BTN_LEFT));
    assert_eq!(c.leds, 1 << LED_NUML | 1 << LED_CAPSL | 1 << LED_SCROLLL);
}

#[test]
fn key_arrays_report_presses_and_releases() {
    let mut k = keyboard();
    // Left Shift and A.
    assert_eq!(
        k.decode(&[0x02, 0, 0x04, 0, 0, 0, 0, 0]),
        vec![(EV_KEY, KEY_LEFTSHIFT, 1), (EV_KEY, KEY_A, 1)]
    );
    // A held, B added in another slot.
    assert_eq!(
        k.decode(&[0x02, 0, 0x05, 0x04, 0, 0, 0, 0]),
        vec![(EV_KEY, KEY_B, 1)]
    );
    // Shift and A released.
    assert_eq!(
        k.decode(&[0, 0, 0x05, 0, 0, 0, 0, 0]),
        vec![(EV_KEY, KEY_LEFTSHIFT, 0), (EV_KEY, KEY_A, 0)]
    );
    // The same report again changes nothing.
    assert_eq!(k.decode(&[0, 0, 0x05, 0, 0, 0, 0, 0]), vec![]);
}

#[test]
fn error_roll_over_reports_are_ignored() {
    let mut k = keyboard();
    k.decode(&[0, 0, 0x04, 0, 0, 0, 0, 0]);
    assert_eq!(
        k.decode(&[0x01, 0, 0x01, 0x01, 0x01, 0x01, 0x01, 0x01]),
        vec![]
    );
    // A is still held: the next report releases it.
    assert_eq!(
        k.decode(&[0, 0, 0, 0, 0, 0, 0, 0]),
        vec![(EV_KEY, KEY_A, 0)]
    );
}

#[test]
fn led_output_reports() {
    let k = keyboard();
    assert_eq!(k.led_report(0), Some(vec![0]));
    assert_eq!(k.led_report(1 << LED_NUML), Some(vec![0b001]));
    assert_eq!(
        k.led_report(1 << LED_CAPSL | 1 << LED_SCROLLL),
        Some(vec![0b110])
    );
}

#[test]
fn an_absolute_pointer_with_a_wheel() {
    let d = parse(TABLET).unwrap();
    assert_eq!(applications(&d), [(0, Application::Mouse)]);
    let mut t = Device::new(&d, 0, Application::Mouse);
    let c = t.capabilities();
    assert_eq!(
        c.event_types,
        1 << EV_SYN | 1 << EV_KEY | 1 << EV_REL | 1 << EV_ABS
    );
    assert!(bit(&c.keys, BTN_LEFT) && bit(&c.keys, BTN_MIDDLE) && !bit(&c.keys, BTN_SIDE));
    assert_eq!(c.relative_axes, 1 << REL_WHEEL);
    assert_eq!(
        c.absolute_axes,
        vec![
            Axis {
                code: ABS_X,
                minimum: 0,
                maximum: 0x7FFF
            },
            Axis {
                code: ABS_Y,
                minimum: 0,
                maximum: 0x7FFF
            },
        ]
    );
    assert_eq!(t.led_report(1), None);
    assert_eq!(
        t.decode(&[0b001, 0x00, 0x10, 0x00, 0x20, 0x00]),
        vec![
            (EV_KEY, BTN_LEFT, 1),
            (EV_ABS, ABS_X, 0x1000),
            (EV_ABS, ABS_Y, 0x2000)
        ]
    );
    // Only what changed, and the wheel whenever it moves.
    assert_eq!(
        t.decode(&[0b001, 0x00, 0x10, 0x01, 0x20, 0xFF]),
        vec![(EV_ABS, ABS_Y, 0x2001), (EV_REL, REL_WHEEL, -1)]
    );
    assert_eq!(
        t.decode(&[0, 0x00, 0x10, 0x01, 0x20, 0]),
        vec![(EV_KEY, BTN_LEFT, 0)]
    );
}

#[test]
fn a_relative_mouse_with_report_ids_and_horizontal_scrolling() {
    let d = parse(&[
        0x05, 0x01, 0x09, 0x02, 0xA1, 0x01, 0x85, 0x02, // Mouse, report 2.
        0x05, 0x09, 0x19, 0x01, 0x29, 0x05, 0x15, 0x00, 0x25, 0x01, 0x75, 0x01, 0x95, 0x05, 0x81,
        0x02, 0x75, 0x03, 0x95, 0x01, 0x81, 0x01, // Five buttons and padding.
        0x05, 0x01, 0x09, 0x30, 0x09, 0x31, 0x09, 0x38, 0x15, 0x81, 0x25, 0x7F, 0x75, 0x08, 0x95,
        0x03, 0x81, 0x06, // X, Y and the wheel, relative.
        0x05, 0x0C, 0x0A, 0x38, 0x02, 0x95, 0x01, 0x81, 0x06, // AC Pan.
        0xC0,
    ])
    .unwrap();
    let mut m = Device::new(&d, 0, Application::Mouse);
    let c = m.capabilities();
    assert_eq!(
        c.relative_axes,
        1 << REL_X | 1 << REL_Y | 1 << REL_WHEEL | 1 << REL_HWHEEL
    );
    assert!(bit(&c.keys, BTN_EXTRA));
    assert_eq!(
        m.decode(&[2, 0b10000, 3, 0xFD, 0, 1]),
        vec![
            (EV_KEY, BTN_EXTRA, 1),
            (EV_REL, REL_X, 3),
            (EV_REL, REL_Y, -3),
            (EV_REL, REL_HWHEEL, 1)
        ]
    );
    // Reports of other IDs and empty reports are not this device's.
    assert_eq!(m.decode(&[1, 0xFF, 5, 5, 5, 5]), vec![]);
    assert_eq!(m.decode(&[]), vec![]);
}

#[test]
fn other_applications_are_left_out() {
    // A Consumer Control application.
    let d = parse(&[0x05, 0x0C, 0x09, 0x01, 0xA1, 0x01, 0xC0]).unwrap();
    assert_eq!(applications(&d), []);
}
