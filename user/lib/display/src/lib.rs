//! The logic of display drivers that does not need the system: for the
//! basic display driver, converting XRGB8888 pixels to the framebuffer's
//! pixel format and limiting a copy to its damage rectangles, and for
//! every display driver, drawing text in the built-in bitmap font that
//! the gpudev library shows the log with.

#![no_std]

extern crate alloc;

pub mod font;
pub mod pixels;
pub mod rects;
#[cfg(test)]
mod tests;
pub mod text;
