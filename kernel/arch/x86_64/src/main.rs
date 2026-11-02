//! The Mikros kernel for x86_64: the architecture layer and the entry point.

#![cfg_attr(target_os = "none", no_std)]
#![cfg_attr(target_os = "none", no_main)]

#[cfg(target_os = "none")]
mod cpu;
#[cfg(target_os = "none")]
mod entry;
#[cfg(target_os = "none")]
mod panic;
#[cfg(target_os = "none")]
mod serial;

/// The kernel only runs on its own target. This lets the workspace build
/// on the host.
#[cfg(not(target_os = "none"))]
fn main() {}
