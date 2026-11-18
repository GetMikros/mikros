//! readlink on Mikros: it reads each link, or resolves each path against
//! the current folder, and writes it, naming those that it cannot.

use crate::links::{self, COMMAND, Mode};
use mikros_cli::{FAILURE, Paint, Role, SUCCESS, complain, start};
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

pub fn main() -> ExitCode {
    let run = start(&COMMAND, &mikros_command::list);
    let mode = links::mode(&run);
    let (end, ignored) = links::ending(run.has("-z"), run.has("-n"), run.operands.len());
    if ignored {
        complain(&COMMAND, &run, "ignoring -n with several paths");
    }
    let paint = Paint::stdout(&run);
    let mut failed = false;
    let mut out = io::stdout().lock();
    for operand in &run.operands {
        match written(Path::new(operand), mode) {
            Ok(path) => {
                let shown = paint.paint(Role::Path, &path.to_string_lossy());
                failed |= write!(out, "{shown}{end}").is_err();
            }
            Err(why) => {
                let _ = out.flush();
                complain(&COMMAND, &run, &format!("{operand}: {why}"));
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

/// What is written for `path` in `mode`, or why nothing is.
fn written(path: &Path, mode: Mode) -> Result<PathBuf, String> {
    if mode != Mode::Target {
        let current =
            std::env::current_dir().map_err(|e| format!("cannot read the current folder: {e}"))?;
        return links::resolve(path, &current, mode).map_err(|e| e.to_string());
    }
    fs::read_link(path).map_err(|e| match fs::symlink_metadata(path) {
        Ok(m) if !m.file_type().is_symlink() => "not a link".to_string(),
        _ => e.to_string(),
    })
}
