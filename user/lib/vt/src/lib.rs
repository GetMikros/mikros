//! The terminal emulator of the text console, independent of how it is
//! drawn: parsing what programs write, the screen it changes, and the
//! bytes that keys send.

#![no_std]

extern crate alloc;

pub mod cell;
pub mod parser;
#[cfg(test)]
mod parser_tests;
pub mod sgr;
