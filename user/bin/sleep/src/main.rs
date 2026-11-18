//! sleep, which waits for the sum of its durations. `duration` reads the
//! arguments and the durations, and `program`, left out of the host's
//! build, waits.

#![cfg_attr(not(target_os = "mikros"), allow(dead_code))]

mod duration;
#[cfg(target_os = "mikros")]
mod program;

#[cfg(target_os = "mikros")]
fn main() -> std::process::ExitCode {
    program::main()
}

#[cfg(not(target_os = "mikros"))]
fn main() {}
