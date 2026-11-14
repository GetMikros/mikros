//! sudo on Mikros: it calls `Elevate` on the session manager's
//! `Elevation`, and once that succeeds, replaces itself with the command
//! through `Exec`.

use crate::request::{self, HELP, Request};
use mikros_cli::{FAILURE, INVALID, SUCCESS};
use mikros_rt::interfaces::__midl::{ClientEnd, OwnedHandle, endpoints};
use mikros_rt::interfaces::mikros::base::Status;
use mikros_rt::interfaces::mikros::service::DirectoryClient;
use mikros_rt::interfaces::mikros::session::{Elevation, ElevationClient};
use mikros_rt::sys;
use std::io::Write;
use std::os::unix::process::CommandExt;
use std::process::{Command, ExitCode};

const ELEVATION: &str = "mikros.session.Elevation";
const FOREVER: i64 = i64::MAX;
const SAME_RIGHTS: u32 = 1 << 31;

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
    let directory = directory().ok_or("cannot reach the service directory")?;
    let (client, server) = endpoints::<Elevation>().map_err(|e| format!("{e:?}"))?;
    match directory.connect(ELEVATION.into(), None, server.into_handle(), FOREVER) {
        Ok(Ok(_)) => {}
        _ => return Err("cannot reach the session manager".into()),
    }
    match ElevationClient::new(client).elevate(reason.into(), FOREVER) {
        Ok(Ok(_)) => Ok(()),
        Ok(Err(Status::AccessDenied)) => Err("not authorized".into()),
        Ok(Err(status)) => Err(format!("Elevate failed: {status:?}")),
        Err(error) => Err(format!("Elevate failed: {error:?}")),
    }
}

/// A client of the process's service directory.
fn directory() -> Option<DirectoryClient> {
    let mut raw = 0;
    let directory = std::os::mikros::directory().as_raw_handle();
    // SAFETY: the out pointer is valid for the call.
    if unsafe { sys::handle_duplicate(directory, SAME_RIGHTS, &mut raw) } != mikros_rt::OK {
        return None;
    }
    // SAFETY: the kernel just made the handle for us.
    Some(DirectoryClient::new(ClientEnd::new(unsafe {
        OwnedHandle::from_raw(raw)
    })))
}
