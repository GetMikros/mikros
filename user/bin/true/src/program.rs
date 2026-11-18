//! true on Mikros: it exits with its status, unless it answers `--help`
//! or msh's completion mode, which `start` does and then exits with 0.

use crate::command::{COMMAND, answers, completion};
use mikros_cli::{SUCCESS, start};
use std::process::ExitCode;

pub fn main() -> ExitCode {
    let arguments: Vec<String> = std::env::args().skip(1).collect();
    if answers(&arguments, completion().as_deref()) {
        start(&COMMAND, &mikros_command::list);
    }
    ExitCode::from(SUCCESS)
}
