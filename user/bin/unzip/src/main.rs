//! unzip, which extracts, lists, checks or writes out the entries of a
//! zip archive. `args` holds what it takes and which entries a run
//! takes, with the patterns of `pattern`; `central` reads the central
//! directory, `data` the entries' data, `extract` extracts them into a
//! folder and only into it, with the checks of `inside`, and `listing`
//! writes what `-l` shows; `program`, left out of the host's build,
//! opens the archive and asks the volumes for their space.

#![cfg_attr(not(target_os = "mikros"), allow(dead_code))]

use mikros_extract as inside;
use mikros_glob as pattern;
use mikros_zip as central;
mod args;
mod data;
mod extract;
mod listing;
#[cfg(target_os = "mikros")]
mod program;

#[cfg(target_os = "mikros")]
fn main() -> std::process::ExitCode {
    program::main()
}

#[cfg(not(target_os = "mikros"))]
fn main() {}
