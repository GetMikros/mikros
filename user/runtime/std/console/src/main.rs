//! The text console: the full-screen terminal that runs when no desktop
//! does. So far it has its drawing, with the renderer, the monospaced
//! font and the colour palette, its keyboard, with the keymap and key
//! repeat, and the terminals of the terminal service that it shows,
//! which the main loop that follows uses.

#![feature(mikros_handles)]
// Used once the console's main loop follows.
#![allow(dead_code)]

mod font;
mod keyboard;
mod notify;
mod palette;
mod process;
mod render;
mod term;
mod xkb;

fn main() {}
