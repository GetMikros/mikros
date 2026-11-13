//! The automated run of selftest in a test image. When the kernel command
//! line asks for it with the word `selftest` or `selftest=<names>`, the
//! process manager gives the service manager that word as its argument;
//! once the services that start at boot run, the service manager starts
//! `/usr/bin/selftest --poweroff`, with those names, as `system.selftest`,
//! with its standard output and error going to the log service. Without
//! the program, the system boots as usual.

use crate::launch;
use crate::supervisor::Supervisor;
use alloc::string::String;
use alloc::vec::Vec;
use mikros_rt::interfaces::mikros::base::Status;
use mikros_rt::interfaces::mikros::process::{HandleRole, SpawnRequest, StartupHandle};
use mikros_rt::sys;

const PROGRAM: &str = "/usr/bin/selftest";
const APP_ID: &str = "system.selftest";
/// The bytes of the arguments read: their counts, and enough for the
/// service manager's own path and the word.
const ARGUMENTS_MAX: usize = 4096;

/// The tests that the arguments in the memory object `arguments` ask
/// selftest to run, none for every test, or `None` when they ask for no
/// run.
pub fn requested(arguments: sys::Handle) -> Option<Vec<String>> {
    let mut bytes = alloc::vec![0u8; ARGUMENTS_MAX];
    // SAFETY: the pointer and length describe `bytes`.
    let status = unsafe { sys::mem_read(arguments, 0, bytes.as_mut_ptr(), bytes.len() as u64) };
    if status != mikros_rt::OK {
        return None;
    }
    let count = u32::from_le_bytes(bytes[..4].try_into().ok()?) as usize;
    // The program's own path comes first.
    for arg in bytes[8..].split(|b| *b == 0).take(count).skip(1) {
        match core::str::from_utf8(arg) {
            Ok("selftest") => return Some(Vec::new()),
            Ok(arg) => {
                if let Some(names) = arg.strip_prefix("selftest=") {
                    return Some(
                        names
                            .split(',')
                            .filter(|n| !n.is_empty())
                            .map(String::from)
                            .collect(),
                    );
                }
            }
            Err(_) => {}
        }
    }
    None
}

/// Starts selftest for the tests `names`, every test when there are
/// none, through the launcher of `supervisor`.
pub async fn start(supervisor: &Supervisor, names: &[String]) {
    let streams = launch::stream().and_then(|o| Ok((o, launch::stream()?)));
    let ((output, output_end), (error, error_end)) = match streams {
        Ok(streams) => streams,
        Err(e) => {
            mikros_rt::log!(Error, "selftest did not start: {e}");
            return;
        }
    };
    let mut args = alloc::vec![PROGRAM, "--poweroff"];
    args.extend(names.iter().map(String::as_str));
    let arguments = match launch::arguments(&args) {
        Ok(arguments) => arguments,
        Err(status) => {
            mikros_rt::log!(Error, "selftest did not start: arguments: {status}");
            return;
        }
    };
    let request = SpawnRequest {
        path: PROGRAM.into(),
        arguments,
        handles: alloc::vec![
            StartupHandle {
                role: HandleRole::Stdout,
                id: launch::STDOUT_ID,
                handle: output,
            },
            StartupHandle {
                role: HandleRole::Stderr,
                id: launch::STDERR_ID,
                handle: error,
            },
        ],
        app_id: Some(APP_ID.into()),
        user: launch::CALLER_USER,
        login_session: false,
        new_session: false,
        working_directory: None,
        umask: u32::MAX,
    };
    match supervisor.launcher.spawn(request).await {
        Ok(Ok(spawned)) => {
            mikros_rt::log!(Info, "started selftest as process {}", spawned.pid);
            supervisor
                .sources
                .add(spawned.pid, APP_ID.into(), output_end, error_end);
        }
        Ok(Err(Status::NotFound)) => {
            mikros_rt::log!(Info, "there is no {PROGRAM}; the system boots as usual");
        }
        Ok(Err(status)) => mikros_rt::log!(Error, "selftest did not start: {status:?}"),
        Err(e) => mikros_rt::log!(Error, "selftest did not start: {e:?}"),
    }
}
