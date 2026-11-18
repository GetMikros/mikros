//! ps on Mikros: it reads its arguments, lists the processes through the
//! process manager, and names their users through the C library.

use crate::table::{self, COMMAND, Process};
use mikros_cli::{FAILURE, Kind, Paint, Run, SUCCESS, complain, start};
use std::collections::BTreeMap;
use std::ffi::{CStr, c_char};
use std::io::{ErrorKind, Write};
use std::os::mikros::process;
use std::process::ExitCode;

/// The error number of a process that does not exist or may not be seen.
const ESRCH: i32 = 3;
/// The name shown for user ID 0.
const SYSTEM: &str = "system";

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
    let run = start(&COMMAND, &nothing);
    let processes = match list() {
        Ok(processes) => processes,
        Err(message) => return fail(&run, &message),
    };
    let text = match run.has("--json") {
        true => table::json(&processes),
        false => table::text(&processes, Paint::stdout(&run)),
    };
    let mut out = std::io::stdout().lock();
    match out.write_all(text.as_bytes()).and_then(|()| out.flush()) {
        Ok(()) => ExitCode::from(SUCCESS),
        Err(error) if error.kind() == ErrorKind::BrokenPipe => ExitCode::from(SUCCESS),
        Err(error) => fail(&run, &format!("cannot write the list: {error}")),
    }
}

/// Names of system things for completion, of which ps takes none.
fn nothing(_: Kind, _: &str) -> Vec<String> {
    Vec::new()
}

/// Writes the error `message` to standard error and gives the status of
/// a failure.
fn fail(run: &Run, message: &str) -> ExitCode {
    complain(&COMMAND, run, message);
    ExitCode::from(FAILURE)
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
