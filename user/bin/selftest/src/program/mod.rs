//! Measuring how fast the machine is, running the tests of
//! `/usr/bin/tests/` one at a time, each with a timeout scaled to that
//! speed, writing a line for each and a summary, and, with `--poweroff`,
//! then powering the machine off through the service manager's
//! `mikros.service.Control`.

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
/// How long the calibration workload takes at best on the reference
/// machine, a debug build under KVM, where it took 61 to 68 ms. It took
/// 3 s on QEMU's emulated processor, and 0.21 s in a fuzz build.
const REFERENCE: Duration = Duration::from_millis(60);
/// How many times the workload runs, of which the fastest counts.
const ROUNDS: usize = 5;
/// The workload: system calls, pages touched for the first time, and
/// round trips between two threads.
const CALLS: usize = 20_000;
const PAGES: usize = 256;
const TRIPS: usize = 1_000;
const PAGE: usize = 4096;
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
        Ok(options) => {
            let factor = options.factor.unwrap_or_else(measure);
            say(&report::factor_line(factor));
            run_tests(&options.names, report::timeout(factor))
        }
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

/// The machine's speed factor: how much longer than on the reference
/// machine the calibration workload takes at best.
fn measure() -> f64 {
    let fastest = (0..ROUNDS)
        .map(|_| workload())
        .min()
        .unwrap_or(Duration::MAX);
    report::factor(fastest, REFERENCE)
}

/// How long one run of the calibration workload takes.
fn workload() -> Duration {
    let start = Instant::now();
    let mut event = 0;
    // SAFETY: the out pointer is valid for the call.
    if unsafe { sys::event_create(0, &mut event) } == mikros_rt::OK {
        for _ in 0..CALLS {
            // SAFETY: the call takes no pointers.
            unsafe { sys::object_signal(event, 0, 0) };
        }
        // SAFETY: the call takes no pointers.
        unsafe { sys::handle_close(event) };
    }
    let mut pages = vec![0u8; PAGES * PAGE];
    for page in pages.chunks_mut(PAGE) {
        page[0] = 1;
    }
    std::hint::black_box(&pages);
    drop(pages);
    let (to_echo, echo_in) = std::sync::mpsc::channel::<usize>();
    let (echo_out, back) = std::sync::mpsc::channel::<usize>();
    let echo = std::thread::spawn(move || {
        for n in echo_in {
            if echo_out.send(n).is_err() {
                break;
            }
        }
    });
    for n in 0..TRIPS {
        if to_echo.send(n).is_err() || back.recv().is_err() {
            break;
        }
    }
    drop(to_echo);
    let _ = echo.join();
    start.elapsed()
}

/// Runs the tests `names`, every test when there are none, each for at
/// most `timeout`, and returns selftest's exit status.
fn run_tests(names: &[String], timeout: Duration) -> i32 {
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
        let ending = run(&Path::new(TESTS).join(name), timeout);
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

/// Runs the test at `path` until it ends or `timeout` is up.
fn run(path: &Path, timeout: Duration) -> Ending {
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
            Ok(None) if start.elapsed() >= timeout => {
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
