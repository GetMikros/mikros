//! A program that global-check runs in ada's session, with a program
//! manifest that lets it call the broker's Check and Request as a service
//! does for its clients. It asks for `user.camera.use` of its own process
//! while naming another app, as a service that forwards a forged identity
//! would. The session manager refuses that at once, without a question
//! (MIKSPEC-0057.R33), so the broker answers DENIED and records nothing.
//! It exits with 0 then, and otherwise with the number of the check that
//! failed.

#![cfg_attr(target_os = "mikros", feature(mikros_handles))]
#![cfg_attr(not(target_os = "mikros"), allow(dead_code))]

#[cfg(target_os = "mikros")]
mod program;

#[cfg(target_os = "mikros")]
fn main() -> std::process::ExitCode {
    program::main()
}

#[cfg(not(target_os = "mikros"))]
fn main() {}
