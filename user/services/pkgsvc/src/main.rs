//! pkgsvc, the install service (MIKSPEC-0101.R43): `place` says where a
//! package is installed, `records` keeps what the file system does not
//! say about installed packages, `commands` which package owns each
//! command, and `service`, left out of the host's build, serves
//! `mikros.packages.Installer` on the root file system.

#![cfg_attr(target_os = "mikros", feature(mikros_handles))]
#![cfg_attr(not(target_os = "mikros"), allow(dead_code))]

mod commands;
mod place;
mod records;
#[cfg(target_os = "mikros")]
mod service;

#[cfg(target_os = "mikros")]
fn main() -> std::process::ExitCode {
    service::main()
}

#[cfg(not(target_os = "mikros"))]
fn main() {}
