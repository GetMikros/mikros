//! bootsvc, the boot attempts service: `attempts` reads the `MikrosBoot`
//! firmware variable and says what the booted version's state is and how
//! marking it good changes the variable, and `service`, left out of the
//! host's build, serves `mikros.boot.Attempts` through the firmware
//! variable calls.

#![cfg_attr(target_os = "mikros", feature(mikros_handles))]
#![cfg_attr(not(target_os = "mikros"), allow(dead_code))]

mod attempts;
#[cfg(target_os = "mikros")]
mod service;

#[cfg(target_os = "mikros")]
fn main() -> std::process::ExitCode {
    service::main()
}

#[cfg(not(target_os = "mikros"))]
fn main() {}
