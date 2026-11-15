//! power on Mikros: it calls `Shutdown` on the service manager's
//! `Control`, whose connection needs `admin.power.control`, for which the
//! directory asks an administrator.

use crate::action::{self, Action, COMMAND};
use mikros_cli::{FAILURE, INVALID, SUCCESS};
use mikros_command::Directory;
use mikros_rt::interfaces::mikros::service::{ControlClient, ShutdownKind};
use std::io::Write;
use std::process::ExitCode;

const CONTROL: &str = "mikros.service.Control";
const FOREVER: i64 = i64::MAX;

pub fn main() -> ExitCode {
    let arguments: Vec<String> = std::env::args().skip(1).collect();
    let kind = match action::read(arguments) {
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
    let client = match directory.connect_unless_denied(CONTROL) {
        Ok(Some(client)) => client,
        Ok(None) => return fail(FAILURE, "not authorized"),
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
