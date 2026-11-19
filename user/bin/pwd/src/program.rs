//! pwd on Mikros: it compares `$PWD` with the current folder through the
//! standard library, and resolves the current folder's links with the C
//! library's `realpath`.

use crate::folder::{self, COMMAND};
use mikros_cli::{EntryType, FAILURE, Paint, Role, SUCCESS, complain, start};
use std::fs;
use std::io::Write;
use std::os::unix::fs::MetadataExt;
use std::process::ExitCode;

pub fn main() -> ExitCode {
    let run = start(&COMMAND, &mikros_command::list);
    let logical = match folder::physical(&run) {
        true => None,
        false => logical(),
    };
    let path = match logical {
        Some(pwd) => pwd,
        None => match std::env::current_dir().and_then(fs::canonicalize) {
            Ok(path) => path.to_string_lossy().into_owned(),
            Err(e) => {
                complain(
                    &COMMAND,
                    &run,
                    &format!("cannot resolve the current folder: {e}"),
                );
                return ExitCode::from(FAILURE);
            }
        },
    };
    let shown = Paint::stdout(&run).paint(Role::Path(EntryType::Folder), &path);
    match writeln!(std::io::stdout(), "{shown}") {
        Ok(()) => ExitCode::from(SUCCESS),
        Err(e) => {
            complain(&COMMAND, &run, &format!("writing the folder: {e}"));
            ExitCode::from(FAILURE)
        }
    }
}

/// `$PWD`, when it may be written and names the current folder: the
/// path that the C library keeps for it, or the same node, known by a
/// device and an inode that is not 0.
fn logical() -> Option<String> {
    let pwd = std::env::var("PWD").ok().filter(|p| folder::plausible(p))?;
    if std::env::current_dir().is_ok_and(|c| c.as_os_str() == pwd.as_str()) {
        return Some(pwd);
    }
    let named = fs::metadata(&pwd).ok()?;
    let current = fs::metadata(".").ok()?;
    let same = named.is_dir()
        && named.ino() != 0
        && named.dev() == current.dev()
        && named.ino() == current.ino();
    same.then_some(pwd)
}
