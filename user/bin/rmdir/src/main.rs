//! rmdir, which removes empty folders, and with -p the folders above
//! them that the path names. `parents` reads the arguments and lists the
//! folders to remove, and `program`, left out of the host's build, removes
//! them.

#![cfg_attr(not(target_os = "mikros"), allow(dead_code))]

mod parents;
#[cfg(target_os = "mikros")]
mod program;

#[cfg(target_os = "mikros")]
fn main() -> std::process::ExitCode {
    program::main()
}

#[cfg(not(target_os = "mikros"))]
fn main() {}
