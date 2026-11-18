//! hostname, which writes the host name. `name` reads the arguments and
//! shortens the name, and `program`, left out of the host's build, gets
//! it from the C library.

#![cfg_attr(not(target_os = "mikros"), allow(dead_code))]

mod name;
#[cfg(target_os = "mikros")]
mod program;

#[cfg(target_os = "mikros")]
fn main() -> std::process::ExitCode {
    program::main()
}

#[cfg(not(target_os = "mikros"))]
fn main() {}
