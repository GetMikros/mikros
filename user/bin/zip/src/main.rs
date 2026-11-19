//! zip, which creates zip archives or adds files to them, replacing
//! entries of the same name, or removes entries from them. `args` holds
//! what it takes, `central` reads the central directory of an archive
//! that is there, `writing` writes entries and the directory, and
//! `update` gathers the paths and makes the new archive from the old
//! one, with the patterns of `pattern`; `program`, left out of the
//! host's build, opens the archives and puts the new one in place.

#![cfg_attr(not(target_os = "mikros"), allow(dead_code))]

use mikros_glob as pattern;
use mikros_zip as central;
mod args;
#[cfg(target_os = "mikros")]
mod program;
mod update;
mod writing;

#[cfg(target_os = "mikros")]
fn main() -> std::process::ExitCode {
    program::main()
}

#[cfg(not(target_os = "mikros"))]
fn main() {}
