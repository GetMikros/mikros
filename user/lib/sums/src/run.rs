//! A run of a sum command: it reads the files, or standard input, through
//! the standard library, continuing after one that fails, and with `-c`
//! reads lists of sums and checks the files they name
//! (MIKSPEC-0098.R44, MIKSPEC-0098.R33).

use crate::Algorithm;
use crate::lines::{self as sums, CHECKING, Form, Tally};
use mikros_cli::complete::Lister;
use mikros_cli::{FAILURE, INVALID, Paint, Role, Run, SUCCESS, complain, start};
use std::ffi::OsStr;
use std::fs::File;
use std::io::{self, Read, Write};
use std::os::unix::ffi::OsStrExt;
use std::process::ExitCode;

/// Runs the command of `algorithm`, whose completion offers what `list`
/// names.
pub fn run(algorithm: &'static Algorithm, list: Lister) -> ExitCode {
    let a = algorithm;
    let run = start(a.command, list);
    if let Some(why) = refused(&run) {
        complain(a.command, &run, why);
        return ExitCode::from(INVALID);
    }
    let operands: Vec<String> = match run.operands.is_empty() {
        true => vec!["-".into()],
        false => run.operands.clone(),
    };
    let mut out = io::BufWriter::new(io::stdout().lock());
    let mut failed = match run.has("-c") {
        true => check(a, &run, &operands, &mut out),
        false => hash(a, &run, &operands, &mut out),
    };
    if let Err(e) = out.flush() {
        if e.kind() != io::ErrorKind::BrokenPipe {
            complain(a.command, &run, &format!("standard output: {e}"));
        }
        failed = true;
    }
    match failed {
        true => ExitCode::from(FAILURE),
        false => ExitCode::from(SUCCESS),
    }
}

/// Why the options given do not go together, if they do not.
fn refused(run: &Run) -> Option<&'static str> {
    if run.has("-c") {
        if run.has("-b") || run.has("-t") || run.has("--tag") {
            return Some("-b, -t and --tag do not apply when checking sums");
        }
        if run.has("--json") {
            return Some("--json does not apply when checking sums");
        }
        return None;
    }
    if CHECKING.iter().any(|o| run.has(o)) {
        return Some("--quiet, --status, --strict and --warn apply only with -c");
    }
    if run.has("--tag") && run.has("-t") {
        return Some("--tag does not take -t");
    }
    None
}

/// What `operand` names, or standard input for `-`.
fn open(operand: &[u8]) -> io::Result<Box<dyn Read>> {
    match operand {
        b"-" => Ok(Box::new(io::stdin().lock())),
        path => Ok(Box::new(File::open(OsStr::from_bytes(path))?)),
    }
}

/// Writes the sum of each operand, and returns whether one failed.
fn hash(a: &Algorithm, run: &Run, operands: &[String], out: &mut dyn Write) -> bool {
    let json = run.has("--json");
    let paint = match json {
        true => Paint::never(),
        false => Paint::stdout(run),
    };
    let form = Form {
        binary: run.has("-b"),
        tag: run.has("--tag"),
        zero: run.has("-z"),
    };
    let mut failed = false;
    let mut rows = Vec::new();
    for operand in operands {
        let sum = open(operand.as_bytes()).and_then(|mut input| (a.digest)(&mut input));
        match sum {
            Err(e) => {
                complain(a.command, run, &format!("{operand}: {e}"));
                failed = true;
            }
            Ok(hash) if json => rows.push((operand.clone(), hash)),
            Ok(hash) => {
                if write(
                    a,
                    run,
                    out,
                    &sums::line(a, &hash, operand.as_bytes(), form, paint),
                ) {
                    return true;
                }
            }
        }
    }
    if json {
        return write(a, run, out, sums::json(&rows).as_bytes()) || failed;
    }
    failed
}

