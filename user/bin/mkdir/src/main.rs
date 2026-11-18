//! mkdir, which creates folders, with the folders above them with -p
//! and a mode with -m. `making` reads the arguments and modes and lists
//! the folders to create, and `program`, left out of the host's build,
//! creates them.

#![cfg_attr(not(target_os = "mikros"), allow(dead_code))]

mod making;
#[cfg(target_os = "mikros")]
mod program;

#[cfg(target_os = "mikros")]
fn main() -> std::process::ExitCode {
    program::main()
}

#[cfg(not(target_os = "mikros"))]
fn main() {}
