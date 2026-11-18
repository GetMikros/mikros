//! pwd, which writes the current folder. `folder` reads the arguments
//! and checks `$PWD`, and `program`, left out of the host's build, finds
//! the folder and writes it.

#![cfg_attr(not(target_os = "mikros"), allow(dead_code))]

mod folder;
#[cfg(target_os = "mikros")]
mod program;

#[cfg(target_os = "mikros")]
fn main() -> std::process::ExitCode {
    program::main()
}

#[cfg(not(target_os = "mikros"))]
fn main() {}
