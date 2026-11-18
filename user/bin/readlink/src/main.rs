//! readlink, which writes the targets of links, or with -f, -e or -m
//! paths made absolute with their links resolved. `links` reads the
//! arguments and resolves paths, and `program`, left out of the host's
//! build, reads the links and writes them.

#![cfg_attr(not(target_os = "mikros"), allow(dead_code))]

mod links;
#[cfg(target_os = "mikros")]
mod program;

#[cfg(target_os = "mikros")]
fn main() -> std::process::ExitCode {
    program::main()
}

#[cfg(not(target_os = "mikros"))]
fn main() {}
