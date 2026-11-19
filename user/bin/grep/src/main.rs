//! grep, which prints the lines of files that match patterns. `pattern`
//! turns basic and extended regular expressions into the regex crate's
//! syntax and matches lines, `search` reads the arguments and selects
//! and writes lines, and `program`, left out of the host's build, reads
//! the files and walks folders.

#![cfg_attr(not(target_os = "mikros"), allow(dead_code))]

mod pattern;
#[cfg(target_os = "mikros")]
mod program;
mod search;

#[cfg(target_os = "mikros")]
fn main() -> std::process::ExitCode {
    program::main()
}

#[cfg(not(target_os = "mikros"))]
fn main() {}
