//! mv, which renames files and folders, and copies and then removes
//! them when the destination is on another file system. `moving` reads
//! the arguments and pairs each source with its destination, and
//! `program`, left out of the host's build, moves them.

#![cfg_attr(not(target_os = "mikros"), allow(dead_code))]

mod moving;
#[cfg(target_os = "mikros")]
mod program;

#[cfg(target_os = "mikros")]
fn main() -> std::process::ExitCode {
    program::main()
}

#[cfg(not(target_os = "mikros"))]
fn main() {}
