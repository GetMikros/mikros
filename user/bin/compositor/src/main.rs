//! The compositor. `wayland` holds the Wayland protocols it serves: the
//! wire format of their messages on a client's channel, and the code
//! that the build script generates for each interface from the protocol
//! files.

#![cfg_attr(target_os = "mikros", feature(mikros_handles))]
// Nothing on Mikros uses the protocols yet: the compositor does not
// serve clients so far.
#![allow(dead_code)]

mod wayland;

fn main() {}
