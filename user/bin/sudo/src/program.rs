//! sudo on Mikros: it calls `Elevate` on the session manager's
//! `Elevation`, and once that succeeds, replaces itself with the command
//! through `Exec`.

use crate::request::{self, COMMAND};
use mikros_cli::{FAILURE, Run, complain, start};
use mikros_command::Directory;
use mikros_rt::interfaces::mikros::base::Status;
use mikros_rt::interfaces::mikros::session::ElevationClient;
use std::os::unix::process::CommandExt;
use std::process::{Command, ExitCode};

const ELEVATION: &str = "mikros.session.Elevation";
const FOREVER: i64 = i64::MAX;

pub fn main() -> ExitCode {
    let run = start(&COMMAND, &mikros_command::list);
    let command = &run.operands;
    if let Err(why) = elevate(&request::reason(command)) {
        return fail(&run, &why);
    }
    // On success this process becomes the command, and keeps its
    // elevation.
    let error = Command::new(&command[0]).args(&command[1..]).exec();
    fail(&run, &format!("cannot run {}: {error}", command[0]))
}

/// Writes the error `message` to standard error and gives the status of
/// a failure.
fn fail(run: &Run, message: &str) -> ExitCode {
    complain(&COMMAND, run, message);
    ExitCode::from(FAILURE)
}

/// Asks the session manager to elevate this process, giving `reason`,
/// and why it did not.
fn elevate(reason: &str) -> Result<(), String> {
    let directory = Directory::new(std::os::mikros::directory().as_raw_handle())
        .ok_or("cannot reach the service directory")?;
    let client = directory
        .connect(ELEVATION)
        .map_err(|_| "cannot reach the session manager")?;
    match ElevationClient::new(client).elevate(reason.into(), FOREVER) {
        Ok(Ok(_)) => Ok(()),
        Ok(Err(Status::AccessDenied)) => Err("not authorized".into()),
        Ok(Err(status)) => Err(format!("Elevate failed: {status:?}")),
        Err(error) => Err(format!("Elevate failed: {error:?}")),
    }
}
