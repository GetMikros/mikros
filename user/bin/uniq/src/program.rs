//! uniq on Mikros: it reads the input file, or standard input, line by
//! line through the standard library, and writes to the output file or
//! standard output (MIKSPEC-0098.R44).

use crate::merging::{COMMAND, Merge, Merger};
use mikros_cli::{FAILURE, INVALID, SUCCESS, complain, start};
use std::fs::File;
use std::io::{self, BufRead, BufReader, Read, Write};
use std::process::ExitCode;

pub fn main() -> ExitCode {
    let run = start(&COMMAND, &mikros_command::list);
    let merge = match Merge::of(&run) {
        Ok(merge) => merge,
        Err(why) => {
            complain(&COMMAND, &run, &why);
            return ExitCode::from(INVALID);
        }
    };
    let input_name = run.operands.first().map_or("-", String::as_str);
    let output_name = run.operands.get(1).map_or("-", String::as_str);
    let input: Box<dyn Read> = match input_name {
        "-" => Box::new(io::stdin().lock()),
        path => match File::open(path) {
            Ok(file) => Box::new(file),
            Err(e) => {
                complain(&COMMAND, &run, &format!("{path}: {e}"));
                return ExitCode::from(FAILURE);
            }
        },
    };
    let output: Box<dyn Write> = match output_name {
        "-" => Box::new(io::stdout().lock()),
        path => match File::create(path) {
            Ok(file) => Box::new(file),
            Err(e) => {
                complain(&COMMAND, &run, &format!("{path}: {e}"));
                return ExitCode::from(FAILURE);
            }
        },
    };
    let mut input = BufReader::with_capacity(64 * 1024, input);
    let mut out = io::BufWriter::with_capacity(64 * 1024, output);
    let mut merger = Merger::new(&merge);
    let mut line = Vec::new();
    loop {
        line.clear();
        match input.read_until(merge.end, &mut line) {
            Ok(0) => break,
            Ok(_) => {}
            Err(e) => {
                complain(&COMMAND, &run, &format!("{input_name}: {e}"));
                let _ = merger.finish(&mut out).and_then(|()| out.flush());
                return ExitCode::from(FAILURE);
            }
        }
        if line.last() == Some(&merge.end) {
            line.pop();
        }
        if let Err(e) = merger.line(&line, &mut out) {
            return written(&run, output_name, e);
        }
    }
    match merger.finish(&mut out).and_then(|()| out.flush()) {
        Ok(()) => ExitCode::from(SUCCESS),
        Err(e) => written(&run, output_name, e),
    }
}

/// Reports that writing to `name` failed with `e`, unless standard
/// output's reader went away, and fails.
fn written(run: &mikros_cli::Run, name: &str, e: io::Error) -> ExitCode {
    if !(name == "-" && e.kind() == io::ErrorKind::BrokenPipe) {
        let name = if name == "-" { "standard output" } else { name };
        complain(&COMMAND, run, &format!("{name}: {e}"));
    }
    ExitCode::from(FAILURE)
}
