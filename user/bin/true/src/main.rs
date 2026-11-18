//! true, which does nothing and succeeds, whatever its arguments. `command`
//! holds what it takes and when it answers instead, and `program`, left
//! out of the host's build, exits.

#![cfg_attr(not(target_os = "mikros"), allow(dead_code))]

mod command;
#[cfg(target_os = "mikros")]
mod program;

#[cfg(target_os = "mikros")]
fn main() -> std::process::ExitCode {
    program::main()
}

#[cfg(not(target_os = "mikros"))]
fn main() {}
