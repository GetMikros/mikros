//! ps on Mikros: it reads its arguments, lists the processes through the
//! process manager, and names their users through the C library.

use crate::table::{self, Process};
use mikros_cli::{Arguments, Command, FAILURE, INVALID, SUCCESS};
use std::collections::BTreeMap;
use std::ffi::{CStr, c_char};
use std::io::{ErrorKind, Write};
use std::os::mikros::process;
use std::process::ExitCode;

/// The error number of a process that does not exist or may not be seen.
const ESRCH: i32 = 3;
/// The name shown for user ID 0.
const SYSTEM: &str = "system";

const COMMAND: Command = Command {
    name: "ps",
    operands: "",
    most: 0,
    about: "Lists the processes, sorted by process ID, with their parent, user,\n\
            app ID and executable.",
    options: &[("--json", "write the processes as a JSON array of objects")],
};

/// musl's `struct passwd`.
#[repr(C)]
struct Passwd {
    name: *const c_char,
    password: *const c_char,
    uid: u32,
    gid: u32,
    gecos: *const c_char,
    home: *const c_char,
    shell: *const c_char,
}

unsafe extern "C" {
    fn getpwuid(uid: u32) -> *const Passwd;
}

pub fn main() -> ExitCode {
    let json = match COMMAND.parse(std::env::args().skip(1)) {
        Arguments::Run { options, .. } => options.contains(&"--json"),
        Arguments::Help => return write(&COMMAND.help()),
        Arguments::Invalid(reason) => return fail(INVALID, &reason),
    };
    let processes = match list() {
        Ok(processes) => processes,
        Err(message) => return fail(FAILURE, &message),
    };
    match json {
        true => write(&table::json(&processes)),
        false => write(&table::text(&processes)),
    }
}

/// Writes `text` to standard output.
fn write(text: &str) -> ExitCode {
    let mut out = std::io::stdout().lock();
    match out.write_all(text.as_bytes()).and_then(|()| out.flush()) {
        Ok(()) => ExitCode::from(SUCCESS),
        Err(error) if error.kind() == ErrorKind::BrokenPipe => ExitCode::from(SUCCESS),
        Err(error) => fail(FAILURE, &format!("cannot write the list: {error}")),
    }
}

/// Writes the error `message` to standard error and gives `status`.
fn fail(status: u8, message: &str) -> ExitCode {
    eprintln!("{}", COMMAND.error(message));
    ExitCode::from(status)
}

/// The processes, sorted by process ID, without those that ended before
/// they could be described.
fn list() -> Result<Vec<Process>, String> {
    let mut pids =
        process::list().map_err(|error| format!("cannot list the processes: {error}"))?;
    pids.sort_unstable();
    let mut names = BTreeMap::new();
    let mut processes = Vec::new();
    for pid in pids {
        let info = match process::info(pid) {
            Ok(info) => info,
            Err(error) if error.raw_os_error() == Some(ESRCH) => continue,
            Err(error) => return Err(format!("cannot describe process {pid}: {error}")),
        };
        let user_name = names
            .entry(info.user)
            .or_insert_with(|| user_name(info.user))
            .clone();
        processes.push(Process {
            pid: info.pid,
            parent: info.parent,
            process_group: info.process_group,
            session: info.session,
            login_session: info.login_session,
            user: info.user,
            user_name,
            app_id: info.app_id,
            path: info.path.to_string_lossy().into_owned(),
        });
    }
    Ok(processes)
}

/// The name of user `user`: `system` for 0, or its number when the
/// accounts service does not know it.
fn user_name(user: u32) -> String {
    if user == 0 {
        return SYSTEM.into();
    }
    // SAFETY: the call takes no pointers.
    let entry = unsafe { getpwuid(user) };
    // SAFETY: a non-null entry and its name stay valid until the next
    // call of getpwuid.
    match unsafe { entry.as_ref() } {
        Some(entry) if !entry.name.is_null() => unsafe { CStr::from_ptr(entry.name) }
            .to_string_lossy()
            .into_owned(),
        _ => user.to_string(),
    }
}
