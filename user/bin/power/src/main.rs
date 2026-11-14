//! power, which powers the machine off or restarts it once an
//! administrator allowed it. `action` reads what it is asked, and
//! `program`, left out of the host's build, asks for the authorization
//! and the shutdown.

#![cfg_attr(target_os = "mikros", feature(mikros_handles))]
#![cfg_attr(not(target_os = "mikros"), allow(dead_code))]

mod action;
#[cfg(target_os = "mikros")]
mod program;

#[cfg(target_os = "mikros")]
fn main() -> std::process::ExitCode {
    program::main()
}

#[cfg(not(target_os = "mikros"))]
fn main() {}
