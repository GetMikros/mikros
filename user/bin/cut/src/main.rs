//! cut, which writes the selected bytes, characters or fields of each
//! line. `cutting` reads the arguments and cuts lines, and `program`,
//! left out of the host's build, reads the files.

#![cfg_attr(not(target_os = "mikros"), allow(dead_code))]

mod cutting;
#[cfg(target_os = "mikros")]
mod program;

#[cfg(target_os = "mikros")]
fn main() -> std::process::ExitCode {
    program::main()
}

#[cfg(not(target_os = "mikros"))]
fn main() {}
