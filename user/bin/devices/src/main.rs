//! devices, which lists the named devices that the device manager gives,
//! each as `<class>/<name>` with its description, or as JSON. `listing`
//! makes the output, and `program`, left out of the host's build, asks
//! the device manager.

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
