//! services on Mikros: it pages through the service manager's
//! `Services`, and for a call on a service asks the session manager's
//! `Authorization` with its own identity, and once an administrator
//! allowed it, calls `ServiceControl`.

use crate::listing::{self, COMMAND, Call, Request, Service};
use mikros_cli::{FAILURE, INVALID, SUCCESS};
use mikros_command::Directory;
use mikros_rt::interfaces::mikros::service::{
    ClientIdentity, ServiceControlClient, ServicesClient,
};
use std::io::Write;
use std::process::ExitCode;

const SERVICES: &str = "mikros.service.Services";
const SERVICE_CONTROL: &str = "mikros.service.ServiceControl";
const APP_ID: &str = "system.services";
const ACTION: &str = "admin.services.control";
const FOREVER: i64 = i64::MAX;

unsafe extern "C" {
    fn getuid() -> u32;
}

pub fn main() -> ExitCode {
    let arguments: Vec<String> = std::env::args().skip(1).collect();
    let request = match listing::read(arguments.clone()) {
        Request::Help => return write(&COMMAND.help()),
        Request::Invalid(reason) => return fail(INVALID, &reason),
        request => request,
    };
    let Some(directory) = Directory::new(std::os::mikros::directory().as_raw_handle()) else {
        return fail(FAILURE, "cannot reach the service directory");
    };
    match request {
        Request::List { json } => {
            let services = match list(&directory) {
                Ok(services) => services,
                Err(why) => return fail(FAILURE, &why),
            };
            match json {
                true => write(&listing::json(&services)),
                false => write(&listing::text(&services)),
            }
        }
        Request::Call(call, name) => {
            if !authorized(&directory, &arguments) {
                return fail(FAILURE, "not authorized");
            }
            match control(&directory, call, &name) {
                Ok(()) => ExitCode::from(SUCCESS),
                Err(why) => fail(FAILURE, &why),
            }
        }
        Request::Help | Request::Invalid(_) => unreachable!("answered above"),
    }
}

/// Every service of `Services.List`, page by page.
fn list(directory: &Directory) -> Result<Vec<Service>, String> {
    let services = ServicesClient::new(directory.connect(SERVICES)?);
    let mut all = Vec::new();
    loop {
        let after = all.last().map(|s: &Service| s.name.clone());
        let page = services
            .list(after, FOREVER)
            .map_err(|e| format!("List failed: {e:?}"))?;
        all.extend(page.entries.into_iter().map(|e| Service {
            name: e.name,
            state: listing::lowercase(&format!("{:?}", e.state)),
            on_demand: e.on_demand,
            pid: e.pid,
            restarts: e.restarts,
            exit_status: e.exit_status,
        }));
        if !page.more {
            return Ok(all);
        }
    }
}

/// Calls `call` on the service `name`, and says why it failed.
fn control(directory: &Directory, call: Call, name: &str) -> Result<(), String> {
    let control = ServiceControlClient::new(directory.connect(SERVICE_CONTROL)?);
    let done = match call {
        Call::Start => control.start(name.into(), FOREVER).map(|r| r.map(drop)),
        Call::Stop => control.stop(name.into(), FOREVER).map(|r| r.map(drop)),
        Call::Restart => control.restart(name.into(), FOREVER).map(|r| r.map(drop)),
    };
    let status = match done {
        Ok(Ok(())) => return Ok(()),
        Ok(Err(status)) => listing::lowercase(&format!("{status:?}")),
        Err(error) => return Err(format!("{name}: {error:?}")),
    };
    // A service in the wrong state is named with the state it is in.
    let state = list(directory)
        .ok()
        .and_then(|all| all.into_iter().find(|s| s.name == name))
        .map(|s| s.state);
    Err(listing::failure(name, &status, state.as_deref()))
}

/// Writes `text` to standard output.
fn write(text: &str) -> ExitCode {
    match std::io::stdout().write_all(text.as_bytes()) {
        Ok(()) => ExitCode::from(SUCCESS),
        Err(_) => ExitCode::from(FAILURE),
    }
}

/// Writes the error `message` to standard error and gives `status`.
fn fail(status: u8, message: &str) -> ExitCode {
    eprintln!("{}", COMMAND.error(message));
    ExitCode::from(status)
}

/// Whether an administrator allows `admin.services.control` for this process,
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
    let reason = format!("services {}", arguments.join(" "));
    directory.authorized(me, ACTION, reason)
}
