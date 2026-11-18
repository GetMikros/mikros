//! ls, which lists folders and names files, with what the caller may do
//! with each in long lines. `listing` reads the arguments and writes the
//! entries, and `program`, left out of the host's build, reads the
//! folders and the entries' node connections.

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
