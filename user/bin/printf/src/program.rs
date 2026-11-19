//! printf on Mikros: it reads its arguments so that those after the
//! format are never options, writes the result, and names each argument
//! that was not a number (MIKSPEC-0098.R45).

use crate::format::{self, COMMAND};
use mikros_cli::{FAILURE, SUCCESS, complain, start};
use std::io::{self, Write};
use std::process::ExitCode;

pub fn main() -> ExitCode {
    let run = start(&COMMAND, &mikros_command::list);
    let format = run.operands[0].as_bytes();
    let written = format::printf(format, &run.operands[1..]);
    let mut failed = false;
    let mut out = io::stdout().lock();
    if let Err(e) = out.write_all(&written.out).and_then(|()| out.flush()) {
        if e.kind() != io::ErrorKind::BrokenPipe {
            complain(&COMMAND, &run, &format!("standard output: {e}"));
        }
        failed = true;
    }
    for error in &written.errors {
        complain(&COMMAND, &run, error);
        failed = true;
    }
    if let Some(why) = &written.fatal {
        complain(&COMMAND, &run, why);
        failed = true;
    }
    match failed {
        true => ExitCode::from(FAILURE),
        false => ExitCode::from(SUCCESS),
    }
}
