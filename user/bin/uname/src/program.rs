//! uname on Mikros: the C library's `gethostname` gives the host name,
//! and bootsvc's `mikros.boot.Version` the booted version
//! (MIKSPEC-0099.R9), which it asks only when the output shows it.

use crate::system::{self, COMMAND, Facts, Field};
use mikros_cli::{FAILURE, SUCCESS, complain, start};
use mikros_command::Directory;
use mikros_rt::interfaces::mikros::boot::VersionClient;
use std::ffi::{CStr, c_char};
use std::io::Write;
use std::process::ExitCode;

const VERSION: &str = "mikros.boot.Version";
const FOREVER: i64 = i64::MAX;

unsafe extern "C" {
    fn gethostname(name: *mut c_char, len: usize) -> i32;
}

pub fn main() -> ExitCode {
    let run = start(&COMMAND, &mikros_command::list);
    let json = run.has("--json");
    let fields = system::fields(&run);
    let fail = |message: &str| {
        complain(&COMMAND, &run, message);
        ExitCode::from(FAILURE)
    };
    let node = match json || fields.contains(&Field::Node) {
        true => match host_name() {
            Ok(node) => node,
            Err(e) => return fail(&format!("cannot read the host name: {e}")),
        },
        false => String::new(),
    };
    let wants_version = fields.contains(&Field::Release) || fields.contains(&Field::Version);
    let version = match json || wants_version {
        true => match booted_version() {
            Ok(version) => version,
            Err(why) => return fail(&why),
        },
        false => None,
    };
    let facts = Facts { node, version };
    let text = match json {
        true => system::json(&facts),
        false => system::line(&fields, &facts),
    };
    match std::io::stdout().write_all(text.as_bytes()) {
        Ok(()) => ExitCode::from(SUCCESS),
        Err(_) => ExitCode::from(FAILURE),
    }
}

/// The host name, from the C library.
fn host_name() -> std::io::Result<String> {
    let mut buf = [0 as c_char; 256];
    // SAFETY: the buffer is valid for its length, of which the last byte
    // stays NUL.
    if unsafe { gethostname(buf.as_mut_ptr(), buf.len() - 1) } != 0 {
        return Err(std::io::Error::last_os_error());
    }
    // SAFETY: the buffer ends in a NUL byte.
    let name = unsafe { CStr::from_ptr(buf.as_ptr()) };
    Ok(name.to_string_lossy().into_owned())
}

/// The booted version from bootsvc, `None` when it knows none.
fn booted_version() -> Result<Option<String>, String> {
    let directory = Directory::new(std::os::mikros::directory().as_raw_handle())
        .ok_or("cannot reach the service directory")?;
    let version = VersionClient::new(directory.connect(VERSION)?)
        .get(FOREVER)
        .map_err(|e| format!("Get of {VERSION} failed: {e:?}"))?
        .version;
    Ok(Some(version).filter(|v| !v.is_empty()))
}
