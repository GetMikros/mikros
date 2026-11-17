//! A program that checks installs for every user on Mikros, which the
//! setup run's administrator runs once and allows with her password:
//! the service of a global package, a driver package, and an old
//! library version that an installed program still needs. It prints the
//! check that failed and exits with its number, and with 0 when all
//! passed.

#![cfg_attr(target_os = "mikros", feature(mikros_handles))]
#![cfg_attr(not(target_os = "mikros"), allow(dead_code))]

#[cfg(target_os = "mikros")]
mod program;

#[cfg(target_os = "mikros")]
fn main() -> std::process::ExitCode {
    program::main()
}

#[cfg(not(target_os = "mikros"))]
fn main() {}
