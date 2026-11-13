//! Running the tests of `/usr/bin/tests/` one at a time, each with a
//! timeout, writing a line for each and a summary, and, with
//! `--poweroff`, then powering the machine off through the service
//! manager's `mikros.service.Control`.

use crate::report::{self, Ending};
use mikros_rt::interfaces::__midl::{ClientEnd, OwnedHandle, endpoints};
use mikros_rt::interfaces::mikros::service::{
    Control, ControlClient, DirectoryClient, ShutdownKind,
};
use mikros_rt::sys;
use std::io::Write;
use std::os::unix::process::ExitStatusExt;
use std::path::Path;
use std::process::{Command, ExitStatus};
use std::time::{Duration, Instant};

/// The directory of the tests.
const TESTS: &str = "/usr/bin/tests";
/// How long a test may run.
const TIMEOUT: Duration = Duration::from_secs(60);
/// How often a running test is looked at.
const POLL: Duration = Duration::from_millis(50);
/// The status that a test which could not be started counts as, which
/// shells give a command that cannot be run.
const NOT_STARTED: i32 = 127;
const CONTROL: &str = "mikros.service.Control";
const FOREVER: i64 = i64::MAX;
const SAME_RIGHTS: u32 = 1 << 31;

pub fn main() -> i32 {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let poweroff = args.iter().any(|a| a == "--poweroff");
    let status = match report::options(&args) {
        Ok(options) => run_tests(&options.names),
        Err(e) => {
            eprintln!("selftest: {e}");
            1
        }
    };
    // An automated run ends with the machine off, whatever happened.
    if poweroff && let Err(e) = power_off() {
        eprintln!("selftest: powering off: {e}");
        return 1;
    }
    status
}

/// Runs the tests `names`, every test when there are none, and returns
/// selftest's exit status.
fn run_tests(names: &[String]) -> i32 {
    let available = match tests() {
        Ok(available) => available,
        Err(e) => {
            eprintln!("selftest: reading {TESTS}: {e}");
            return 1;
        }
    };
    let chosen = match report::selected(&available, names) {
        Ok(chosen) => chosen,
        Err(e) => {
            eprintln!("selftest: {e}");
            return 1;
        }
    };
    let mut endings = Vec::new();
    for name in &chosen {
        let ending = run(&Path::new(TESTS).join(name));
        say(&report::line(name, ending));
        endings.push(ending);
    }
    say(&report::summary(&endings));
    report::status(&endings)
}

/// Writes `line` to standard output at once.
fn say(line: &str) {
    let mut out = std::io::stdout();
    let _ = writeln!(out, "{line}");
    let _ = out.flush();
}

/// The names of the programs in `/usr/bin/tests/`, none when there is no
/// such directory.
fn tests() -> std::io::Result<Vec<String>> {
    let mut names = Vec::new();
    let entries = match std::fs::read_dir(TESTS) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(names),
        Err(e) => return Err(e),
    };
    for entry in entries {
        names.push(entry?.file_name().to_string_lossy().into_owned());
    }
    Ok(names)
}

/// Runs the test at `path` until it ends or its time is up.
fn run(path: &Path) -> Ending {
    let mut child = match Command::new(path).spawn() {
        Ok(child) => child,
        Err(e) => {
            eprintln!("selftest: starting {}: {e}", path.display());
            return Ending::Exited(NOT_STARTED);
        }
    };
    let start = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return ending(status),
            Ok(None) if start.elapsed() >= TIMEOUT => {
                let _ = child.kill();
                let _ = child.wait();
                return Ending::Timeout;
            }
            Ok(None) => std::thread::sleep(POLL),
            Err(e) => {
                eprintln!("selftest: waiting for {}: {e}", path.display());
                let _ = child.kill();
                return Ending::Exited(NOT_STARTED);
            }
        }
    }
}

fn ending(status: ExitStatus) -> Ending {
    match (status.code(), status.signal()) {
        (Some(code), _) => Ending::Exited(code),
        (None, Some(signal)) => Ending::Signal(signal),
        (None, None) => Ending::Exited(NOT_STARTED),
    }
}

/// Shuts the machine down with the kind `POWER_OFF`, through the
/// process's directory.
fn power_off() -> Result<(), String> {
    let mut raw = 0;
    let directory = std::os::mikros::directory().as_raw_handle();
    // SAFETY: the out pointer is valid for the call.
    if unsafe { sys::handle_duplicate(directory, SAME_RIGHTS, &mut raw) } != mikros_rt::OK {
        return Err("the process has no directory".into());
    }
    // SAFETY: the kernel just made the handle for us.
    let directory = DirectoryClient::new(ClientEnd::new(unsafe { OwnedHandle::from_raw(raw) }));
    let (client, server) = endpoints::<Control>().map_err(|e| format!("{e:?}"))?;
    match directory.connect(CONTROL.into(), None, server.into_handle(), FOREVER) {
        Ok(Ok(_)) => {}
        other => return Err(format!("connecting to {CONTROL}: {other:?}")),
    }
    match ControlClient::new(client).shutdown(ShutdownKind::PowerOff, FOREVER) {
        Ok(Ok(_)) => Ok(()),
        other => Err(format!("Shutdown: {other:?}")),
    }
}
