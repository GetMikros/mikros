//! uptime, which writes the time and how long the system has run.
//! `since` reads the arguments and writes the times and durations, and
//! `program`, left out of the host's build, reads the clocks.

#![cfg_attr(not(target_os = "mikros"), allow(dead_code))]

#[cfg(target_os = "mikros")]
mod program;
mod since;

#[cfg(target_os = "mikros")]
fn main() -> std::process::ExitCode {
    program::main()
}

#[cfg(not(target_os = "mikros"))]
fn main() {}
