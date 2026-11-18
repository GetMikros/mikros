//! power on Mikros: it calls `Shutdown` on the service manager's
//! `Control`, whose connection needs `user.power.control`, for which the
//! service manager asks the user, or `admin.power.control`
//! (MIKSPEC-0098.R29).

use crate::action::{self, Action, COMMAND};
use mikros_cli::{FAILURE, INVALID, SUCCESS, complain, start};
use mikros_command::Directory;
use mikros_rt::interfaces::mikros::base::Status;
use mikros_rt::interfaces::mikros::service::{ControlClient, ShutdownKind};
use std::process::ExitCode;

const CONTROL: &str = "mikros.service.Control";
const FOREVER: i64 = i64::MAX;

pub fn main() -> ExitCode {
    let run = start(&COMMAND, &mikros_command::list);
    let fail = |status: u8, message: &str| {
        complain(&COMMAND, &run, message);
        ExitCode::from(status)
    };
    let kind = match action::read(&run) {
        Ok(Action::Off) => ShutdownKind::PowerOff,
        Ok(Action::Restart) => ShutdownKind::Restart,
        Err(reason) => return fail(INVALID, &reason),
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
        Ok(Err(Status::AccessDenied)) => fail(FAILURE, "not authorized"),
        Ok(Err(status)) => fail(FAILURE, &format!("Shutdown failed: {status:?}")),
        Err(error) => fail(FAILURE, &format!("Shutdown failed: {error:?}")),
    }
}
