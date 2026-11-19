//! echo on Mikros: it reads its leading options and writes the text to
//! standard output (MIKSPEC-0098.R45).

use crate::echoing::{self, COMMAND};
use mikros_cli::{FAILURE, INVALID, Paint, Parsed, Role, Run, SUCCESS, complain, show_help, start};
use std::io::{self, IsTerminal, Write};
use std::process::ExitCode;

pub fn main() -> ExitCode {
    let (arguments, how) = echoing::arguments(std::env::args().skip(1).collect());
    let run = begin(arguments);
    let text = echoing::text(&run.operands, how);
    let mut out = io::stdout().lock();
    match out.write_all(&text).and_then(|()| out.flush()) {
        Ok(()) => ExitCode::from(SUCCESS),
        Err(e) => {
            if e.kind() != io::ErrorKind::BrokenPipe {
                complain(&COMMAND, &run, &format!("standard output: {e}"));
            }
            ExitCode::from(FAILURE)
        }
    }
}

/// The run that `arguments` give, as `mikros_cli::start` gives it for
/// the process's own: completion mode and `--help` answer and exit,
/// invalid arguments exit with 2.
fn begin(arguments: Vec<String>) -> Run {
    if std::env::var("MSH_COMPLETE").is_ok_and(|v| v == "1") {
        return start(&COMMAND, &mikros_command::list);
    }
    match COMMAND.parse(arguments) {
        Parsed::Run(run) => run,
        Parsed::Help(path) => {
            show_help(&COMMAND.help(&path));
            std::process::exit(i32::from(SUCCESS));
        }
        Parsed::Invalid(why) => {
            let no_color = std::env::var("NO_COLOR").ok();
            let paint = Paint::new(None, io::stderr().is_terminal(), no_color.as_deref());
            eprintln!("{}: {}", COMMAND.name, paint.paint(Role::Error, &why));
            std::process::exit(i32::from(INVALID));
        }
    }
}
