//! The greeter, the login screen that the compositor starts in greeter
//! mode (MIKSPEC-0077.R10, MIKSPEC-0075.R14): it lists the users, asks for
//! a password and logs the user in through the `Login` protocol
//! (MIKSPEC-0075.R30), and offers to restart and to shut down.

#![cfg_attr(target_os = "mikros", feature(mikros_handles))]
#![cfg_attr(not(target_os = "mikros"), allow(dead_code))]

mod model;
#[cfg(target_os = "mikros")]
mod program;

#[cfg(target_os = "mikros")]
fn main() -> std::process::ExitCode {
    program::run()
}

#[cfg(not(target_os = "mikros"))]
fn main() {}
