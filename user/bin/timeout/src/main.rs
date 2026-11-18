//! timeout, which runs a command and signals it when its time is up.
//! `limits` reads the arguments, the durations and the signals, and
//! `program`, left out of the host's build, runs the command and sends
//! the signals.

#![cfg_attr(not(target_os = "mikros"), allow(dead_code))]

mod limits;
#[cfg(target_os = "mikros")]
mod program;

#[cfg(target_os = "mikros")]
fn main() -> std::process::ExitCode {
    program::main()
}

#[cfg(not(target_os = "mikros"))]
fn main() {}
