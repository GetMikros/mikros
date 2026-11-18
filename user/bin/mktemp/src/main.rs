//! mktemp, which makes a file or folder of a new name from a template
//! and writes its path. `template` reads the arguments and makes names,
//! and `program`, left out of the host's build, creates them.

#![cfg_attr(not(target_os = "mikros"), allow(dead_code))]

#[cfg(target_os = "mikros")]
mod program;
mod template;

#[cfg(target_os = "mikros")]
fn main() -> std::process::ExitCode {
    program::main()
}

#[cfg(not(target_os = "mikros"))]
fn main() {}
