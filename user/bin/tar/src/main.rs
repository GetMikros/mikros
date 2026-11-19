//! tar, which creates, extracts and lists tar archives. `args` holds
//! what it takes and which entries a run takes, with the patterns of
//! `pattern`; `header` reads and writes header blocks, `reader` reads an
//! archive entry by entry, `create` writes one, `extract` extracts one
//! into a folder and only into it, with the checks of `inside`, and
//! `listing` writes what `-t` shows; `program`, left out of the host's
//! build, opens the archive and asks the volumes for their space.

#![cfg_attr(not(target_os = "mikros"), allow(dead_code))]

use mikros_extract as inside;
use mikros_glob as pattern;
mod args;
mod create;
mod extract;
mod header;
mod listing;
#[cfg(target_os = "mikros")]
mod program;
mod reader;

#[cfg(target_os = "mikros")]
fn main() -> std::process::ExitCode {
    program::main()
}

#[cfg(not(target_os = "mikros"))]
fn main() {}
