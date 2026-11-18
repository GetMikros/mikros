//! rm, which removes files, and folders with what they hold, and refuses
//! to remove the root and the read-only system version. `removal` reads
//! the arguments and decides what is refused and asked, and `program`,
//! left out of the host's build, removes the entries.

#![cfg_attr(not(target_os = "mikros"), allow(dead_code))]

#[cfg(target_os = "mikros")]
mod program;
mod removal;

#[cfg(target_os = "mikros")]
fn main() -> std::process::ExitCode {
    program::main()
}

#[cfg(not(target_os = "mikros"))]
fn main() {}
