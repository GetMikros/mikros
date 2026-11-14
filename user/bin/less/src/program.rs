//! less on Mikros: it copies the input to standard output, which paging
//! on a terminal is to replace there.

use mikros_cli::{Arguments, Command, FAILURE, INVALID, SUCCESS};
use std::io::{Read, Write};
use std::process::ExitCode;

const COMMAND: Command = Command {
    name: "less",
    operands: "[file]",
    most: 1,
    about: "Copies the file, or standard input when no file is named, to\n\
            standard output.",
    options: &[],
};

pub fn main() -> ExitCode {
    let file = match COMMAND.parse(std::env::args().skip(1)) {
        Arguments::Run { operands, .. } => operands.into_iter().next(),
        Arguments::Help => {
            let _ = std::io::stdout().write_all(COMMAND.help().as_bytes());
            return ExitCode::from(SUCCESS);
        }
        Arguments::Invalid(reason) => return fail(INVALID, &reason),
    };
    let (input, name): (Box<dyn Read + Send>, String) = match file {
        Some(path) => match std::fs::File::open(&path) {
            Ok(file) => (Box::new(file), path),
            Err(error) => return fail(FAILURE, &format!("cannot open {path}: {error}")),
        },
        None => (Box::new(std::io::stdin()), "(standard input)".into()),
    };
    copy(input, &name)
}

/// Writes the error `message` to standard error and gives `status`.
fn fail(status: u8, message: &str) -> ExitCode {
    eprintln!("{}", COMMAND.error(message));
    ExitCode::from(status)
}

/// Copies `input` to standard output as it is.
fn copy(mut input: Box<dyn Read + Send>, name: &str) -> ExitCode {
    let mut out = std::io::stdout().lock();
    match std::io::copy(&mut input, &mut out).and_then(|_| out.flush()) {
        Ok(()) => ExitCode::from(SUCCESS),
        Err(error) => fail(FAILURE, &format!("cannot copy {name}: {error}")),
    }
}
