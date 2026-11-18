//! How a utility begins: it answers msh's completion mode, prints
//! `--help`, or refuses invalid arguments, and otherwise gets its run.

use crate::colour::{Paint, Role};
use crate::complete::{Lister, candidates};
use crate::spec::{Parsed, Run, Spec};
use crate::{INVALID, SUCCESS};
use std::io::Write;

/// The environment variable that msh sets for completion mode
/// (MIKSPEC-0084.R69).
const COMPLETE: &str = "MSH_COMPLETE";

/// The run that `spec` reads from the process's arguments. In
/// completion mode, writes the candidates and exits with 0; with
/// `--help`, writes the help, through less when it does not fit the
/// terminal unless the command is less, and exits with 0; with invalid
/// arguments, writes the reason and exits with 2. `list` names the
/// system things that completion offers.
pub fn start(spec: &'static Spec, list: Lister) -> Run {
    let arguments: Vec<String> = std::env::args().skip(1).collect();
    if std::env::var(COMPLETE).is_ok_and(|v| v == "1") {
        let mut out = std::io::stdout().lock();
        for (value, what) in candidates(spec, &arguments, list) {
            let _ = match what.is_empty() {
                true => writeln!(out, "{value}"),
                false => writeln!(out, "{value}\t{what}"),
            };
        }
        std::process::exit(i32::from(SUCCESS));
    }
    match spec.parse(arguments) {
        Parsed::Run(run) => run,
        Parsed::Help(path) => {
            let help = spec.help(&path);
            match spec.pages_help {
                true => crate::pager::show(&help),
                false => {
                    let _ = std::io::stdout().write_all(help.as_bytes());
                }
            }
            std::process::exit(i32::from(SUCCESS));
        }
        Parsed::Invalid(why) => {
            let paint = Paint::new(
                None,
                std::io::IsTerminal::is_terminal(&std::io::stderr()),
                std::env::var("NO_COLOR").ok().as_deref(),
            );
            eprintln!("{}: {}", spec.name, paint.paint(Role::Error, &why));
            std::process::exit(i32::from(INVALID));
        }
    }
}

/// Writes `message` to standard error after the command's name, in the
/// error colour when standard error is coloured.
pub fn complain(spec: &Spec, run: &Run, message: &str) {
    let paint = Paint::stderr(run);
    eprintln!("{}: {}", spec.name, paint.paint(Role::Error, message));
}
