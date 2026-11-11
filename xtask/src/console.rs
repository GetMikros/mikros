//! The console runs: the test init serves `tests/console.img` with
//! `test=console`, or `tests/setup.img` with `test=setup`, on the test
//! machine. The session manager in that image starts the text console
//! and gives it the display and the keyboards, and the runner types
//! through the QEMU monitor. In the console run it types bob's login; in
//! the setup run, which starts with no user, it first answers the setup
//! program's questions to make the account ada, and then logs in as
//! ada. Once the user is logged in, it checks that the screen shows
//! text. In the console run it then types a line that the session shell
//! reads; in the setup run, whose shell is msh, it starts a job, stops
//! it with Ctrl-Z, and has msh count its jobs, continue and kill the
//! job, check the host name that setup wrote and that it started in
//! ada's home, and exit with 7 when all of that worked. The test init stops
//! QEMU through `isa-debug-exit` once the run passed or failed.

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
/// How long the setup program has to ask its next question, and msh
/// to run a command.
const QUESTION: Duration = Duration::from_secs(1);
/// What the runner types in ada's msh: a job that runs until it is
/// killed, Ctrl-Z, which stops it, then `let n = (jobs | lines | len)`,
/// `bg`, `kill %1`, `if (spawn-std hostname | lines).0 != 'box' { exit 1 }`,
/// `if (pwd | lines).0 != '/Users/ada' { exit 2 }` and
/// `exit ($n + 6 + $status)`, which exits with 7 when msh listed the one
/// job, the kill worked, the host name is the one setup wrote and msh
/// started in ada's home.
const MSH_KEYS: [&str; 8] = [
    "s p a w n minus s t d spc s l e e p ret",
    "ctrl-z",
    "l e t spc n spc equal spc shift-9 j o b s spc shift-backslash spc \
     l i n e s spc shift-backslash spc l e n shift-0 ret",
    "b g ret",
    "k i l l spc shift-5 1 ret",
    "i f spc shift-9 s p a w n minus s t d spc h o s t n a m e spc shift-backslash spc \
     l i n e s shift-0 dot 0 spc shift-1 equal spc apostrophe b o x apostrophe spc \
     shift-bracket_left spc e x i t spc 1 spc shift-bracket_right ret",
    "i f spc shift-9 p w d spc shift-backslash spc l i n e s shift-0 dot 0 spc shift-1 equal spc \
     apostrophe slash shift-u s e r s slash a d a apostrophe spc \
     shift-bracket_left spc e x i t spc 2 spc shift-bracket_right ret",
    "e x i t spc shift-9 shift-4 n spc shift-equal spc 6 spc shift-equal spc \
     shift-4 s t a t u s shift-0 ret",
];

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

/// The keys of the setup program's questions: the computer's name, box,
/// then ada's name, full name and password twice, each ended by Enter.
const SETUP_KEYS: [&str; 5] = [
    "b o x ret",
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
    let after = [Step::Keys("l s ret")];
    session(
        machine, kind, built, dir, timeout, "console", &steps, "bob", &after, None,
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
    let after: Vec<Step> = MSH_KEYS
        .iter()
        .flat_map(|keys| [Step::Keys(keys), Step::Sleep(QUESTION)])
        .collect();
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
        &after,
        Some(system),
    )
}

/// Boots the run of `scenario` in `dir`, with the root partition made
/// from `system` when given, takes `steps`, and once `user` is logged in
/// takes a screen dump and then the steps `after`.
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
    after: &[Step],
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
        let take = |step: &Step, qemu: &mut std::process::Child| -> Result<()> {
            match step {
                Step::Wait(line) => {
                    scenario::wait_for_line(&log, line, qemu, deadline)?;
                }
                Step::Sleep(time) => std::thread::sleep(*time),
                Step::Keys(keys) => type_keys(&socket, keys)?,
                Step::Dump(file) => {
                    scenario::screen_dump(&socket, &dir.join(file))?;
                }
            }
            Ok(())
        };
        for step in steps {
            take(step, &mut qemu)?;
        }
        scenario::wait_for_line(&log, &logged_in, &mut qemu, deadline)?;
        // The session shell writes its prompt, and the display driver
        // copies what changed after a frame's time.
        std::thread::sleep(Duration::from_secs(2));
        let screen = scenario::screen_dump(&socket, &dir.join("screen.ppm"))?;
        for step in after {
            take(step, &mut qemu)?;
        }
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
