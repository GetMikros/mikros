//! The parts of the boot manager that do not depend on UEFI, so that they can
//! be tested on the host.

#![cfg_attr(not(test), no_std)]

extern crate alloc;

pub mod bmp;
pub mod canvas;
pub mod display;
pub mod entries;
pub mod input;
pub mod menu;
pub mod pointer;
pub mod shape;
pub mod text;
