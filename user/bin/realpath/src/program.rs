//! realpath on Mikros: it resolves each path against the current folder
//! and writes it, naming those that do not resolve.

use crate::paths::{self, COMMAND};
use mikros_cli::{FAILURE, Paint, Role, SUCCESS, complain, start};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

pub fn main() -> ExitCode {
    let run = start(&COMMAND, &mikros_command::list);
    let mode = paths::mode(&run);
    let current = match std::env::current_dir() {
        Ok(current) => current,
        Err(e) => {
            complain(
                &COMMAND,
                &run,
                &format!("cannot read the current folder: {e}"),
            );
            return ExitCode::from(FAILURE);
        }
    };
    let resolve = |path: &str| -> io::Result<PathBuf> {
        match run.has("-s") {
            true => paths::stripped(Path::new(path), &current, mode),
            false => paths::resolve(Path::new(path), &current, mode),
        }
    };
    let base = match run.value("--relative-to").map(resolve) {
        None => None,
        Some(Ok(base)) => Some(base),
        Some(Err(e)) => {
            let folder = run.value("--relative-to").unwrap_or_default();
            complain(&COMMAND, &run, &format!("{folder}: {e}"));
            return ExitCode::from(FAILURE);
        }
    };
    let end = if run.has("-z") { '\0' } else { '\n' };
    let paint = Paint::stdout(&run);
    let mut failed = false;
    let mut out = io::stdout().lock();
    for operand in &run.operands {
        match resolve(operand) {
            Ok(path) => {
                let path = match &base {
                    Some(base) => paths::relative(&path, base),
                    None => path,
                };
                let shown = paint.paint(Role::Path, &path.to_string_lossy());
                failed |= write!(out, "{shown}{end}").is_err();
            }
            Err(e) => {
                let _ = out.flush();
                complain(&COMMAND, &run, &format!("{operand}: {e}"));
                failed = true;
            }
        }
    }
    failed |= out.flush().is_err();
    match failed {
        true => ExitCode::from(FAILURE),
        false => ExitCode::from(SUCCESS),
    }
}
