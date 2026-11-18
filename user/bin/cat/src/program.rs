//! cat on Mikros: it reads the files, or standard input for `-`, through
//! the standard library and writes them to standard output, as they are
//! or as the options show them (MIKSPEC-0098.R33).

use crate::showing::{COMMAND, Lines, Show};
use mikros_cli::{FAILURE, Run, SUCCESS, complain, start};
use std::fs::File;
use std::io::{self, ErrorKind, Read, Write};
use std::process::ExitCode;

/// How much is read at a time.
const CHUNK: usize = 64 * 1024;

/// Why copying one file stopped.
enum Stop {
    /// Reading it failed; the next file is copied.
    Read(io::Error),
    /// Writing failed; nothing more can be copied.
    Write(io::Error),
}

pub fn main() -> ExitCode {
    let run = start(&COMMAND, &mikros_command::list);
    let show = Show::of(&run);
    let mut lines = Lines::new(show);
    let operands: Vec<String> = match run.operands.is_empty() {
        true => vec!["-".into()],
        false => run.operands.clone(),
    };
    let mut out = io::stdout().lock();
    let mut failed = false;
    for operand in &operands {
        let copied = match operand.as_str() {
            "-" => copy(&mut io::stdin().lock(), &mut out, show, &mut lines),
            path => match File::open(path) {
                Ok(mut file) => copy(&mut file, &mut out, show, &mut lines),
                Err(e) => {
                    fail(&run, &format!("cannot open {path}: {e}"), &mut failed);
                    continue;
                }
            },
        };
        match copied {
            Ok(()) => {}
            Err(Stop::Read(e)) => fail(&run, &format!("cannot read {operand}: {e}"), &mut failed),
            Err(Stop::Write(e)) => {
                complain(&COMMAND, &run, &format!("cannot write: {e}"));
                return ExitCode::from(FAILURE);
            }
        }
    }
    if let Err(e) = out.flush() {
        fail(&run, &format!("cannot write: {e}"), &mut failed);
    }
    match failed {
        true => ExitCode::from(FAILURE),
        false => ExitCode::from(SUCCESS),
    }
}

fn fail(run: &Run, message: &str, failed: &mut bool) {
    complain(&COMMAND, run, message);
    *failed = true;
}

/// Copies `input` to `out` until its end, as `show` has it shown.
fn copy(
    input: &mut dyn Read,
    out: &mut dyn Write,
    show: Show,
    lines: &mut Lines,
) -> Result<(), Stop> {
    let mut buffer = vec![0; CHUNK];
    let mut shown = Vec::new();
    loop {
        let count = match input.read(&mut buffer) {
            Ok(0) => return Ok(()),
            Ok(count) => count,
            Err(e) if e.kind() == ErrorKind::Interrupted => continue,
            Err(e) => return Err(Stop::Read(e)),
        };
        let written = match show.plain() {
            true => out.write_all(&buffer[..count]),
            false => {
                shown.clear();
                lines.feed(&buffer[..count], &mut shown);
                out.write_all(&shown)
            }
        };
        written.map_err(Stop::Write)?;
    }
}
