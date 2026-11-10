//! The terminal service's logic that needs no system: terminal
//! attributes as the C library defines them, and the line discipline.

#![no_std]

extern crate alloc;

pub mod discipline;
pub mod termios;
#[cfg(test)]
mod termios_tests;
#[cfg(test)]
mod tests;
