//! devices on Mikros: it reads its arguments, pages through
//! `mikros.driver.Devices` of the device manager, and writes what it
//! got.

use crate::cli::{Arguments, Command, FAILURE, INVALID, SUCCESS};
use crate::listing::{self, Device};
use mikros_rt::interfaces::__midl::{ClientEnd, OwnedHandle, endpoints};
use mikros_rt::interfaces::mikros::driver::{Devices, DevicesClient};
use mikros_rt::interfaces::mikros::service::DirectoryClient;
use mikros_rt::sys;
use std::io::Write;
use std::process::ExitCode;

const DEVICES: &str = "mikros.driver.Devices";
const FOREVER: i64 = i64::MAX;
const SAME_RIGHTS: u32 = 1 << 31;
/// The longest class that `List` takes, in bytes.
const CLASS_MAX: usize = 32;

const COMMAND: Command = Command {
    name: "devices",
    operands: "[class]",
    most: 1,
    about: "Lists the named devices, of every class or of class, each as\n\
            <class>/<name> followed by its description.",
    options: &[("--json", "write the devices as a JSON array of objects")],
};

pub fn main() -> ExitCode {
    let (json, class) = match COMMAND.parse(std::env::args().skip(1)) {
        Arguments::Run { options, operands } => {
            (options.contains(&"--json"), operands.into_iter().next())
        }
        Arguments::Help => return write(&COMMAND.help()),
        Arguments::Invalid(reason) => return fail(INVALID, &reason),
    };
    if class.as_ref().is_some_and(|c| c.len() > CLASS_MAX) {
        return fail(INVALID, &format!("a class has at most {CLASS_MAX} bytes"));
    }
    let devices = match list(class.as_deref()) {
        Ok(devices) => devices,
        Err(message) => return fail(FAILURE, &message),
    };
    if json {
        write(&listing::json(&devices))
    } else {
        write(&listing::text(&devices))
    }
}

/// Writes `text` to standard output.
fn write(text: &str) -> ExitCode {
    let mut out = std::io::stdout().lock();
    match out.write_all(text.as_bytes()).and_then(|()| out.flush()) {
        Ok(()) => ExitCode::from(SUCCESS),
        Err(error) => fail(FAILURE, &format!("cannot write the list: {error}")),
    }
}

/// Writes the error `message` to standard error and gives `status`.
fn fail(status: u8, message: &str) -> ExitCode {
    eprintln!("{}", COMMAND.error(message));
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
    let refused = |what: &str| format!("cannot connect to {DEVICES}: {what}");
    let mut raw = 0;
    let directory = std::os::mikros::directory().as_raw_handle();
    // SAFETY: the out pointer is valid for the call.
    let status = unsafe { sys::handle_duplicate(directory, SAME_RIGHTS, &mut raw) };
    if status != mikros_rt::OK {
        return Err(refused(&format!("status {status}")));
    }
    // SAFETY: the kernel just made the handle for us.
    let directory = DirectoryClient::new(ClientEnd::new(unsafe { OwnedHandle::from_raw(raw) }));
    let (client, server) = endpoints::<Devices>().map_err(|e| refused(&format!("{e:?}")))?;
    match directory.connect(DEVICES.into(), None, server.into_handle(), FOREVER) {
        Ok(Ok(_)) => Ok(DevicesClient::new(client)),
        Ok(Err(status)) => Err(refused(&format!("{status:?}"))),
        Err(error) => Err(refused(&format!("{error:?}"))),
    }
}
