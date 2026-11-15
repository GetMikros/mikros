//! power on Mikros: it asks the session manager's `Authorization` with
//! its own identity, and once an administrator allowed it, calls
//! `Shutdown` on the service manager's `Control`.

use crate::action::{self, Action, COMMAND};
use mikros_cli::{FAILURE, INVALID, SUCCESS};
use mikros_command::Directory;
use mikros_rt::interfaces::mikros::service::{ClientIdentity, ControlClient, ShutdownKind};
use std::io::Write;
use std::process::ExitCode;

const CONTROL: &str = "mikros.service.Control";
const APP_ID: &str = "system.power";
const ACTION: &str = "power.control";
const FOREVER: i64 = i64::MAX;

unsafe extern "C" {
    fn getuid() -> u32;
}

pub fn main() -> ExitCode {
    let arguments: Vec<String> = std::env::args().skip(1).collect();
    let kind = match action::read(arguments.clone()) {
        Action::Off => ShutdownKind::PowerOff,
        Action::Restart => ShutdownKind::Restart,
        Action::Help => {
            let _ = std::io::stdout().write_all(COMMAND.help().as_bytes());
            return ExitCode::from(SUCCESS);
        }
        Action::Invalid(reason) => return fail(INVALID, &reason),
    };
    let Some(directory) = Directory::new(std::os::mikros::directory().as_raw_handle()) else {
        return fail(FAILURE, "cannot reach the service directory");
    };
    if !authorized(&directory, &arguments) {
        return fail(FAILURE, "not authorized");
    }
    let client = match directory.connect(CONTROL) {
        Ok(client) => client,
        Err(why) => return fail(FAILURE, &why),
    };
    // On success the machine goes down before a reply comes.
    match ControlClient::new(client).shutdown(kind, FOREVER) {
        Ok(Ok(_)) => ExitCode::from(SUCCESS),
        Ok(Err(status)) => fail(FAILURE, &format!("Shutdown failed: {status:?}")),
        Err(error) => fail(FAILURE, &format!("Shutdown failed: {error:?}")),
    }
}

/// Writes the error `message` to standard error and gives `status`.
fn fail(status: u8, message: &str) -> ExitCode {
    eprintln!("{}", COMMAND.error(message));
    ExitCode::from(status)
}

/// Whether an administrator allows `power.control` for this process,
/// asked with the command line as the reason.
fn authorized(directory: &Directory, arguments: &[String]) -> bool {
    let me = ClientIdentity {
        pid: std::process::id(),
        // SAFETY: the call takes no pointers.
        user: unsafe { getuid() },
        app_id: APP_ID.into(),
        admin: 0,
        koid: 0,
    };
    let reason = format!("power {}", arguments.join(" "));
    directory.authorized(me, ACTION, reason)
}
