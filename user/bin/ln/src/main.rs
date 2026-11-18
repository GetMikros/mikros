//! ln, which creates hard links, and symbolic links with -s. `linking`
//! reads the arguments and works out each link's target and name, and
//! `program`, left out of the host's build, creates the links.

#![cfg_attr(not(target_os = "mikros"), allow(dead_code))]

mod linking;
#[cfg(target_os = "mikros")]
mod program;

#[cfg(target_os = "mikros")]
fn main() -> std::process::ExitCode {
    program::main()
}

#[cfg(not(target_os = "mikros"))]
fn main() {}
