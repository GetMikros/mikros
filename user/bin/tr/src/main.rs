//! tr, which translates, deletes and squeezes characters. `translating`
//! reads the arguments and the sets and changes the text, and `program`,
//! left out of the host's build, reads standard input.

#![cfg_attr(not(target_os = "mikros"), allow(dead_code))]

#[cfg(target_os = "mikros")]
mod program;
mod translating;

#[cfg(target_os = "mikros")]
fn main() -> std::process::ExitCode {
    program::main()
}

#[cfg(not(target_os = "mikros"))]
fn main() {}
