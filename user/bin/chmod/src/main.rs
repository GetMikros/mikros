//! chmod, which sets the stored mode of files and folders, of which only
//! the execute bits have an effect on Mikros. `mode` reads the arguments
//! and works out the new modes, and `program`, left out of the host's
//! build, reads and sets them.

#![cfg_attr(not(target_os = "mikros"), allow(dead_code))]

mod mode;
#[cfg(target_os = "mikros")]
mod program;

#[cfg(target_os = "mikros")]
fn main() -> std::process::ExitCode {
    program::main()
}

#[cfg(not(target_os = "mikros"))]
fn main() {}
