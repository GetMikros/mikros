//! cut on Mikros: it reads each file, or standard input, line by line
//! through the standard library, continuing after one that fails
//! (MIKSPEC-0098.R44, MIKSPEC-0098.R33).

use crate::cutting::{COMMAND, Cut};
use mikros_cli::{FAILURE, INVALID, SUCCESS, complain, start};
use std::fs::File;
use std::io::{self, BufRead, BufReader, Read, Write};
use std::process::ExitCode;

pub fn main() -> ExitCode {
    let run = start(&COMMAND, &mikros_command::list);
    let cut = match Cut::of(&run) {
        Ok(cut) => cut,
        Err(why) => {
            complain(&COMMAND, &run, &why);
            return ExitCode::from(INVALID);
        }
    };
    let operands: Vec<String> = match run.operands.is_empty() {
        true => vec!["-".into()],
        false => run.operands.clone(),
    };
    let mut out = io::BufWriter::new(io::stdout().lock());
    let mut failed = false;
    for operand in &operands {
        match cut_file(operand, &cut, &mut out) {
            Ok(()) => {}
            Err(Failure::Output(e)) => {
                if e.kind() != io::ErrorKind::BrokenPipe {
                    complain(&COMMAND, &run, &format!("standard output: {e}"));
                }
                return ExitCode::from(FAILURE);
            }
            Err(Failure::Input(e)) => {
                complain(&COMMAND, &run, &format!("{operand}: {e}"));
                failed = true;
            }
        }
    }
    if let Err(e) = out.flush() {
        if e.kind() != io::ErrorKind::BrokenPipe {
            complain(&COMMAND, &run, &format!("standard output: {e}"));
        }
        failed = true;
    }
    match failed {
        true => ExitCode::from(FAILURE),
        false => ExitCode::from(SUCCESS),
    }
}

/// What failed: reading a file, or writing.
enum Failure {
    Input(io::Error),
    Output(io::Error),
}

/// Cuts each line of the file `operand`, or of standard input for `-`,
/// to `out`.
fn cut_file(operand: &str, cut: &Cut, out: &mut impl Write) -> Result<(), Failure> {
    let input: Box<dyn Read> = match operand {
        "-" => Box::new(io::stdin().lock()),
        path => Box::new(File::open(path).map_err(Failure::Input)?),
    };
    let mut input = BufReader::with_capacity(64 * 1024, input);
    let mut line = Vec::new();
    let mut written = Vec::new();
    loop {
        line.clear();
        if input.read_until(b'\n', &mut line).map_err(Failure::Input)? == 0 {
            return Ok(());
        }
        if line.last() == Some(&b'\n') {
            line.pop();
        }
        written.clear();
        cut.line(&line, &mut written);
        out.write_all(&written).map_err(Failure::Output)?;
    }
}
