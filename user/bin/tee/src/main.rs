//! tee, which copies standard input to standard output and files.
//! `copying` reads the arguments and copies to every output, and
//! `program`, left out of the host's build, opens the files.

#![cfg_attr(not(target_os = "mikros"), allow(dead_code))]

mod copying;
#[cfg(target_os = "mikros")]
mod program;

#[cfg(target_os = "mikros")]
fn main() -> std::process::ExitCode {
    program::main()
}

#[cfg(not(target_os = "mikros"))]
fn main() {}
