//! df, which shows the volumes of the caller's namespace, or those that
//! hold the paths named, with their file systems and space. `table`
//! reads the arguments and writes the volumes, and `program`, left out
//! of the host's build, finds the volumes and asks their file systems.

#![cfg_attr(not(target_os = "mikros"), allow(dead_code))]

#[cfg(target_os = "mikros")]
mod program;
mod table;

#[cfg(target_os = "mikros")]
fn main() -> std::process::ExitCode {
    program::main()
}

#[cfg(not(target_os = "mikros"))]
fn main() {}
