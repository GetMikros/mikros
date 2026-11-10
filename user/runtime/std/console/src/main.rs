//! The text console: the full-screen terminal that runs when no desktop
//! does. So far it has its drawing: the renderer, the monospaced font
//! and the colour palette, which the parts that follow use.

#![feature(mikros_handles)]
// Used once the console's main loop follows.
#![allow(dead_code)]

mod font;
mod palette;
mod process;
mod render;

fn main() {}
