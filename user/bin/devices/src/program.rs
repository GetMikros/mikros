//! devices on Mikros: it reads its arguments, pages through
//! `mikros.driver.Devices` of the device manager, and writes what it
//! got.

use crate::listing::{self, COMMAND, Device};
use mikros_cli::{FAILURE, INVALID, Paint, Run, SUCCESS, complain, start};
use mikros_command::Directory;
use mikros_rt::interfaces::mikros::driver::DevicesClient;
use std::io::Write;
use std::process::ExitCode;

const DEVICES: &str = "mikros.driver.Devices";
const FOREVER: i64 = i64::MAX;

pub fn main() -> ExitCode {
    let run = start(&COMMAND, &mikros_command::list);
    let class = match listing::class(&run) {
        Ok(class) => class,
        Err(reason) => return fail(&run, INVALID, &reason),
    };
    let devices = match list(class.as_deref()) {
        Ok(devices) => devices,
        Err(message) => return fail(&run, FAILURE, &message),
    };
    let text = match run.has("--json") {
        true => listing::json(&devices),
        false => listing::text(&devices, Paint::stdout(&run)),
    };
    let mut out = std::io::stdout().lock();
    match out.write_all(text.as_bytes()).and_then(|()| out.flush()) {
        Ok(()) => ExitCode::from(SUCCESS),
        Err(error) => fail(&run, FAILURE, &format!("cannot write the list: {error}")),
    }
}

/// Writes the error `message` to standard error and gives `status`.
fn fail(run: &Run, status: u8, message: &str) -> ExitCode {
    complain(&COMMAND, run, message);
    ExitCode::from(status)
}

/// The named devices of `class`, or of every class, page by page.
fn list(class: Option<&str>) -> Result<Vec<Device>, String> {
    let devices = connect()?;
    let mut all = Vec::new();
    let mut after: Option<String> = None;
    loop {
        let page = devices
            .list(class.map(String::from), after.clone(), FOREVER)
            .map_err(|error| format!("cannot list the devices: {error:?}"))?;
        let ended = !page.more || page.entries.is_empty();
        all.extend(page.entries.into_iter().map(|e| Device {
            class: e.class,
            name: e.name,
            description: e.description,
        }));
        if ended {
            return Ok(all);
        }
        after = all.last().map(Device::key);
    }
}

/// A client of the device manager's device list, through the service
/// directory.
fn connect() -> Result<DevicesClient, String> {
    let directory = Directory::new(std::os::mikros::directory().as_raw_handle())
        .ok_or_else(|| format!("cannot connect to {DEVICES}: no service directory"))?;
    directory.connect(DEVICES).map(DevicesClient::new)
}
