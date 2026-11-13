//! selftest, which runs the system tests of a test image, the programs in
//! `/usr/bin/tests/`: `report` chooses the tests from its arguments,
//! scales their timeout by the machine's speed and words what happened to
//! each, and `program` measures that speed, runs them and, with
//! `--poweroff`, then powers the machine off. Only `report`, with its
//! tests, is built for the host.

#![cfg_attr(not(target_os = "mikros"), allow(dead_code))]
#![cfg_attr(target_os = "mikros", feature(mikros_handles))]

#[cfg(target_os = "mikros")]
mod program;
mod report;

#[cfg(target_os = "mikros")]
fn main() {
    std::process::exit(program::main());
}

#[cfg(not(target_os = "mikros"))]
fn main() {}
