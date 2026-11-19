//! sort, which sorts lines, or checks that they are sorted. `sorting`
//! reads the arguments and orders lines, and `program`, left out of the
//! host's build, reads and writes the files.

#![cfg_attr(not(target_os = "mikros"), allow(dead_code))]

#[cfg(target_os = "mikros")]
mod program;
mod sorting;

#[cfg(target_os = "mikros")]
fn main() -> std::process::ExitCode {
    program::main()
}

#[cfg(not(target_os = "mikros"))]
fn main() {}
