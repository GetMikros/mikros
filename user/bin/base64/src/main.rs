//! base64, which encodes files as Base64 and decodes them. `coding`
//! reads the arguments and holds the codec, and `program`, left out of
//! the host's build, reads the file and writes the result.

#![cfg_attr(not(target_os = "mikros"), allow(dead_code))]

mod coding;
#[cfg(target_os = "mikros")]
mod program;

#[cfg(target_os = "mikros")]
fn main() -> std::process::ExitCode {
    program::main()
}

#[cfg(not(target_os = "mikros"))]
fn main() {}
