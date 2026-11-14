//! sudo, which runs a command as an authorized administrator: it asks
//! the session manager to elevate it, then replaces itself with the
//! command, which keeps its process record and so the elevation.
//! `request` reads the arguments, and `program`, left out of the host's
//! build, asks and runs the command.

#![cfg_attr(target_os = "mikros", feature(mikros_handles))]
#![cfg_attr(not(target_os = "mikros"), allow(dead_code))]

#[cfg(target_os = "mikros")]
mod program;
mod request;

#[cfg(target_os = "mikros")]
fn main() -> std::process::ExitCode {
    program::main()
}

#[cfg(not(target_os = "mikros"))]
fn main() {}
