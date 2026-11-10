//! PS/2 for the i8042 driver: the keyboard's scan code set 2,
//! independent of how the driver reaches the ports.

#![no_std]

pub mod scancode;
#[cfg(test)]
mod scancode_tests;
