//! The logic of the basic display driver that does not need the system:
//! converting XRGB8888 pixels to the framebuffer's pixel format, and
//! limiting a copy to its damage rectangles.

#![no_std]

extern crate alloc;

pub mod pixels;
pub mod rects;
#[cfg(test)]
mod tests;
