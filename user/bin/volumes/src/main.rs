//! volumes, which lists the volumes of the device manager with their
//! names in `/volumes`, or ejects or mounts one. `listing` reads the
//! arguments and makes the output, and `program`, left out of the host's
//! build, asks the device manager and `/volumes`.

#![cfg_attr(target_os = "mikros", feature(mikros_handles))]
#![cfg_attr(not(target_os = "mikros"), allow(dead_code))]

mod listing;
#[cfg(target_os = "mikros")]
mod program;

#[cfg(target_os = "mikros")]
fn main() -> std::process::ExitCode {
    program::main()
}

#[cfg(not(target_os = "mikros"))]
fn main() {}
