//! dirname on Mikros: it writes the folders to standard output.

use crate::names::{self, COMMAND};
use mikros_cli::{FAILURE, Paint, SUCCESS, complain, start};
use std::io::Write;
use std::process::ExitCode;

pub fn main() -> ExitCode {
    let run = start(&COMMAND, &mikros_command::list);
    let text = names::output(&run, Paint::stdout(&run));
    match std::io::stdout().write_all(text.as_bytes()) {
        Ok(()) => ExitCode::from(SUCCESS),
        Err(e) => {
            complain(&COMMAND, &run, &format!("writing the folders: {e}"));
            ExitCode::from(FAILURE)
        }
    }
}
