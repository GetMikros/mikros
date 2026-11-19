//! tail, which writes the last lines or bytes of files and follows them as
//! they grow. `tailing` reads
//! the arguments and finds the part asked for, and `program`, left out
//! of the host's build, opens and follows the files.

#![cfg_attr(target_os = "mikros", feature(mikros_handles))]
#![cfg_attr(not(target_os = "mikros"), allow(dead_code))]

#[cfg(target_os = "mikros")]
mod program;
mod tailing;

#[cfg(target_os = "mikros")]
fn main() -> std::process::ExitCode {
    program::main()
}

#[cfg(not(target_os = "mikros"))]
fn main() {}
