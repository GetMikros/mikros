//! stat, which shows what Mikros knows of each path: its type, sizes,
//! inode and links, what the caller may do, its owner, its stored mode,
//! its times and its access control lists. `facts` reads the arguments
//! and writes the facts, and `program`, left out of the host's build,
//! gathers them.

#![cfg_attr(target_os = "mikros", feature(mikros_handles))]
#![cfg_attr(not(target_os = "mikros"), allow(dead_code))]

mod facts;
#[cfg(target_os = "mikros")]
mod program;

#[cfg(target_os = "mikros")]
fn main() -> std::process::ExitCode {
    program::main()
}

#[cfg(not(target_os = "mikros"))]
fn main() {}
