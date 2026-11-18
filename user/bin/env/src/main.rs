//! env, which runs a command with the environment changed, or writes the
//! environment without one. `changes` reads the arguments and works out
//! the environment, and `program`, left out of the host's build, writes
//! it or runs the command.

#![cfg_attr(not(target_os = "mikros"), allow(dead_code))]

mod changes;
#[cfg(target_os = "mikros")]
mod program;

#[cfg(target_os = "mikros")]
fn main() -> std::process::ExitCode {
    program::main()
}

#[cfg(not(target_os = "mikros"))]
fn main() {}
