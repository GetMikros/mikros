//! tr on Mikros: it copies standard input to standard output through the
//! standard library, changing the characters as it goes
//! (MIKSPEC-0098.R44).

use crate::translating::{COMMAND, Tr, Translator};
use mikros_cli::{FAILURE, INVALID, SUCCESS, complain, start};
use std::io::{self, Read, Write};
use std::process::ExitCode;

pub fn main() -> ExitCode {
    let run = start(&COMMAND, &mikros_command::list);
    let tr = match Tr::of(&run) {
        Ok(tr) => tr,
        Err(why) => {
            complain(&COMMAND, &run, &why);
            return ExitCode::from(INVALID);
        }
    };
    let mut input = io::stdin().lock();
    let mut output = io::stdout().lock();
    let mut translator = Translator::new(&tr);
    let mut buffer = vec![0; 64 * 1024];
    let mut out = Vec::with_capacity(buffer.len());
    loop {
        let n = match input.read(&mut buffer) {
            Ok(0) => break,
            Ok(n) => n,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(e) => {
                complain(&COMMAND, &run, &format!("standard input: {e}"));
                return ExitCode::from(FAILURE);
            }
        };
        out.clear();
        translator.feed(&buffer[..n], &mut out);
        if let Err(e) = output.write_all(&out) {
            return written(&run, e);
        }
    }
    out.clear();
    translator.finish(&mut out);
    match output.write_all(&out).and_then(|()| output.flush()) {
        Ok(()) => ExitCode::from(SUCCESS),
        Err(e) => written(&run, e),
    }
}

/// Reports that writing failed with `e`, unless the reader went away,
/// and fails.
fn written(run: &mikros_cli::Run, e: io::Error) -> ExitCode {
    if e.kind() != io::ErrorKind::BrokenPipe {
        complain(&COMMAND, run, &format!("standard output: {e}"));
    }
    ExitCode::from(FAILURE)
}