/// Writes `bytes`, and returns whether that failed, after saying so
/// unless the reader went away.
fn write(a: &Algorithm, run: &Run, out: &mut dyn Write, bytes: &[u8]) -> bool {
    match out.write_all(bytes) {
        Ok(()) => false,
        Err(e) => {
            if e.kind() != io::ErrorKind::BrokenPipe {
                complain(a.command, run, &format!("standard output: {e}"));
            }
            true
        }
    }
}

/// Checks the sums of each list, and returns whether one did not match,
/// could not be read, or the list itself failed.
fn check(a: &Algorithm, run: &Run, lists: &[String], out: &mut dyn Write) -> bool {
    let mut failed = false;
    for list in lists {
        let mut text = Vec::new();
        if let Err(e) = open(list.as_bytes()).and_then(|mut l| l.read_to_end(&mut text)) {
            complain(a.command, run, &format!("{list}: {e}"));
            failed = true;
            continue;
        }
        let shown = match list.as_str() {
            "-" => "standard input",
            name => name,
        };
        match check_list(a, run, shown, &text, out) {
            None => return true,
            Some(tally) => {
                if tally.listed == 0 {
                    complain(
                        a.command,
                        run,
                        &format!("{shown}: no properly formatted checksum lines found"),
                    );
                    failed = true;
                    continue;
                }
                if !run.has("--status") {
                    let paint = Paint::stderr(run);
                    for warning in tally.warnings() {
                        eprintln!(
                            "{}: {}",
                            a.command.name,
                            paint.paint(Role::Warning, &warning)
                        );
                    }
                }
                failed |= tally.mismatched > 0
                    || tally.unread > 0
                    || (run.has("--strict") && tally.improper > 0);
            }
        }
    }
    failed
}

/// Checks the lines of the list `text`, named `shown`, and tallies what
/// it found, `None` when writing failed.
fn check_list(
    a: &Algorithm,
    run: &Run,
    shown: &str,
    text: &[u8],
    out: &mut dyn Write,
) -> Option<Tally> {
    let paint = Paint::stdout(run);
    let quiet = run.has("--quiet");
    let status = run.has("--status");
    let end = if run.has("-z") { b'\0' } else { b'\n' };
    let mut tally = Tally::default();
    let text = text.strip_suffix(b"\n").unwrap_or(text);
    for (number, line) in text.split(|&b| b == b'\n').enumerate() {
        if line.first() == Some(&b'#') {
            continue;
        }
        let Some(listed) = sums::parse(a, line) else {
            tally.improper += 1;
            if run.has("--warn") {
                let number = number + 1;
                complain(
                    a.command,
                    run,
                    &format!(
                        "{shown}: {number}: improperly formatted {} checksum line",
                        a.name
                    ),
                );
            }
            continue;
        };
        tally.listed += 1;
        let sum = open(&listed.name).and_then(|mut input| (a.digest)(&mut input));
        let (verdict, role) = match sum {
            Err(e) => {
                let name = String::from_utf8_lossy(&listed.name);
                complain(a.command, run, &format!("{name}: {e}"));
                tally.unread += 1;
                ("FAILED open or read", Role::Error)
            }
            Ok(hash) if hash == listed.hash => ("OK", Role::Ok),
            Ok(_) => {
                tally.mismatched += 1;
                ("FAILED", Role::Error)
            }
        };
        if status || (quiet && role == Role::Ok) {
            continue;
        }
        let (escaped, name) = match run.has("-z") {
            true => (false, listed.name.clone()),
            false => sums::escape(&listed.name),
        };
        let mut line = Vec::new();
        if escaped {
            line.push(b'\\');
        }
        line.extend(sums::paint_bytes(paint, Role::Path, &name));
        line.extend_from_slice(b": ");
        line.extend_from_slice(paint.paint(role, verdict).as_bytes());
        line.push(end);
        if write(a, run, out, &line) {
            return None;
        }
    }
    Some(tally)
}
