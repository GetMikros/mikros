//! env on Mikros: it writes the changed environment, or starts the
//! command with it through the standard library, waits for it and exits
//! with its status: 127 when the command is not found and 126 when it
//! cannot be started otherwise, as POSIX gives env.

use crate::changes::{self, COMMAND, plan};
use mikros_cli::{FAILURE, INVALID, SUCCESS, complain, start};
use std::io::{ErrorKind, Write};
use std::os::unix::process::ExitStatusExt;
use std::process::{Command, ExitCode};

const NOT_FOUND: u8 = 127;
const CANNOT_RUN: u8 = 126;

pub fn main() -> ExitCode {
    let run = start(&COMMAND, &mikros_command::list);
    let plan = match plan(&run) {
        Ok(plan) => plan,
        Err(why) => {
            complain(&COMMAND, &run, &why);
            return ExitCode::from(INVALID);
        }
    };
    let Some((program, arguments)) = plan.command.split_first() else {
        let vars = changes::apply(std::env::vars_os().collect(), &plan);
        let text = changes::written(&vars, plan.null);
        return match std::io::stdout().write_all(&text) {
            Ok(()) => ExitCode::from(SUCCESS),
            Err(_) => ExitCode::from(FAILURE),
        };
    };
    let mut command = Command::new(program);
    command.args(arguments);
    if plan.clear {
        command.env_clear();
    }
    for name in &plan.unset {
        command.env_remove(name);
    }
    command.envs(plan.set.iter().map(|(n, v)| (n, v)));
    if let Some(folder) = &plan.folder {
        command.current_dir(folder);
    }
    let status = command.spawn().and_then(|mut child| child.wait());
    match status {
        Ok(status) => ExitCode::from(changes::status(status.code(), status.signal())),
        Err(e) => {
            let what = match &plan.folder {
                Some(folder) => format!("{program} in {folder}: {e}"),
                None => format!("{program}: {e}"),
            };
            complain(&COMMAND, &run, &what);
            match e.kind() {
                ErrorKind::NotFound => ExitCode::from(NOT_FOUND),
                _ => ExitCode::from(CANNOT_RUN),
            }
        }
    }
}
