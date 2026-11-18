//! dd, which copies blocks from a file or standard input to a file or
//! standard output. `transfer` reads the operands and copies between any
//! reader and writer, and `program`, left out of the host's build, opens
//! the files.

#![cfg_attr(not(target_os = "mikros"), allow(dead_code))]

#[cfg(target_os = "mikros")]
mod program;
mod transfer;

#[cfg(target_os = "mikros")]
fn main() -> std::process::ExitCode {
    program::main()
}

#[cfg(not(target_os = "mikros"))]
fn main() {}
