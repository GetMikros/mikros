//! pkgsvc, the install service (MIKSPEC-0101.R35): `service`, left out
//! of the host's build, serves `mikros.packages.Installer`.

#![cfg_attr(target_os = "mikros", feature(mikros_handles))]
#![cfg_attr(not(target_os = "mikros"), allow(dead_code))]

#[cfg(target_os = "mikros")]
mod service;

#[cfg(target_os = "mikros")]
fn main() -> std::process::ExitCode {
    service::main()
}

#[cfg(not(target_os = "mikros"))]
fn main() {}
