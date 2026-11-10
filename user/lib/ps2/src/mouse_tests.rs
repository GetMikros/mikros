extern crate std;

use crate::controller::Controller;
use crate::controller_tests::Fake;
use crate::mouse::{Decoder, Protocol, setup};
use mikros_evdev::codes::*;
use std::vec;
use std::vec::Vec;

fn written_to_mouse(c: &mut Controller<Fake>) -> Vec<u8> {
    c.ports()
        .written
        .iter()
        .filter(|(p, _)| *p == crate::controller::Port::Second)
        .map(|&(_, b)| b)
        .collect()
}

#[test]
fn a_five_button_mouse_stops_after_the_first_sequence() {
    let mut c = Controller::new(Fake::new(Some(4)));
    assert_eq!(setup(&mut c, &mut |_, _| {}), Ok(Protocol::FiveButton));
    assert_eq!(
        written_to_mouse(&mut c),
        vec![0xF3, 200, 0xF3, 200, 0xF3, 80, 0xF2, 0xF4]
    );
}

#[test]
fn a_wheel_mouse_gets_the_second_sequence() {
    let mut c = Controller::new(Fake::new(Some(3)));
    assert_eq!(setup(&mut c, &mut |_, _| {}), Ok(Protocol::Wheel));
    assert_eq!(
        written_to_mouse(&mut c),
        vec![
            0xF3, 200, 0xF3, 200, 0xF3, 80, 0xF2, 0xF3, 200, 0xF3, 100, 0xF3, 80, 0xF2, 0xF4
        ]
    );
}

#[test]
fn a_plain_mouse_keeps_the_standard_protocol() {
    let mut c = Controller::new(Fake::new(Some(0)));
    assert_eq!(setup(&mut c, &mut |_, _| {}), Ok(Protocol::Standard));
    assert_eq!(written_to_mouse(&mut c).last(), Some(&0xF4));
}

fn feed(d: &mut Decoder, bytes: &[u8]) -> Vec<Vec<(u16, u16, i32)>> {
    bytes.iter().filter_map(|&b| d.feed(b)).collect()
}

#[test]
fn standard_packets() {
    let mut d = Decoder::new(Protocol::Standard);
    // Left button down, 5 to the right, 3 up.
    assert_eq!(
        feed(&mut d, &[0x09, 5, 3]),
        vec![vec![
            (EV_KEY, BTN_LEFT, 1),
            (EV_REL, REL_X, 5),
            (EV_REL, REL_Y, -3)
        ]]
    );
    // Left up, right down, 2 to the left, 4 down.
    assert_eq!(
        feed(&mut d, &[0x08 | 0x02 | 0x10 | 0x20, 0xFE, 0xFC]),
        vec![vec![
            (EV_KEY, BTN_LEFT, 0),
            (EV_KEY, BTN_RIGHT, 1),
            (EV_REL, REL_X, -2),
            (EV_REL, REL_Y, 4),
        ]]
    );
    // Nothing changed.
    assert_eq!(feed(&mut d, &[0x0A, 0, 0]), vec![vec![]]);
}

#[test]
fn wheel_packets() {
    let mut d = Decoder::new(Protocol::Wheel);
    // Turning towards the user is negative in evdev.
    assert_eq!(
        feed(&mut d, &[0x08, 0, 0, 1]),
        vec![vec![(EV_REL, REL_WHEEL, -1)]]
    );
    assert_eq!(
        feed(&mut d, &[0x08, 0, 0, 0xFF]),
        vec![vec![(EV_REL, REL_WHEEL, 1)]]
    );
}

#[test]
fn five_button_packets() {
    let mut d = Decoder::new(Protocol::FiveButton);
    assert_eq!(
        feed(&mut d, &[0x08, 0, 0, 0x10 | 0x0F]),
        vec![vec![(EV_KEY, BTN_SIDE, 1), (EV_REL, REL_WHEEL, 1)]]
    );
    assert_eq!(
        feed(&mut d, &[0x08, 0, 0, 0x20 | 0x02]),
        vec![vec![
            (EV_KEY, BTN_SIDE, 0),
            (EV_KEY, BTN_EXTRA, 1),
            (EV_REL, REL_WHEEL, -2),
        ]]
    );
}

#[test]
fn out_of_step_bytes_are_dropped_until_a_first_byte() {
    let mut d = Decoder::new(Protocol::Standard);
    // Two bytes without the always-one bit, then a whole packet.
    assert_eq!(
        feed(&mut d, &[0x05, 0x00, 0x08, 1, 0]),
        vec![vec![(EV_REL, REL_X, 1)]]
    );
}
