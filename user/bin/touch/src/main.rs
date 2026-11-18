//! touch, which creates files and sets their access and modified times.
//! `stamps` reads the arguments and the dates and stamps given, and
//! `program`, left out of the host's build, creates the files and sets
//! the times.

#![cfg_attr(not(target_os = "mikros"), allow(dead_code))]

#[cfg(target_os = "mikros")]
mod program;
mod stamps;

#[cfg(target_os = "mikros")]
fn main() -> std::process::ExitCode {
    program::main()
}

#[cfg(not(target_os = "mikros"))]
fn main() {}
