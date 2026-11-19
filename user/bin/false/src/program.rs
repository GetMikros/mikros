//! false on Mikros: it exits with 1, after its help with `--help` alone,
//! and answers msh's completion mode, which `start` does and then exits
//! with 0.

use crate::command::{COMMAND, answers, completion};
use mikros_cli::{FAILURE, start};
use std::process::ExitCode;

pub fn main() -> ExitCode {
    let arguments: Vec<String> = std::env::args().skip(1).collect();
    if arguments == ["--help"] {
        mikros_cli::show_help(&COMMAND.help(&[]));
    } else if answers(&arguments, completion().as_deref()) {
        start(&COMMAND, &mikros_command::list);
    }
    ExitCode::from(FAILURE)
}
