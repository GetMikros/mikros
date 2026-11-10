//! The text console: the full-screen terminal that runs when no desktop
//! does. So far it has its drawing, with the renderer, the monospaced
//! font and the colour palette, and its keyboard, with the keymap and key
//! repeat, which the main loop that follows uses.

#![feature(mikros_handles)]
// Used once the console's main loop follows.
#![allow(dead_code)]

mod font;
mod keyboard;
mod palette;
mod process;
mod render;
mod xkb;

fn main() {}
