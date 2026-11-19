//! head, which writes the first lines or bytes of files. `heading` reads
//! the arguments and copies the part asked for, and `program`, left out
//! of the host's build, opens the files.

#![cfg_attr(not(target_os = "mikros"), allow(dead_code))]

mod heading;
#[cfg(target_os = "mikros")]
mod program;

#[cfg(target_os = "mikros")]
fn main() -> std::process::ExitCode {
    program::main()
}

#[cfg(not(target_os = "mikros"))]
fn main() {}
