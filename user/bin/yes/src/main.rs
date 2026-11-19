//! yes, which writes a line again and again. `repeating` reads the
//! arguments and makes the line, and `program`, left out of the host's
//! build, writes it until writing fails.

#![cfg_attr(not(target_os = "mikros"), allow(dead_code))]

#[cfg(target_os = "mikros")]
mod program;
mod repeating;

#[cfg(target_os = "mikros")]
fn main() -> std::process::ExitCode {
    program::main()
}

#[cfg(not(target_os = "mikros"))]
fn main() {}
