//! PS/2 mice: choosing the protocol with the sample rate sequences, and
//! decoding their packets into evdev events.

use crate::controller::{ACK_TIMEOUT, Controller, Error, Port, Ports};
use alloc::vec::Vec;
use mikros_evdev::codes::*;

const SET_SAMPLE_RATE: u8 = 0xF3;
const GET_ID: u8 = 0xF2;
const ENABLE_REPORTING: u8 = 0xF4;
/// The first byte's bits: the buttons, the bit that is always set, and
/// the signs of X and Y movement.
const ALWAYS_ONE: u8 = 1 << 3;
const X_SIGN: u8 = 1 << 4;
const Y_SIGN: u8 = 1 << 5;
/// The buttons, in the order of their bits: left, right and middle in
/// the first byte, and side and extra in the fourth byte of the
/// five-button protocol.
const BUTTONS: [u16; 5] = [BTN_LEFT, BTN_RIGHT, BTN_MIDDLE, BTN_SIDE, BTN_EXTRA];

/// The protocols, by the device ID that a mouse reports in each.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Protocol {
    Standard = 0,
    Wheel = 3,
    FiveButton = 4,
}

impl Protocol {
    fn packet_len(self) -> usize {
        match self {
            Protocol::Standard => 3,
            Protocol::Wheel | Protocol::FiveButton => 4,
        }
    }

    /// The buttons of a mouse in this protocol.
    pub fn buttons(self) -> &'static [u16] {
        match self {
            Protocol::FiveButton => &BUTTONS,
            Protocol::Standard | Protocol::Wheel => &BUTTONS[..3],
        }
    }

    /// Whether a mouse in this protocol has a wheel.
    pub fn has_wheel(self) -> bool {
        self != Protocol::Standard
    }
}

/// Chooses the protocol of the mouse on the second port: the
/// five-button one if it reports ID 4 after the sample rates 200, 200
/// and 80, otherwise the wheel one if it reports ID 3 after 200, 100 and
/// 80, otherwise the standard one; then enables data reporting.
pub fn setup<P: Ports>(
    controller: &mut Controller<P>,
    other: &mut impl FnMut(Port, u8),
) -> Result<Protocol, Error> {
    let mut protocol = Protocol::Standard;
    for (rates, wanted) in [
        ([200, 200, 80], Protocol::FiveButton),
        ([200, 100, 80], Protocol::Wheel),
    ] {
        for rate in rates {
            controller.send_all(Port::Second, &[SET_SAMPLE_RATE, rate], other)?;
        }
        controller.send(Port::Second, GET_ID, other)?;
        let id = controller.receive(Port::Second, ACK_TIMEOUT, other);
        if id == Some(wanted as u8) {
            protocol = wanted;
            break;
        }
    }
    controller.send(Port::Second, ENABLE_REPORTING, other)?;
    Ok(protocol)
}

/// Decodes the packets of one mouse.
pub struct Decoder {
    protocol: Protocol,
    packet: [u8; 4],
    len: usize,
    /// The buttons held down, bit `n` for `BUTTONS[n]`.
    buttons: u8,
}

impl Decoder {
    pub fn new(protocol: Protocol) -> Decoder {
        Decoder {
            protocol,
            packet: [0; 4],
            len: 0,
            buttons: 0,
        }
    }

    /// Takes in `byte`; when it ends a packet, the packet's events as
    /// (type, code, value), which may be none.
    pub fn feed(&mut self, byte: u8) -> Option<Vec<(u16, u16, i32)>> {
        // A first byte without the bit that is always set is out of
        // step: dropping it makes the next byte the first.
        if self.len == 0 && byte & ALWAYS_ONE == 0 {
            return None;
        }
        self.packet[self.len] = byte;
        self.len += 1;
        if self.len < self.protocol.packet_len() {
            return None;
        }
        self.len = 0;
        Some(self.events())
    }

    fn events(&mut self) -> Vec<(u16, u16, i32)> {
        let [first, x, y, extra] = self.packet;
        let mut buttons = first & 0b111;
        let mut wheel = 0;
        match self.protocol {
            Protocol::Standard => {}
            Protocol::Wheel => wheel = i32::from(extra as i8),
            Protocol::FiveButton => {
                // A four-bit signed wheel value, and the fourth and fifth
                // buttons above it.
                wheel = i32::from(((extra << 4) as i8) >> 4);
                buttons |= (extra >> 4 & 0b11) << 3;
            }
        }
        let signed =
            |value: u8, sign: u8| i32::from(value) - if first & sign != 0 { 256 } else { 0 };
        let dx = signed(x, X_SIGN);
        // The mouse counts upwards as positive.
        let dy = -signed(y, Y_SIGN);
        let mut events = Vec::new();
        for (n, &button) in self.protocol.buttons().iter().enumerate() {
            let now = buttons & (1 << n) != 0;
            if now != (self.buttons & (1 << n) != 0) {
                events.push((EV_KEY, button, i32::from(now)));
            }
        }
        self.buttons = buttons;
        if dx != 0 {
            events.push((EV_REL, REL_X, dx));
        }
        if dy != 0 {
            events.push((EV_REL, REL_Y, dy));
        }
        // The mouse counts turning towards the user as positive.
        if wheel != 0 {
            events.push((EV_REL, REL_WHEEL, -wheel));
        }
        events
    }
}
