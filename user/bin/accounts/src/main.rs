//! accounts, which lists the users and adds, deletes and changes them
//! through the accounts service, which asks for an administrator's
//! authorization where it needs one. `listing` reads the arguments and
//! makes the output, and `program`, left out of the host's build, reads
//! passwords and calls the accounts service.

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
