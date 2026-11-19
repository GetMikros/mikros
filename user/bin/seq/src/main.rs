//! seq, which prints a sequence of numbers. `sequence` reads the
//! arguments and makes the numbers, with the conversions of `numbers`,
//! and `program`, left out of the host's build, writes them.

#![cfg_attr(not(target_os = "mikros"), allow(dead_code))]

#[cfg(target_os = "mikros")]
mod program;
mod sequence;

#[cfg(target_os = "mikros")]
fn main() -> std::process::ExitCode {
    program::main()
}

#[cfg(not(target_os = "mikros"))]
fn main() {}
