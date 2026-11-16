//! pkg, which installs, removes, lists, shows and makes `.mika` packages
//! (MIKSPEC-0101.R28). `request` reads the arguments, `order` puts the
//! packages to install after those they need, `show` makes the output,
//! `pack` makes packages from folders, and `program`, left out of the
//! host's build, calls the install service.

#![cfg_attr(target_os = "mikros", feature(mikros_handles))]
#![cfg_attr(not(target_os = "mikros"), allow(dead_code))]

mod order;
mod pack;
#[cfg(target_os = "mikros")]
mod program;
mod request;
mod show;

#[cfg(target_os = "mikros")]
fn main() -> std::process::ExitCode {
    program::main()
}

#[cfg(not(target_os = "mikros"))]
fn main() {}
