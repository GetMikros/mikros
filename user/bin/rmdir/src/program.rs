//! rmdir on Mikros: it removes folders through the standard library
//! (MIKSPEC-0098.R33).

use crate::parents::{COMMAND, folders};
use mikros_cli::{EntryType, FAILURE, Paint, Role, SUCCESS, complain, start};
use std::io::Write;
use std::process::ExitCode;

pub fn main() -> ExitCode {
    let run = start(&COMMAND, &mikros_command::list);
    let paint = Paint::stdout(&run);
    let mut failed = false;
    for operand in &run.operands {
        for folder in folders(operand, run.has("-p")) {
            if let Err(e) = std::fs::remove_dir(&folder) {
                complain(&COMMAND, &run, &format!("cannot remove {folder}: {e}"));
                failed = true;
                break;
            }
            if run.has("-v") {
                let _ = writeln!(
                    std::io::stdout(),
                    "removed folder {}",
                    paint.paint(Role::Path(EntryType::Folder), &folder)
                );
            }
        }
    }
    match failed {
        true => ExitCode::from(FAILURE),
        false => ExitCode::from(SUCCESS),
    }
}
