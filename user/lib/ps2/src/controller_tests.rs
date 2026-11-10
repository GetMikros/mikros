extern crate std;

use crate::controller::*;
use crate::*;
use mikros_evdev::codes::*;
use std::collections::VecDeque;
use std::vec;
use std::vec::Vec;

/// A controller with a keyboard on its first port and, when `mouse` is
/// set, a mouse with that ID on its second.
pub(crate) struct Fake {
    config: u8,
    output: VecDeque<(Port, u8)>,
    expect_config: bool,
    to_second: bool,
    mouse: Option<u8>,
    /// How many more device writes are answered with a resend.
    resends: usize,
    /// The device bytes written, by port.
    pub(crate) written: Vec<(Port, u8)>,
    commands: Vec<u8>,
    clock: i64,
}

impl Fake {
    pub(crate) fn new(mouse: Option<u8>) -> Fake {
        Fake {
            // Interrupts, translation and the second port's clock off.
            config: 0x01 | 0x02 | 0x40 | 0x20,
            output: VecDeque::new(),
            expect_config: false,
            to_second: false,
            mouse,
            resends: 0,
            written: Vec::new(),
            commands: Vec::new(),
            clock: 0,
        }
    }

    fn answer(&mut self, port: Port, bytes: &[u8]) {
        self.output.extend(bytes.iter().map(|&b| (port, b)));
    }
}

impl Ports for Fake {
    fn status(&mut self) -> u8 {
        match self.output.front() {
            Some((Port::Second, _)) => 0x21,
            Some((Port::First, _)) => 0x01,
            None => 0,
        }
    }

    fn command(&mut self, command: u8) {
        self.commands.push(command);
        match command {
            0x20 => self.output.push_back((Port::First, self.config)),
            0x60 => self.expect_config = true,
            0xAA => self.output.push_back((Port::First, 0x55)),
            0xAB => self.output.push_back((Port::First, 0)),
            0xA9 => {
                let answer = if self.mouse.is_some() { 0 } else { 1 };
                self.output.push_back((Port::First, answer));
            }
            0xD4 => self.to_second = true,
            _ => {}
        }
    }

    fn read(&mut self) -> u8 {
        self.output.pop_front().map_or(0, |(_, b)| b)
    }

    fn write(&mut self, data: u8) {
        if self.expect_config {
            self.expect_config = false;
            self.config = data;
            return;
        }
        let port = if core::mem::take(&mut self.to_second) {
            Port::Second
        } else {
            Port::First
        };
        self.written.push((port, data));
        if self.resends > 0 {
            self.resends -= 1;
            self.answer(port, &[RESEND]);
            return;
        }
        match (port, data) {
            (Port::First, RESET) => self.answer(port, &[ACK, 0xAA]),
            (Port::Second, RESET) => self.answer(port, &[ACK, 0xAA, 0x00]),
            (Port::Second, 0xF2) => {
                let id = self.mouse.unwrap_or(0);
                self.answer(port, &[ACK, id]);
            }
            _ => self.answer(port, &[ACK]),
        }
    }

    fn now(&mut self) -> i64 {
        self.clock += 1_000_000;
        self.clock
    }
}

fn ignore(_: Port, _: u8) {}

#[test]
fn init_follows_the_order_and_finds_both_devices() {
    let mut c = Controller::new(Fake::new(Some(0)));
    let mut stray = Vec::new();
    let present = c.init(&mut |p, b| stray.push((p, b))).unwrap();
    assert_eq!(
        present,
        Present {
            first: true,
            second: true
        }
    );
    c.enable_interrupts(present).unwrap();
    let fake = c.ports();
    assert_eq!(
        fake.commands,
        vec![
            0xAD, 0xA7, 0x20, 0x60, 0xAA, 0x60, 0xAB, 0xA9, 0xAE, 0xA8, 0xD4, 0x60
        ]
    );
    // Interrupts on, translation off.
    assert_eq!(fake.config & 0x43, 0x03);
    assert_eq!(
        fake.written,
        vec![(Port::First, RESET), (Port::Second, RESET)]
    );
    // The mouse's ID after its reset was read.
    assert!(stray.is_empty());
    assert!(fake.output.is_empty());
}

#[test]
fn a_controller_without_a_second_port_skips_it() {
    let mut fake = Fake::new(None);
    fake.config &= !0x20;
    let mut c = Controller::new(fake);
    let present = c.init(&mut ignore).unwrap();
    assert_eq!(
        present,
        Present {
            first: true,
            second: false
        }
    );
    assert!(!c.ports().commands.contains(&0xA9));
}

#[test]
fn a_failed_self_test_is_an_error() {
    struct Broken(Fake);
    impl Ports for Broken {
        fn status(&mut self) -> u8 {
            self.0.status()
        }
        fn command(&mut self, c: u8) {
            self.0.command(c);
            if c == 0xAA {
                self.0.output.pop_back();
                self.0.output.push_back((Port::First, 0xFC));
            }
        }
        fn read(&mut self) -> u8 {
            self.0.read()
        }
        fn write(&mut self, d: u8) {
            self.0.write(d)
        }
        fn now(&mut self) -> i64 {
            self.0.now()
        }
    }
    let mut c = Controller::new(Broken(Fake::new(None)));
    assert_eq!(c.init(&mut ignore), Err(Error::SelfTest(0xFC)));
}

#[test]
fn a_command_is_resent_once() {
    let mut fake = Fake::new(None);
    fake.resends = 1;
    let mut c = Controller::new(fake);
    c.send(Port::First, 0xF4, &mut ignore).unwrap();
    assert_eq!(c.ports().written.len(), 2);
    c.ports().resends = 2;
    assert_eq!(c.send(Port::First, 0xF4, &mut ignore), Err(Error::NoAck));
}

#[test]
fn a_missing_ack_times_out() {
    struct Silent(i64);
    impl Ports for Silent {
        fn status(&mut self) -> u8 {
            0
        }
        fn command(&mut self, _: u8) {}
        fn read(&mut self) -> u8 {
            0
        }
        fn write(&mut self, _: u8) {}
        fn now(&mut self) -> i64 {
            self.0 += 1_000_000;
            self.0
        }
    }
    let mut c = Controller::new(Silent(0));
    assert_eq!(c.send(Port::First, 0xF4, &mut ignore), Err(Error::NoAck));
    // About 100 ms passed, at 1 ms per look at the clock.
    let waited = c.ports().0;
    assert!((100_000_000..110_000_000).contains(&waited), "{waited}");
}

#[test]
fn keyboard_bytes_before_an_ack_go_elsewhere() {
    let mut fake = Fake::new(None);
    fake.answer(Port::First, &[0x1C]);
    let mut c = Controller::new(fake);
    let mut stray = Vec::new();
    // The `A` make code and a mouse byte arrive before the ACK; neither
    // is the answer.
    c.ports().output.push_back((Port::Second, 0x08));
    keyboard_leds(&mut c, 1 << LED_CAPSL, &mut |p, b| stray.push((p, b))).unwrap();
    assert_eq!(
        c.ports().written,
        vec![(Port::First, 0xED), (Port::First, 0x04)]
    );
    assert_eq!(stray, vec![(Port::First, 0x1C), (Port::Second, 0x08)]);
}

#[test]
fn led_bits_map_to_the_keyboard_command() {
    assert_eq!(led_byte(0), 0);
    assert_eq!(led_byte(1 << LED_SCROLLL), 1);
    assert_eq!(led_byte(1 << LED_NUML), 2);
    assert_eq!(led_byte(1 << LED_CAPSL), 4);
    assert_eq!(led_byte(KEYBOARD_LEDS), 7);
}
