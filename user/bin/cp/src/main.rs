//! cp, which copies files, and folders with what they hold, or makes
//! links to them. `copying` reads the arguments and pairs each source
//! with its destination, and `program`, left out of the host's build,
//! copies through the standard library and the node connections.

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
