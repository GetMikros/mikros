//! The terminal service's logic that needs no system: terminal
//! attributes as the C library defines them.

#![no_std]

pub mod termios;
#[cfg(test)]
mod termios_tests;
