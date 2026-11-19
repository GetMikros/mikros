//! echo, which writes its operands. `echoing` reads the arguments and
//! makes the text, and `program`, left out of the host's build, writes
//! it.

#![cfg_attr(not(target_os = "mikros"), allow(dead_code))]

mod echoing;
#[cfg(target_os = "mikros")]
mod program;

#[cfg(target_os = "mikros")]
fn main() -> std::process::ExitCode {
    program::main()
}

#[cfg(not(target_os = "mikros"))]
fn main() {}
