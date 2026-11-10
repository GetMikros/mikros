//! The console runs: the test init serves `tests/console.img` with
//! `test=console`, or `tests/setup.img` with `test=setup`, on the test
//! machine. The session manager in that image starts the text console
//! and gives it the display and the keyboards, and the runner types
//! through the QEMU monitor. In the console run it types bob's login; in
//! the setup run, which starts with no user, it first answers the setup
//! program's questions to make the account ada, and then logs in as
//! ada. Once the user is logged in, it checks that the screen shows
//! text, and types a line that the session shell reads. The test init
//! stops QEMU through `isa-debug-exit` once the run passed or failed.

use crate::build::{self, Kind};
use crate::qemu::MachineArgs;
use crate::{image, scenario};
use anyhow::{Context, Result, bail};
use std::fs;
use std::path::Path;
use std::process::ExitStatus;
use std::time::{Duration, Instant};

const SEAT: &str = "sessionmgr: gave the console the display";
const SETUP_DONE: &str = "sessionmgr: setup exited";
/// How long the console has to open the devices it was given.
const SETTLE: Duration = Duration::from_secs(3);
/// How long the setup program has to ask its next question.
const QUESTION: Duration = Duration::from_secs(1);

/// What the runner does in a run, in order.
enum Step {
    /// Waits until the serial output holds the line.
    Wait(&'static str),
    /// Waits as long.
    Sleep(Duration),
    /// Sends the monitor command `sendkey` for each key, each line ended
    /// by Enter.
    Keys(&'static str),
    /// Dumps the screen into the file of that name in the run's
    /// directory, to look at when the run fails.
    Dump(&'static str),
}

/// The keys of the setup program's questions: ada's name, full name and
/// password twice, each ended by Enter.
const SETUP_KEYS: [&str; 4] = [
    "a d a ret",
    "shift-a d a ret",
    "a d a minus p w ret",
    "a d a minus p w ret",
];

/// Sends the monitor command `sendkey` once for each of `keys`.
fn type_keys(socket: &Path, keys: &str) -> Result<()> {
    for key in keys.split(' ') {
        scenario::monitor(socket, &format!("sendkey {key}"))?;
        std::thread::sleep(Duration::from_millis(50));
    }
    Ok(())
}

/// Why the run of `scenario` did not pass, if it did not, given QEMU's
/// exit code, the serial output and the screen dump taken in the session.
fn verdict(scenario: &str, code: Option<i32>, serial: &str, screen: &[u8]) -> Result<(), String> {
    crate::test::verdict(code, serial)?;
    let passed = format!("SCENARIO {scenario} PASSED");
    if !serial.contains(&passed) {
        return Err(format!("{passed:?} is missing"));
    }
    match scenario::console_problem(screen) {
        Some(problem) => Err(problem.into()),
        None => Ok(()),
    }
}

/// Boots the console run of the components `built` of the build kind
/// `kind` in `dir`.
pub fn run(
    machine: &MachineArgs,
    kind: Kind,
    built: &build::Built,
    dir: &Path,
    timeout: Duration,
) -> Result<()> {
    let steps = [
        Step::Wait(SEAT),
        Step::Sleep(SETTLE),
        Step::Keys("b o b ret"),
        Step::Keys("b o b minus p a s s w o r d ret"),
    ];
    session(
        machine, kind, built, dir, timeout, "console", &steps, "bob", None,
    )
}

/// Boots the setup run of the components `built` of the build kind
/// `kind` in `dir`.
pub fn run_setup(
    machine: &MachineArgs,
    kind: Kind,
    built: &build::Built,
    dir: &Path,
    timeout: Duration,
) -> Result<()> {
    let mut steps = vec![Step::Wait(SEAT), Step::Sleep(SETTLE)];
    for keys in SETUP_KEYS {
        steps.extend([Step::Keys(keys), Step::Sleep(QUESTION)]);
    }
    steps.extend([
        Step::Dump("setup.ppm"),
        Step::Wait(SETUP_DONE),
        Step::Sleep(QUESTION),
        Step::Keys("a d a ret"),
        Step::Keys("a d a minus p w ret"),
    ]);
    // The root partition holds the services that need it, which start
    // once it is available.
    let system = built
        .first_boot_image
        .as_deref()
        .context("the test build has no first-boot archive")?;
    session(
        machine,
        kind,
        built,
        dir,
        timeout,
        "setup",
        &steps,
        "ada",
        Some(system),
    )
}

/// Boots the run of `scenario` in `dir`, with the root partition made
/// from `system` when given, takes `steps`, and once `user` is logged in
/// takes a screen dump and types the line that the session shell reads.
#[allow(clippy::too_many_arguments)]
fn session(
    machine: &MachineArgs,
    kind: Kind,
    built: &build::Built,
    dir: &Path,
    timeout: Duration,
    scenario: &str,
    steps: &[Step],
    user: &str,
    system: Option<&Path>,
) -> Result<()> {
    fs::create_dir_all(dir)?;
    let command_line = format!("test={scenario}");
    let disk = dir.join("disk.img");
    match system {
        Some(system) => image::create_with_system(kind, built, system, &disk, &command_line)?,
        None => image::create_with(kind, built, &disk, &command_line, &[])?,
    }
    let log = dir.join("serial.log");
    let socket = scenario::monitor_socket(dir);
    let mut qemu = scenario::start(machine, dir, false)?;
    let deadline = Instant::now() + timeout;
    let logged_in = format!("sessionmgr: {user} logged in");
    let ended = (|| -> Result<(ExitStatus, Vec<u8>)> {
        for step in steps {
            match step {
                Step::Wait(line) => {
                    scenario::wait_for_line(&log, line, &mut qemu, deadline)?;
                }
                Step::Sleep(time) => std::thread::sleep(*time),
                Step::Keys(keys) => type_keys(&socket, keys)?,
                Step::Dump(file) => {
                    scenario::screen_dump(&socket, &dir.join(file))?;
                }
            }
        }
        scenario::wait_for_line(&log, &logged_in, &mut qemu, deadline)?;
        // The session shell writes its prompt, and the display driver
        // copies what changed after a frame's time.
        std::thread::sleep(Duration::from_secs(2));
        let screen = scenario::screen_dump(&socket, &dir.join("screen.ppm"))?;
        type_keys(&socket, "l s ret")?;
        loop {
            if let Some(status) = qemu.try_wait()? {
                return Ok((status, screen));
            }
            if Instant::now() > deadline {
                bail!("timed out waiting for the run to end");
            }
            std::thread::sleep(Duration::from_millis(100));
        }
    })();
    let _ = qemu.kill();
    let _ = qemu.wait();
    let (status, screen) = ended.with_context(|| {
        format!(
            "the {scenario} run failed; serial output in {}",
            log.display()
        )
    })?;
    let serial = String::from_utf8_lossy(&fs::read(&log).unwrap_or_default()).into_owned();
    if let Err(why) = verdict(scenario, status.code(), &serial, &screen) {
        bail!(
            "the {scenario} run failed: {why}; serial output in {}",
            log.display()
        );
    }
    Ok(())
}
