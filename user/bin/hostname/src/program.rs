//! hostname on Mikros: the C library's `gethostname` reads the host name
//! from `/system/config/hostname` (MIKSPEC-0100.R10).

use crate::name::{COMMAND, shown};
use mikros_cli::{FAILURE, SUCCESS, complain, start};
use std::ffi::{CStr, c_char};
use std::io::Write;
use std::process::ExitCode;

unsafe extern "C" {
    fn gethostname(name: *mut c_char, len: usize) -> i32;
}

pub fn main() -> ExitCode {
    let run = start(&COMMAND, &mikros_command::list);
    let name = match host_name() {
        Ok(name) => name,
        Err(e) => {
            complain(&COMMAND, &run, &format!("cannot read the host name: {e}"));
            return ExitCode::from(FAILURE);
        }
    };
    let line = format!("{}\n", shown(&name, run.has("-s")));
    match std::io::stdout().write_all(line.as_bytes()) {
        Ok(()) => ExitCode::from(SUCCESS),
        Err(_) => ExitCode::from(FAILURE),
    }
}

/// The host name, from the C library.
pub fn host_name() -> std::io::Result<String> {
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
