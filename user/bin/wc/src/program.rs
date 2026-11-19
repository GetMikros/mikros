//! wc on Mikros: it reads each file, or standard input, through the
//! standard library, continuing after one that fails (MIKSPEC-0098.R44,
//! MIKSPEC-0098.R33).

use crate::counting::{self, COMMAND, Counter, Counts, Name, Show};
use mikros_cli::{EntryType, FAILURE, Paint, SUCCESS, complain, start};
use std::fs::File;
use std::io::{self, Read, Write};
use std::process::ExitCode;

pub fn main() -> ExitCode {
    let run = start(&COMMAND, &mikros_command::list);
    let show = Show::of(&run);
    let json = run.has("--json");
    let named = !run.operands.is_empty();
    let operands: Vec<String> = match named {
        true => run.operands.clone(),
        false => vec!["-".into()],
    };
    let mut failed = false;
    let mut rows: Vec<(String, Counts)> = Vec::new();
    for operand in &operands {
        match count(operand) {
            Ok(counts) => rows.push((operand.clone(), counts)),
            Err(e) => {
                complain(&COMMAND, &run, &format!("{operand}: {e}"));
                failed = true;
            }
        }
    }
    let text = match json {
        true => counting::json(&rows),
        false => {
            let paint = Paint::stdout(&run);
            let mut shown: Vec<(Option<Name>, Counts)> = rows
                .iter()
                .map(|(path, counts)| {
                    let name = named.then(|| (path.clone(), paint.entry_type(path)));
                    (name, *counts)
                })
                .collect();
            if operands.len() > 1 {
                let mut total = Counts::default();
                rows.iter().for_each(|(_, c)| total.add(c));
                shown.push((Some(("total".into(), EntryType::Other)), total));
            }
            counting::text(&shown, show, paint)
        }
    };
    if let Err(e) = io::stdout().write_all(text.as_bytes()) {
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

/// The counts of the file `operand`, or of standard input for `-`.
fn count(operand: &str) -> io::Result<Counts> {
    let mut input: Box<dyn Read> = match operand {
        "-" => Box::new(io::stdin().lock()),
        path => Box::new(File::open(path)?),
    };
    let mut counter = Counter::default();
    let mut buffer = vec![0; 64 * 1024];
    loop {
        match input.read(&mut buffer) {
            Ok(0) => return Ok(counter.finish()),
            Ok(n) => counter.feed(&buffer[..n]),
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
}
