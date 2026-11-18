//! realpath, which writes paths made absolute with their links
//! resolved. `paths` reads the arguments and resolves the paths, and
//! `program`, left out of the host's build, writes them.

#![cfg_attr(not(target_os = "mikros"), allow(dead_code))]

mod paths;
#[cfg(target_os = "mikros")]
mod program;

#[cfg(target_os = "mikros")]
fn main() -> std::process::ExitCode {
    program::main()
}

#[cfg(not(target_os = "mikros"))]
fn main() {}
