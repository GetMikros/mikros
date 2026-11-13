//! The console runs: the test init serves `tests/console.img` with
//! `test=console`, or `tests/setup.img` with `test=setup`, on the test
//! machine. The session manager in that image starts the text console
//! and gives it the display and the keyboards, and the runner types
//! through the QEMU monitor. In the console run it types bob's login; in
//! the setup run, which starts with no user, it first answers the setup
//! program's questions to make the account ada, and then logs in as
//! ada. Before the login, at the prompt or in setup, the screen must
//! show the log above them; once the user is logged in, it must show the
//! session's text and no log, and while the runner holds Ctrl, Alt and
//! F12, the log on the whole screen. In the console run it then types a line that the session shell
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
/// How long msh has to start `spawn-std sleep` and give it the terminal
/// before the runner types Ctrl-Z. Until then the shell's own process
/// group is in the terminal's foreground, and a Ctrl-Z goes to the shell,
/// which ignores it, as other shells do; the job then runs on in the
/// foreground and takes the rest of the input. While the command word is
/// typed, msh lists the programs of `PATH` again for its suggestion after
/// each key, so in a debug build the line reaches it about 1 s after the
/// last key; starting the job and handing it the terminal takes about
/// 70 ms more.
const JOB_START: Duration = Duration::from_secs(5);
/// What the runner types in ada's msh: `spaw`, which Tab completes to
/// `spawn-std `, and `sleep`, a job that runs until it is killed,
/// Ctrl-Z, which stops it, then `let n = (jobs | lines | len)`, `bg`,
/// `kill %1`, `if (spawn-std hostname | lines).0 != 'box' { exit 1 }`,
/// `if (pwd | lines).0 != '/Users/ada' { exit 2 }`,
/// `try { files-c . } catch e { exit 5 }`, which makes, changes and
/// removes files in ada's home,
/// `try { coreutils-check . } catch e { exit 6 }`, which runs the
/// command-line utilities there,
/// `cd /; if ((print /*) !~ 'Users') { exit 4 }` and
/// `exit ($n + 6 + $status)`, which exits with 7 when msh listed the one
/// job, the kill worked, the host name is the one setup wrote, msh
/// started in ada's home, files-c's and coreutils-check's checks
/// passed, and `/`, which no namespace entry covers, is a folder that
/// lists `Users`.
const MSH_KEYS: [&str; 11] = [
    "s p a w tab s l e e p ret",
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
    "t r y spc shift-bracket_left spc f i l e s minus c spc dot spc shift-bracket_right spc \
     c a t c h spc e spc shift-bracket_left spc e x i t spc 5 spc shift-bracket_right ret",
    "t r y spc shift-bracket_left spc c o r e u t i l s minus c h e c k spc dot spc \
     shift-bracket_right spc c a t c h spc e spc shift-bracket_left spc e x i t spc 6 spc \
     shift-bracket_right ret",
    "c d spc slash semicolon spc i f spc shift-9 shift-9 p r i n t spc slash shift-8 shift-0 spc \
     shift-1 shift-grave_accent spc apostrophe shift-u s e r s apostrophe shift-0 spc \
     shift-bracket_left spc e x i t spc 4 spc shift-bracket_right ret",
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
    /// Sends the monitor command `sendkey` for the keys with a hold time
    /// of as many milliseconds, and returns at once. QEMU waits that
    /// long after pressing each key, and after releasing each, the last
    /// first: of `n` keys, all are held from `n - 1` hold times on to `n`.
    Hold(&'static str, u32),
    /// Dumps the screen into the file of that name in the run's
    /// directory, which must show the log where it says.
    Log(&'static str, Shown),
}

/// Where a screen dump must show the log.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Shown {
    /// Above the login prompt or setup's terminal.
    AbovePrompt,
    /// On the whole screen.
    Whole,
    /// Not at all: a session's terminal fills the screen.
    Session,
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

/// Why the screen `ppm` does not show the log above the prompt, if it
/// does not. The log's lines start on the top row, and it takes all but
/// the bottom eight rows, so its top quarter shows text.
fn log_problem(ppm: &[u8]) -> Option<&'static str> {
    let pixels = scenario::ppm_pixels(ppm)?;
    let top = &pixels[..pixels.len() / 4];
    top.iter()
        .all(|&p| p == top[0])
        .then_some("the screen shows no log above the prompt")
}

/// Why the screen `ppm`, of a session that has written a line or two,
/// shows the log, if it does: its bottom half has text.
fn session_problem(ppm: &[u8]) -> Option<&'static str> {
    let pixels = scenario::ppm_pixels(ppm)?;
    let bottom = &pixels[pixels.len() / 2..];
    (!bottom.iter().all(|&p| p == bottom[0]))
        .then_some("the session's screen shows more than its text")
}

/// Why the screen `ppm` does not show the log where `shown` says, if it
/// does not.
fn shown_problem(ppm: &[u8], shown: Shown) -> Option<&'static str> {
    if shown == Shown::Session {
        return scenario::console_problem(ppm).or_else(|| session_problem(ppm));
    }
    scenario::console_problem(ppm)
        .or_else(|| log_problem(ppm))
        .or_else(|| match shown {
            Shown::AbovePrompt | Shown::Session => None,
            Shown::Whole => session_problem(ppm)
                .is_none()
                .then_some("the log does not fill the screen"),
        })
}

