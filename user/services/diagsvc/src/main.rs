//! The diagnostic service of the diagnostic build: `service` serves the
//! overview of the system's state, which `snapshot` makes from the
//! kernel's account of it. Other builds do not install the service, and
//! their program only exits. Only `snapshot`, with its tests, is built for
//! the host.

#![cfg_attr(target_os = "mikros", no_std, no_main)]
#![cfg_attr(not(target_os = "mikros"), allow(dead_code))]

extern crate alloc;

#[cfg(target_os = "mikros")]
mod service;
#[cfg(any(mikros_diag, not(target_os = "mikros")))]
mod snapshot;

#[cfg(not(target_os = "mikros"))]
fn main() {}
