//! date, which writes the time, or sets it. `request` reads the
//! arguments and writes the forms, `format` writes the conversions of
//! strftime, `parse` reads the dates given, and `program`, left out of
//! the host's build, reads the clock, the local zone and files, and
//! asks the time service to set the time.

#![cfg_attr(target_os = "mikros", feature(mikros_handles))]
#![cfg_attr(not(target_os = "mikros"), allow(dead_code))]

mod format;
mod parse;
#[cfg(target_os = "mikros")]
mod program;
mod request;

#[cfg(target_os = "mikros")]
fn main() -> std::process::ExitCode {
    program::main()
}

#[cfg(not(target_os = "mikros"))]
fn main() {}
