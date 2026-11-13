//! The controller and the devices on its ports: reading the bytes they
//! send and turning them into events.

use crate::driver::ports::{IoPorts, now};
use crate::ps2::controller::{Controller, Port};
use crate::ps2::keyboard_leds;
use crate::ps2::mouse;
use crate::ps2::scancode::Decoder;
use alloc::rc::{Rc, Weak};
use alloc::vec::Vec;
use core::cell::{OnceCell, RefCell};
use mikros_evdev::codes::EV_KEY;
use mikros_evdev::{Event, group};
use mikros_inputdev::{Backend, InputDevice};
use mikros_rt::interfaces::mikros::base::Status;

/// The most bytes read for one interrupt, so that a device that never
/// stops sending cannot hold the driver.
const DRAIN_MAX: usize = 256;

pub struct Hub {
    pub controller: RefCell<Controller<IoPorts>>,
    decoder: RefCell<Decoder>,
    pub keyboard: OnceCell<Weak<InputDevice<KeyboardLeds>>>,
    /// The mouse's packet decoder and device, once it is set up.
    pub mouse: OnceCell<(RefCell<mouse::Decoder>, Weak<InputDevice<NoLeds>>)>,
}

impl Hub {
    pub fn new(controller: Controller<IoPorts>) -> Hub {
        Hub {
            controller: RefCell::new(controller),
            decoder: RefCell::new(Decoder::new()),
            keyboard: OnceCell::new(),
            mouse: OnceCell::new(),
        }
    }

    /// Reads what the devices sent.
    pub fn drain(&self) {
        for _ in 0..DRAIN_MAX {
            let polled = self.controller.borrow_mut().poll();
            let Some((port, byte)) = polled else {
                return;
            };
            self.byte(port, byte, now());
        }
    }

    /// Takes in `byte` from `port`, received at `timestamp`.
    pub fn byte(&self, port: Port, byte: u8, timestamp: i64) {
        if port == Port::Second {
            self.mouse_byte(byte, timestamp);
            return;
        }
        let Some(keyboard) = self.keyboard.get().and_then(Weak::upgrade) else {
            return;
        };
        let mut changes = Vec::new();
        self.decoder
            .borrow_mut()
            .feed(byte, |change| changes.push(change));
        // Each change is a report of its own, so that the Pause key's
        // press and release are not taken as simultaneous.
        for (key, pressed) in changes {
            let event = Event::new(timestamp, EV_KEY, key, i32::from(pressed));
            keyboard.report(&group(timestamp, [event]));
        }
    }
}

impl Hub {
    fn mouse_byte(&self, byte: u8, timestamp: i64) {
        let Some((decoder, device)) = self.mouse.get() else {
            return;
        };
        let Some(events) = decoder.borrow_mut().feed(byte) else {
            return;
        };
        let Some(device) = device.upgrade() else {
            return;
        };
        if !events.is_empty() {
            let events = events
                .into_iter()
                .map(|(kind, code, value)| Event::new(timestamp, kind, code, value));
            device.report(&group(timestamp, events));
        }
    }
}

/// A device without LEDs.
pub struct NoLeds;

impl Backend for NoLeds {
    async fn set_leds(&self, _leds: u16) -> Result<(), Status> {
        Err(Status::NotSupported)
    }
}

/// Sets the keyboard's LEDs.
pub struct KeyboardLeds(pub Rc<Hub>);

impl Backend for KeyboardLeds {
    async fn set_leds(&self, leds: u16) -> Result<(), Status> {
        let mut stray = Vec::new();
        let sent = {
            let mut controller = self.0.controller.borrow_mut();
            keyboard_leds(&mut controller, leds, &mut |port, byte| {
                stray.push((port, byte, now()))
            })
        };
        for (port, byte, timestamp) in stray {
            self.0.byte(port, byte, timestamp);
        }
        sent.map_err(|_| Status::Io)
    }
}
