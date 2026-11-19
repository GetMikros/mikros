//! head on Mikros: it reads each file, or standard input, through the
//! standard library, continuing after one that fails (MIKSPEC-0098.R44,
//! MIKSPEC-0098.R33).

use crate::heading::{self, COMMAND, Failed, Part};
use mikros_cli::{FAILURE, INVALID, Paint, SUCCESS, complain, start};
use std::fs::File;
use std::io::{self, BufRead, BufReader, BufWriter, Write};
use std::process::ExitCode;

pub fn main() -> ExitCode {
    let run = start(&COMMAND, &mikros_command::list);
    let part = match Part::of(&run) {
        Ok(part) => part,
        Err(why) => {
            complain(&COMMAND, &run, &why);
            return ExitCode::from(INVALID);
        }
    };
    let operands: Vec<String> = match run.operands.is_empty() {
        true => vec!["-".into()],
        false => run.operands.clone(),
    };
    let titled = heading::titled(&run, operands.len());
    let paint = Paint::stdout(&run);
    let mut out = BufWriter::new(io::stdout().lock());
    let mut failed = false;
    let mut first = true;
    for operand in &operands {
        let mut input: Box<dyn BufRead> = match operand.as_str() {
            "-" => Box::new(io::stdin().lock()),
            path => match File::open(path) {
                Ok(file) => Box::new(BufReader::new(file)),
                Err(e) => {
                    complain(&COMMAND, &run, &format!("{operand}: {e}"));
                    failed = true;
                    continue;
                }
            },
        };
        let mut copied = Ok(());
        if titled {
            let title = heading::title(operand, first, paint);
            copied = out.write_all(title.as_bytes()).map_err(Failed::Write);
        }
        first = false;
        if copied.is_ok() {
            copied = heading::copy(part, &mut input, &mut out);
        }
        match copied {
            Ok(()) => {}
            Err(Failed::Read(e)) => {
                complain(&COMMAND, &run, &format!("{operand}: {e}"));
                failed = true;
            }
            Err(Failed::Write(e)) => return written(&run, e),
        }
    }
    if let Err(e) = out.flush() {
        return written(&run, e);
    }
    match failed {
        true => ExitCode::from(FAILURE),
        false => ExitCode::from(SUCCESS),
    }
}

/// Ends after writing to standard output failed, quietly when the
/// reader closed the pipe.
fn written(run: &mikros_cli::Run, e: io::Error) -> ExitCode {
    if e.kind() != io::ErrorKind::BrokenPipe {
        complain(&COMMAND, run, &format!("standard output: {e}"));
    }
    ExitCode::from(FAILURE)
}
