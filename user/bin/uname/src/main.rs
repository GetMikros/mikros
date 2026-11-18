//! uname, which writes what system this is. `system` reads the
//! arguments and writes the fields, and `program`, left out of the
//! host's build, gets the host name and the booted version.

#![cfg_attr(target_os = "mikros", feature(mikros_handles))]
#![cfg_attr(not(target_os = "mikros"), allow(dead_code))]

#[cfg(target_os = "mikros")]
mod program;
mod system;

#[cfg(target_os = "mikros")]
fn main() -> std::process::ExitCode {
    program::main()
}

#[cfg(not(target_os = "mikros"))]
fn main() {}
