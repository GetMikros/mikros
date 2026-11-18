//! du, which shows the space that files and folders use. `space` reads
//! the arguments and writes the sizes, and `program`, left out of the
//! host's build, walks the folders.

#![cfg_attr(not(target_os = "mikros"), allow(dead_code))]

#[cfg(target_os = "mikros")]
mod program;
mod space;

#[cfg(target_os = "mikros")]
fn main() -> std::process::ExitCode {
    program::main()
}

#[cfg(not(target_os = "mikros"))]
fn main() {}
