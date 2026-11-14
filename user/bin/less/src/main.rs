//! less, which shows a file or its standard input one screen at a time
//! on the terminal of its standard output, or copies it when that is not
//! a terminal. `keys` reads the terminal's keys, `pager` keeps what is
//! shown and moves it, and `program`, left out of the host's build, runs
//! them on the terminal.

#![cfg_attr(not(target_os = "mikros"), allow(dead_code))]

// Until the next commit pages with them on Mikros.
#[cfg_attr(target_os = "mikros", allow(dead_code))]
mod keys;
#[cfg_attr(target_os = "mikros", allow(dead_code))]
mod pager;
#[cfg(target_os = "mikros")]
mod program;

#[cfg(target_os = "mikros")]
fn main() -> std::process::ExitCode {
    program::main()
}

#[cfg(not(target_os = "mikros"))]
fn main() {}
