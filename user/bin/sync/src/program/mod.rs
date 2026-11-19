//! sync on Mikros: it opens each path through the standard library and
//! syncs it, which the C library sends as `Sync` (MIKSPEC-0049.R11) on
//! the file's node connection. Without paths, it sends `Sync` on the
//! connection of each entry of the process's namespace (MIKSPEC-0060.R13)
//! whose `Access` lets the caller write.

use crate::targets::COMMAND;
use mikros_cli::{FAILURE, SUCCESS, complain, start};
use mikros_command::node::{self, Rights};
use std::fs::File;
use std::io;
use std::path::Path;
use std::process::ExitCode;

pub fn main() -> ExitCode {
    let run = start(&COMMAND, &mikros_command::list);
    let mut failed = false;
    if run.operands.is_empty() {
        match node::entries() {
            Ok(entries) => {
                for (root, connection) in entries {
                    let writable =
                        node::access(&connection).is_ok_and(|r| r.contains(Rights::WRITE));
                    if writable && let Err(e) = node::sync(&connection) {
                        complain(&COMMAND, &run, &format!("{}: {e:?}", root.display()));
                        failed = true;
                    }
                }
            }
            Err(e) => {
                complain(&COMMAND, &run, &format!("cannot list the namespace: {e}"));
                failed = true;
            }
        }
    }
    for path in &run.operands {
        if let Err(e) = sync(Path::new(path)) {
            complain(&COMMAND, &run, &format!("{path}: {e}"));
            failed = true;
        }
    }
    match failed {
        true => ExitCode::from(FAILURE),
        false => ExitCode::from(SUCCESS),
    }
}

fn sync(path: &Path) -> io::Result<()> {
    File::open(path)?.sync_all()
}
