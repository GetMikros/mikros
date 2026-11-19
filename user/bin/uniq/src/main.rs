//! uniq, which merges adjacent equal lines. `merging` reads the
//! arguments and compares lines, and `program`, left out of the host's
//! build, reads and writes the files.

#![cfg_attr(not(target_os = "mikros"), allow(dead_code))]

mod merging;
#[cfg(target_os = "mikros")]
mod program;

#[cfg(target_os = "mikros")]
fn main() -> std::process::ExitCode {
    program::main()
}

#[cfg(not(target_os = "mikros"))]
fn main() {}
