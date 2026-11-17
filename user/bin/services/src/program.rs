//! services on Mikros: it pages through the service manager's
//! `Services`, and for a call on a service calls `ServiceControl`, which
//! asks an administrator for `admin.services.control` unless the service
//! is the caller's own (MIKSPEC-0036.R52).

use crate::listing::{self, COMMAND, Call, Request, Service};
use mikros_cli::{FAILURE, INVALID, SUCCESS};
use mikros_command::Directory;
use mikros_rt::interfaces::mikros::base::Status;
use mikros_rt::interfaces::mikros::service::{ServiceControlClient, ServicesClient};
use std::io::Write;
use std::process::ExitCode;

const SERVICES: &str = "mikros.service.Services";
const SERVICE_CONTROL: &str = "mikros.service.ServiceControl";
const FOREVER: i64 = i64::MAX;

pub fn main() -> ExitCode {
    let arguments: Vec<String> = std::env::args().skip(1).collect();
    let request = match listing::read(arguments) {
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
            let control = match directory.connect_unless_denied(SERVICE_CONTROL) {
                Ok(Some(client)) => ServiceControlClient::new(client),
                Ok(None) => return fail(FAILURE, "not authorized"),
                Err(why) => return fail(FAILURE, &why),
            };
            match call_on(&directory, &control, call, &name) {
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
            enabled: e.enabled,
        }));
        if !page.more {
            return Ok(all);
        }
    }
}

/// Calls `call` on the service `name` through `control`, and says why
/// it failed.
fn call_on(
    directory: &Directory,
    control: &ServiceControlClient,
    call: Call,
    name: &str,
) -> Result<(), String> {
    let done = match call {
        Call::Start => control.start(name.into(), FOREVER).map(|r| r.map(drop)),
        Call::Stop => control.stop(name.into(), FOREVER).map(|r| r.map(drop)),
        Call::Restart => control.restart(name.into(), FOREVER).map(|r| r.map(drop)),
        Call::Enable { now } => control.enable(name.into(), FOREVER).map(|r| {
            // Then started, unless it runs already (MIKSPEC-0098.R30).
            r.map(drop).and_then(|()| match now {
                true => now_too(control.start(name.into(), FOREVER).map(|r| r.map(drop))),
                false => Ok(()),
            })
        }),
        Call::Disable { now } => control.disable(name.into(), FOREVER).map(|r| {
            r.map(drop).and_then(|()| match now {
                true => now_too(control.stop(name.into(), FOREVER).map(|r| r.map(drop))),
                false => Ok(()),
            })
        }),
    };
    let status = match done {
        Ok(Ok(())) => return Ok(()),
        Ok(Err(Status::AccessDenied)) => return Err("not authorized".into()),
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

/// The status of the `Start` or `Stop` that `--now` adds, for which a
/// service that already runs or does not run is no failure.
fn now_too(
    done: Result<Result<(), Status>, mikros_rt::interfaces::__midl::Error>,
) -> Result<(), Status> {
    match done {
        Ok(Ok(())) | Ok(Err(Status::BadState)) => Ok(()),
        Ok(Err(status)) => Err(status),
        Err(_) => Err(Status::PeerClosed),
    }
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
