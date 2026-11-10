//! PS/2 for the i8042 driver: the controller, the keyboard's scan code
//! set 2 and LEDs, independent of how the driver reaches the ports.

#![no_std]

pub mod controller;
pub mod scancode;
#[cfg(test)]
mod controller_tests;
#[cfg(test)]
mod scancode_tests;

use controller::{Controller, Error, Port, Ports};
use mikros_evdev::codes::{LED_CAPSL, LED_NUML, LED_SCROLLL};

/// Keyboard commands: setting the LEDs, and choosing a scan code set.
const SET_LEDS: u8 = 0xED;
const SCAN_CODE_SET: u8 = 0xF0;
const SET_2: u8 = 2;
/// The LED bits of `SET_LEDS`.
const SCROLL_LOCK: u8 = 1 << 0;
const NUM_LOCK: u8 = 1 << 1;
const CAPS_LOCK: u8 = 1 << 2;

/// The LEDs a keyboard has, as evdev LED bits.
pub const KEYBOARD_LEDS: u16 = 1 << LED_NUML | 1 << LED_CAPSL | 1 << LED_SCROLLL;

/// Puts the keyboard on the first port in scan code set 2.
pub fn keyboard_set_2<P: Ports>(
    controller: &mut Controller<P>,
    other: &mut impl FnMut(Port, u8),
) -> Result<(), Error> {
    controller.send_all(Port::First, &[SCAN_CODE_SET, SET_2], other)
}

/// Lights the keyboard's LEDs of the evdev LED bits `leds`.
pub fn keyboard_leds<P: Ports>(
    controller: &mut Controller<P>,
    leds: u16,
    other: &mut impl FnMut(Port, u8),
) -> Result<(), Error> {
    controller.send_all(Port::First, &[SET_LEDS, led_byte(leds)], other)
}

/// The `SET_LEDS` byte of the evdev LED bits `leds`.
pub fn led_byte(leds: u16) -> u8 {
    let lit = |code: u16, bit: u8| if leds & (1 << code) != 0 { bit } else { 0 };
    lit(LED_SCROLLL, SCROLL_LOCK) | lit(LED_NUML, NUM_LOCK) | lit(LED_CAPSL, CAPS_LOCK)
}
