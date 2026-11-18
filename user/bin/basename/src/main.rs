//! basename, which writes names without the folders before them and,
//! when asked, without a suffix. `names` reads the arguments and works
//! the names out, and `program`, left out of the host's build, writes
//! them.

#![cfg_attr(not(target_os = "mikros"), allow(dead_code))]

mod names;
#[cfg(target_os = "mikros")]
mod program;

#[cfg(target_os = "mikros")]
fn main() -> std::process::ExitCode {
    program::main()
}

#[cfg(not(target_os = "mikros"))]
fn main() {}
