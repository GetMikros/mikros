//! seq on Mikros: it writes the numbers until the last or until writing
//! fails (MIKSPEC-0098.R50).

use crate::sequence::{self, COMMAND};
use mikros_cli::{FAILURE, INVALID, SUCCESS, complain, start};
use std::io::{self, Write};
use std::process::ExitCode;

pub fn main() -> ExitCode {
    let run = start(&COMMAND, &mikros_command::list);
    let numbers = sequence::numbers(&run.operands, run.value("-f"), run.has("-w"));
    let numbers = match numbers {
        Ok(numbers) => numbers,
        Err(why) => {
            complain(&COMMAND, &run, &why);
            return ExitCode::from(INVALID);
        }
    };
    let separator = run.value("-s").unwrap_or("\n");
    let mut out = io::BufWriter::new(io::stdout().lock());
    let mut written = Ok(());
    let mut any = false;
    for number in numbers {
        if any {
            written = out.write_all(separator.as_bytes());
        }
        written = written.and_then(|()| out.write_all(number.as_bytes()));
        any = true;
        if written.is_err() {
            break;
        }
    }
    if any {
        written = written.and_then(|()| out.write_all(b"\n"));
    }
    match written.and_then(|()| out.flush()) {
        Ok(()) => ExitCode::from(SUCCESS),
        Err(e) => {
            if e.kind() != io::ErrorKind::BrokenPipe {
                complain(&COMMAND, &run, &format!("standard output: {e}"));
            }
            ExitCode::from(FAILURE)
        }
    }
}
