//! basename on Mikros: it writes the names to standard output.

use crate::names::{self, COMMAND};
use mikros_cli::{FAILURE, INVALID, Paint, SUCCESS, complain, start};
use std::io::Write;
use std::process::ExitCode;

pub fn main() -> ExitCode {
    let run = start(&COMMAND, &mikros_command::list);
    let text = match names::output(&run, Paint::stdout(&run)) {
        Ok(text) => text,
        Err(why) => {
            complain(&COMMAND, &run, &why);
            return ExitCode::from(INVALID);
        }
    };
    match std::io::stdout().write_all(text.as_bytes()) {
        Ok(()) => ExitCode::from(SUCCESS),
        Err(e) => {
            complain(&COMMAND, &run, &format!("writing the names: {e}"));
            ExitCode::from(FAILURE)
        }
    }
}
