//! permissions, which lists the users' decisions on permissions, and
//! allows or denies a permission to an app. `listing` reads the arguments
//! and makes the output, and `program`, left out of the host's build,
//! asks for the authorization and calls the permission broker.

#![cfg_attr(target_os = "mikros", feature(mikros_handles))]
#![cfg_attr(not(target_os = "mikros"), allow(dead_code))]

mod listing;
#[cfg(target_os = "mikros")]
mod program;

#[cfg(target_os = "mikros")]
fn main() -> std::process::ExitCode {
    program::main()
}

#[cfg(not(target_os = "mikros"))]
fn main() {}
