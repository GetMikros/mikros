//! The compositor. `wayland` holds the Wayland protocols it serves: the
//! wire format of their messages on a client's channel, and the code
//! that the build script generates for each interface from the protocol
//! files. `server` holds its side of the clients' connections, `theme`
//! the desktop's theme from its files, `keyboard` the keyboard layout
//! from its file, `haze` the blurred copy of a
//! wallpaper, and `program`, which only runs on Mikros, shows their
//! surfaces on the display and serves their channels.

#![cfg_attr(target_os = "mikros", feature(mikros_handles))]
// The program does not use every part of the protocols and the server
// yet, and the host builds only the tests.
#![allow(dead_code)]

mod accessibility;
mod haze;
mod keyboard;
#[cfg(target_os = "mikros")]
mod program;
mod restarts;
mod server;
mod theme;
mod wayland;

#[cfg(target_os = "mikros")]
fn main() {
    program::main();
}

#[cfg(not(target_os = "mikros"))]
fn main() {}
