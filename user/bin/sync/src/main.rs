//! sync, which has file systems write what they hold of files and
//! folders to their storage. `targets` says what it takes, and
//! `program`, left out of the host's build, finds what to sync and asks
//! for it.

#![cfg_attr(not(target_os = "mikros"), allow(dead_code))]

#[cfg(target_os = "mikros")]
mod program;
mod targets;

#[cfg(target_os = "mikros")]
fn main() -> std::process::ExitCode {
    program::main()
}

#[cfg(not(target_os = "mikros"))]
fn main() {}
