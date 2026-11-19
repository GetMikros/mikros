//! wc, which counts the lines, words, characters and bytes of files.
//! `counting` reads the arguments, counts and writes the counts, and
//! `program`, left out of the host's build, reads the files.

#![cfg_attr(not(target_os = "mikros"), allow(dead_code))]

mod counting;
#[cfg(target_os = "mikros")]
mod program;

#[cfg(target_os = "mikros")]
fn main() -> std::process::ExitCode {
    program::main()
}

#[cfg(not(target_os = "mikros"))]
fn main() {}
