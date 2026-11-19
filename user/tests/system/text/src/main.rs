//! The text commands as a system test: each check runs one from where
//! the system installs it, as a user's shell would, and looks at its exit
//! status and what it wrote, with files in a fresh folder of the
//! temporary folder that the test removes at the end. It exits with 0
//! when every check passed, and otherwise with the number, from 1, of
//! the first that failed, after writing its name and failure to standard
//! error.

mod lines;
mod matching;
mod numbers;
mod streams;

use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};

/// Where the rewrites of the port's commands are, beside the port's
/// until the switch.
const UTILITIES: &str = "/system/utilities";

/// The argument with which the test, run again, only sleeps for three
/// seconds, as a process for `tail --pid` to outlive.
pub const SLEEP: &str = "--sleep";

/// What failed, and the exit status of the command, or -1 when it did
/// not exit.
pub struct Failure {
    pub what: &'static str,
    pub status: i32,
}

pub type Check = fn() -> Result<(), Failure>;

/// What a run of a command gave.
pub struct Run {
    pub status: i32,
    pub out: String,
    pub err: String,
    /// The bytes of standard output, for output that is not text.
    pub bytes: Vec<u8>,
}

fn main() {
    if std::env::args().nth(1).as_deref() == Some(SLEEP) {
        std::thread::sleep(std::time::Duration::from_secs(3));
        return;
    }
    let folder = base();
    let made = std::fs::create_dir_all(&folder);
    let mut checks: Vec<(&str, Check)> = Vec::new();
    checks.extend(streams::checks());
    checks.extend(lines::checks());
    checks.extend(numbers::checks());
    checks.extend(matching::checks());
    let mut failed = None;
    if made.is_err() {
        eprintln!("making {} FAILED", folder.display());
        failed = Some(checks.len() as i32 + 1);
    }
    for (number, (name, check)) in checks.iter().enumerate() {
        if failed.is_some() {
            break;
        }
        if let Err(Failure { what, status }) = check() {
            eprintln!("{name} FAILED: {what} ({status})");
            failed = Some(number as i32 + 1);
        }
    }
    let _ = std::fs::remove_dir_all(&folder);
    if let Some(number) = failed {
        std::process::exit(number);
    }
}

/// The test's folder in the temporary folder.
fn base() -> PathBuf {
    let tmp = std::env::var_os("TMPDIR")
        .filter(|t| !t.is_empty())
        .map_or_else(|| PathBuf::from("/tmp"), PathBuf::from);
    tmp.join(format!("text-{}", std::process::id()))
}

/// The installed path of the utility `name`.
pub fn utility(name: &str) -> String {
    format!("{UTILITIES}/{name}")
}

/// The path of `name` in the test's folder, as a string.
pub fn path(name: &str) -> String {
    base().join(name).to_string_lossy().into_owned()
}

/// Writes `contents` to `name` in the test's folder, and returns its
/// path.
pub fn file(name: &str, contents: &[u8]) -> Result<String, Failure> {
    let path = path(name);
    std::fs::write(&path, contents).map_err(|_| Failure {
        what: "writing a file for the check",
        status: -1,
    })?;
    Ok(path)
}

/// Runs `program` with `arguments` until it exits.
pub fn run(program: &str, arguments: &[&str]) -> Result<Run, Failure> {
    run_with(program, arguments, None)
}

/// Runs `program` with `arguments` and, when there is one, `input` on
/// its standard input, until it exits.
pub fn run_with(program: &str, arguments: &[&str], input: Option<&[u8]>) -> Result<Run, Failure> {
    let failed = |what| Failure { what, status: -1 };
    let mut child = Command::new(program)
        .args(arguments)
        .stdin(if input.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|_| failed("starting the command"))?;
    if let (Some(input), Some(mut stdin)) = (input, child.stdin.take()) {
        stdin
            .write_all(input)
            .map_err(|_| failed("writing the input"))?;
    }
    let output = child
        .wait_with_output()
        .map_err(|_| failed("waiting for the command"))?;
    Ok(Run {
        status: output.status.code().unwrap_or(-1),
        out: String::from_utf8_lossy(&output.stdout).into_owned(),
        err: String::from_utf8_lossy(&output.stderr).into_owned(),
        bytes: output.stdout,
    })
}

/// Passes when `ok` holds, and otherwise fails with `what` and the
/// status of `run`.
pub fn ensure(run: &Run, what: &'static str, ok: bool) -> Result<(), Failure> {
    match ok {
        true => Ok(()),
        false => Err(Failure {
            what,
            status: run.status,
        }),
    }
}

/// Passes when `run` exited with 0, wrote nothing to standard error and
/// wrote `out`.
pub fn wrote(run: &Run, what: &'static str, out: &str) -> Result<(), Failure> {
    ensure(
        run,
        what,
        run.status == 0 && run.err.is_empty() && run.out == out,
    )
}

/// Passes when `run` exited with `status` and its error message starts
/// with `<command>: `.
pub fn refused(run: &Run, what: &'static str, command: &str, status: i32) -> Result<(), Failure> {
    let start = format!("{command}: ");
    ensure(
        run,
        what,
        run.status == status && run.err.starts_with(&start),
    )
}
