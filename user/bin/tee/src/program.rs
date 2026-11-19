//! tee on Mikros: it opens the files through the standard library,
//! going on without one that does not open, and copies standard input
//! to them and to standard output (MIKSPEC-0098.R49).

use crate::copying::{self, COMMAND, Output};
use mikros_cli::{FAILURE, SUCCESS, complain, start};
use std::ffi::c_int;
use std::fs::OpenOptions;
use std::io::{self, Write};
use std::process::ExitCode;

const SIGINT: c_int = 2;
const SIG_IGN: usize = 1;

unsafe extern "C" {
    fn signal(signal: c_int, handler: usize) -> usize;
}

pub fn main() -> ExitCode {
    let run = start(&COMMAND, &mikros_command::list);
    if run.has("-i") {
        // SAFETY: ignoring a signal installs no handler.
        unsafe { signal(SIGINT, SIG_IGN) };
    }
    let mut failed = false;
    let mut outputs: Vec<Output<Box<dyn Write>>> = vec![Output {
        name: "standard output".into(),
        writer: Some(Box::new(io::stdout())),
    }];
    for name in &run.operands {
        let opened = OpenOptions::new()
            .write(true)
            .create(true)
            .append(run.has("-a"))
            .truncate(!run.has("-a"))
            .open(name);
        match opened {
            Ok(file) => outputs.push(Output {
                name: name.clone(),
                writer: Some(Box::new(file)),
            }),
            Err(e) => {
                complain(&COMMAND, &run, &format!("{name}: {e}"));
                failed = true;
            }
        }
    }
    let copied = copying::copy(&mut io::stdin().lock(), &mut outputs, run.has("-p"));
    for (name, e) in &copied.failed {
        complain(&COMMAND, &run, &format!("{name}: {e}"));
    }
    if let Some(e) = &copied.read {
        complain(&COMMAND, &run, &format!("standard input: {e}"));
    }
    failed |= !copied.failed.is_empty() || copied.read.is_some() || copied.closed;
    match failed {
        true => ExitCode::from(FAILURE),
        false => ExitCode::from(SUCCESS),
    }
}
