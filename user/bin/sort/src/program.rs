//! sort on Mikros: it reads each file, or standard input, through the
//! standard library, continuing after one that fails, and writes the
//! lines to standard output or, once all are read, to the output file
//! (MIKSPEC-0098.R44, MIKSPEC-0098.R33).

use crate::sorting::{self, COMMAND, Sort};
use mikros_cli::{FAILURE, INVALID, Run, SUCCESS, complain, start};
use std::fs::File;
use std::io::{self, Read, Write};
use std::process::ExitCode;

pub fn main() -> ExitCode {
    let run = start(&COMMAND, &mikros_command::list);
    let sort = match Sort::of(&run) {
        Ok(sort) => sort,
        Err(why) => {
            complain(&COMMAND, &run, &why);
            return ExitCode::from(INVALID);
        }
    };
    let check = run.has("-c");
    if check && run.operands.len() > 1 {
        complain(&COMMAND, &run, "-c checks one file");
        return ExitCode::from(INVALID);
    }
    let operands: Vec<String> = match run.operands.is_empty() {
        true => vec!["-".into()],
        false => run.operands.clone(),
    };
    let mut failed = false;
    let mut lines = Vec::new();
    for operand in &operands {
        match read(operand) {
            Ok(text) => {
                let read = sorting::split(&text, sort.end);
                if check {
                    return checked(&run, &sort, operand, &read);
                }
                lines.extend(read);
            }
            Err(e) => {
                complain(&COMMAND, &run, &format!("{operand}: {e}"));
                failed = true;
            }
        }
    }
    if check {
        return ExitCode::from(FAILURE);
    }
    // Sorting all the lines together gives what merging sorted files
    // gives, so -m needs nothing of its own.
    sort.sort(&mut lines);
    let mut text = Vec::with_capacity(lines.iter().map(|l| l.len() + 1).sum());
    for line in &lines {
        text.extend_from_slice(line);
        text.push(sort.end);
    }
    let output = run.value("-o");
    let written = match output {
        Some(path) => File::create(path).and_then(|mut f| f.write_all(&text)),
        None => io::stdout().lock().write_all(&text),
    };
    if let Err(e) = written {
        match output {
            Some(path) => complain(&COMMAND, &run, &format!("{path}: {e}")),
            None if e.kind() == io::ErrorKind::BrokenPipe => {}
            None => complain(&COMMAND, &run, &format!("standard output: {e}")),
        }
        failed = true;
    }
    match failed {
        true => ExitCode::from(FAILURE),
        false => ExitCode::from(SUCCESS),
    }
}

/// Checks that `lines`, read from `operand`, are sorted, naming the
/// first that is not.
fn checked(run: &Run, sort: &Sort, operand: &str, lines: &[Vec<u8>]) -> ExitCode {
    match sort.disorder(lines) {
        None => ExitCode::from(SUCCESS),
        Some(number) => {
            let line = String::from_utf8_lossy(&lines[number - 1]);
            complain(
                &COMMAND,
                run,
                &format!("{operand}:{number}: disorder: {line}"),
            );
            ExitCode::from(FAILURE)
        }
    }
}

/// The bytes of the file `operand`, or of standard input for `-`.
fn read(operand: &str) -> io::Result<Vec<u8>> {
    let mut text = Vec::new();
    match operand {
        "-" => io::stdin().lock().read_to_end(&mut text)?,
        path => File::open(path)?.read_to_end(&mut text)?,
    };
    Ok(text)
}
