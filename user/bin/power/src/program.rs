//! power on Mikros: it asks the session manager's `Authorization` with
//! its own identity, and once an administrator allowed it, calls
//! `Shutdown` on the service manager's `Control`.

use crate::action::{self, Action, COMMAND};
use mikros_cli::{FAILURE, INVALID, SUCCESS};
use mikros_rt::interfaces::__midl::{ClientEnd, OwnedHandle, endpoints};
use mikros_rt::interfaces::mikros::service::{
    ClientIdentity, Control, ControlClient, DirectoryClient, ShutdownKind,
};
use mikros_rt::interfaces::mikros::session::{Authorization, AuthorizationClient};
use mikros_rt::sys;
use std::io::Write;
use std::process::ExitCode;

const AUTHORIZATION: &str = "mikros.session.Authorization";
const CONTROL: &str = "mikros.service.Control";
const APP_ID: &str = "system.power";
const ACTION: &str = "power.control";
/// The longest reason that `Authorize` takes, in bytes.
const REASON_MAX: usize = 256;
const FOREVER: i64 = i64::MAX;
const SAME_RIGHTS: u32 = 1 << 31;

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
    let Some(directory) = directory() else {
        return fail(FAILURE, "cannot reach the service directory");
    };
    if !authorized(&directory, &arguments) {
        return fail(FAILURE, "not authorized");
    }
    let (client, server) = match endpoints::<Control>() {
        Ok(ends) => ends,
        Err(error) => return fail(FAILURE, &format!("cannot make a channel: {error:?}")),
    };
    match directory.connect(CONTROL.into(), None, server.into_handle(), FOREVER) {
        Ok(Ok(_)) => {}
        other => return fail(FAILURE, &format!("cannot reach {CONTROL}: {other:?}")),
    }
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

/// Whether an administrator allows `power.control` for this process,
/// asked with the command line as the reason.
fn authorized(directory: &DirectoryClient, arguments: &[String]) -> bool {
    let Ok((client, server)) = endpoints::<Authorization>() else {
        return false;
    };
    match directory.connect(AUTHORIZATION.into(), None, server.into_handle(), FOREVER) {
        Ok(Ok(_)) => {}
        _ => return false,
    }
    let mut reason = format!("power {}", arguments.join(" "));
    while reason.len() > REASON_MAX {
        reason.pop();
    }
    let me = ClientIdentity {
        pid: std::process::id(),
        // SAFETY: the call takes no pointers.
        user: unsafe { getuid() },
        app_id: APP_ID.into(),
        admin: 0,
    };
    let answer = AuthorizationClient::new(client).authorize(me, ACTION.into(), reason, FOREVER);
    matches!(answer, Ok(Ok(a)) if a.allowed)
}