/// Why the run of `scenario` did not pass, if it did not, given QEMU's
/// exit code, the serial output, the screen dump taken in the session,
/// and those that must show the log, by file name.
fn verdict(
    scenario: &str,
    code: Option<i32>,
    serial: &str,
    screen: &[u8],
    logs: &[(&str, Shown, Vec<u8>)],
) -> Result<(), String> {
    crate::test::verdict(code, serial)?;
    let passed = format!("SCENARIO {scenario} PASSED");
    if !serial.contains(&passed) {
        return Err(format!("{passed:?} is missing"));
    }
    if let Some(problem) = scenario::console_problem(screen).or_else(|| session_problem(screen)) {
        return Err(problem.into());
    }
    for (file, shown, ppm) in logs {
        if let Some(problem) = shown_problem(ppm, *shown) {
            return Err(format!("{file}: {problem}"));
        }
    }
    Ok(())
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
        Step::Log("login.ppm", Shown::AbovePrompt),
        Step::Keys("b o b ret"),
        Step::Keys("b o b minus p a s s w o r d ret"),
    ];
    // Ctrl, Alt and F12 are all held from 4 to 6 seconds on, and all
    // released by 10. Then console 2 shows its own login prompt below the
    // log, cy logs in there while bob's session goes on, and console 1
    // shows bob's session again, whose shell then gets its line.
    let after = [
        Step::Hold("ctrl-alt-f12", 2000),
        Step::Sleep(Duration::from_secs(5)),
        Step::Log("logview.ppm", Shown::Whole),
        Step::Sleep(Duration::from_secs(6)),
        Step::Keys("ctrl-alt-f2"),
        Step::Sleep(SETTLE),
        Step::Log("console2.ppm", Shown::AbovePrompt),
        Step::Keys("c y ret"),
        Step::Keys("c y minus p a s s w o r d ret"),
        Step::Wait("sessionmgr: cy logged in on console 2"),
        Step::Keys("ctrl-alt-f1"),
        Step::Sleep(SETTLE),
        Step::Log("console1.ppm", Shown::Session),
        Step::Keys("l s ret"),
    ];
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
        Step::Log("setup.ppm", Shown::AbovePrompt),
        Step::Wait(SETUP_DONE),
        Step::Sleep(QUESTION),
        Step::Keys("a d a ret"),
        Step::Keys("a d a minus p w ret"),
    ]);
    // The first keys start the job that Ctrl-Z stops next.
    let after: Vec<Step> = MSH_KEYS
        .iter()
        .enumerate()
        .flat_map(|(i, keys)| {
            let wait = if i == 0 { JOB_START } else { QUESTION };
            [Step::Keys(keys), Step::Sleep(wait)]
        })
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
    let mut logs = Vec::new();
    let ended = (|| -> Result<(ExitStatus, Vec<u8>)> {
        let mut take = |step: &Step, qemu: &mut std::process::Child| -> Result<()> {
            match step {
                Step::Wait(line) => {
                    scenario::wait_for_line(&log, line, qemu, deadline)?;
                }
                Step::Sleep(time) => std::thread::sleep(*time),
                Step::Keys(keys) => type_keys(&socket, keys)?,
                Step::Hold(keys, ms) => {
                    scenario::monitor(&socket, &format!("sendkey {keys} {ms}"))?;
                }
                Step::Log(file, shown) => {
                    let ppm = scenario::screen_dump(&socket, &dir.join(file))?;
                    logs.push((*file, *shown, ppm));
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
    if let Err(why) = verdict(scenario, status.code(), &serial, &screen, &logs) {
        bail!(
            "the {scenario} run failed: {why}; serial output in {}",
            log.display()
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A screen of 1 by 8 pixels, top first.
    fn ppm(rows: [[u8; 3]; 8]) -> Vec<u8> {
        let mut ppm = b"P6\n1 8\n255\n".to_vec();
        ppm.extend(rows.iter().flatten());
        ppm
    }

    const DARK: [u8; 3] = [0, 0, 0];
    const LIGHT: [u8; 3] = [200, 200, 200];

    #[test]
    fn the_log_shows_at_the_top_and_not_in_a_session() {
        let log_and_prompt = ppm([LIGHT, DARK, LIGHT, DARK, DARK, DARK, LIGHT, DARK]);
        let session = ppm([LIGHT, DARK, DARK, DARK, DARK, DARK, DARK, DARK]);
        assert_eq!(log_problem(&log_and_prompt), None);
        let prompt_only = ppm([DARK, DARK, DARK, DARK, DARK, DARK, LIGHT, DARK]);
        assert!(log_problem(&prompt_only).is_some());
        assert_eq!(session_problem(&session), None);
        assert!(session_problem(&log_and_prompt).is_some());
        let serial = "SCENARIO console PASSED";
        let code = Some(crate::test::PASSED);
        let whole = ppm([LIGHT, DARK, LIGHT, DARK, LIGHT, DARK, LIGHT, DARK]);
        let logs = [
            ("login.ppm", Shown::AbovePrompt, log_and_prompt.clone()),
            ("logview.ppm", Shown::Whole, whole),
        ];
        assert_eq!(verdict("console", code, serial, &session, &logs), Ok(()));
        assert!(verdict("console", code, serial, &log_and_prompt, &logs).is_err());
        let blank = [("login.ppm", Shown::AbovePrompt, ppm([DARK; 8]))];
        assert!(verdict("console", code, serial, &session, &blank).is_err());
        // The log above the prompt does not fill the screen.
        let above = [("logview.ppm", Shown::Whole, session.clone())];
        assert!(verdict("console", code, serial, &session, &above).is_err());
    }
}
