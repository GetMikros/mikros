//! cat, which copies files, or its standard input, to its standard
//! output. `showing` reads the arguments and numbers, squeezes and shows
//! the lines, and `program`, left out of the host's build, reads the
//! files.

#![cfg_attr(not(target_os = "mikros"), allow(dead_code))]

#[cfg(target_os = "mikros")]
mod program;
mod showing;

#[cfg(target_os = "mikros")]
fn main() -> std::process::ExitCode {
    program::main()
}

#[cfg(not(target_os = "mikros"))]
fn main() {}
