//! sudo on Mikros: it calls `Elevate` on the session manager's
//! `Elevation`, and once that succeeds, replaces itself with the command
//! through `Exec`.

use crate::request::{self, HELP, Request};
use mikros_cli::{FAILURE, INVALID, SUCCESS};
use mikros_command::Directory;
use mikros_rt::interfaces::mikros::base::Status;
use mikros_rt::interfaces::mikros::session::ElevationClient;
use std::io::Write;
use std::os::unix::process::CommandExt;
use std::process::{Command, ExitCode};

const ELEVATION: &str = "mikros.session.Elevation";
const FOREVER: i64 = i64::MAX;

pub fn main() -> ExitCode {
    let command = match request::read(std::env::args().skip(1).collect()) {
        Request::Run(command) => command,
        Request::Help => {
            let _ = std::io::stdout().write_all(HELP.as_bytes());
            return ExitCode::from(SUCCESS);
        }
        Request::Invalid(reason) => return fail(INVALID, &reason),
    };
    if let Err(why) = elevate(&request::reason(&command)) {
        return fail(FAILURE, &why);
    }
    // On success this process becomes the command, and keeps its
    // elevation.
    let error = Command::new(&command[0]).args(&command[1..]).exec();
    fail(FAILURE, &format!("cannot run {}: {error}", command[0]))
}

/// Writes the error `message` to standard error and gives `status`.
fn fail(status: u8, message: &str) -> ExitCode {
    eprintln!("sudo: {message}");
    ExitCode::from(status)
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
