//! printf, which writes a format with its arguments. `format` reads the
//! arguments and writes the format, with the values of `values` and the
//! number conversions of `numbers`, and `program`, left out of the
//! host's build, writes the result and the errors.

#![cfg_attr(not(target_os = "mikros"), allow(dead_code))]

mod format;
#[cfg(target_os = "mikros")]
mod program;
mod values;

#[cfg(target_os = "mikros")]
fn main() -> std::process::ExitCode {
    program::main()
}

#[cfg(not(target_os = "mikros"))]
fn main() {}
