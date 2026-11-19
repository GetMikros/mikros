//! The file commands that show and keep files, as a system test: stat,
//! chmod, du, df, dd and sync. Each check runs one from where the system
//! installs it, in a folder of its own under `$TMPDIR`, or `/tmp`, and
//! looks at its exit status, what it wrote and the files it left. It
//! exits with 0 when every check passed, and otherwise with the number,
//! from 1, of the first that failed, after writing its name and failure
//! to standard error.

mod blocks;
mod facts;
mod modes;
mod space;

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// What failed, and the exit status of the command, or -1 when it did not
/// exit or the check failed before running it.
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
    pub bytes: Vec<u8>,
}

fn main() {
    let mut checks: Vec<(&str, Check)> = Vec::new();
    checks.extend_from_slice(facts::CHECKS);
    checks.extend_from_slice(modes::CHECKS);
    checks.extend_from_slice(space::CHECKS);
    checks.extend_from_slice(blocks::CHECKS);
    for (number, (name, check)) in checks.iter().enumerate() {
        if let Err(Failure { what, status }) = check() {
            eprintln!("{name} FAILED: {what} ({status})");
            std::process::exit(number as i32 + 1);
        }
    }
}

/// Where the system installs the rewrite of `command`, beside the port's
/// until the switch.
pub fn utility(command: &str) -> String {
    format!("/system/utilities/{command}")
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

/// Fails with `what` unless `ok` holds, for what a check looks at
/// itself, such as the files a command left.
pub fn holds(what: &'static str, ok: bool) -> Result<(), Failure> {
    match ok {
        true => Ok(()),
        false => Err(Failure { what, status: -1 }),
    }
}

/// A new, empty folder for the check `name`, which `Scratch` removes
/// when dropped.
pub fn scratch(name: &str) -> Result<Scratch, Failure> {
    let base = std::env::var("TMPDIR").unwrap_or_else(|_| "/tmp".into());
    let dir = Path::new(&base).join(format!("files-info-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).map_err(|_| Failure {
        what: "making the folder",
        status: -1,
    })?;
    Ok(Scratch(dir))
}

/// A check's folder, removed with what it holds at the end.
pub struct Scratch(PathBuf);

impl Scratch {
    /// The folder's own path, as text for arguments.
    pub fn path(&self) -> String {
        self.0.to_string_lossy().into_owned()
    }

    /// The path of `name` in the folder, as text for arguments.
    pub fn at(&self, name: &str) -> String {
        self.0.join(name).to_string_lossy().into_owned()
    }

    /// Writes `contents` to the file `name` in the folder.
    pub fn write(&self, name: &str, contents: &[u8]) -> Result<String, Failure> {
        let path = self.at(name);
        std::fs::write(&path, contents).map_err(|_| Failure {
            what: "writing a file",
            status: -1,
        })?;
        Ok(path)
    }

    /// Makes the folder `name` in the folder.
    pub fn folder(&self, name: &str) -> Result<String, Failure> {
        let path = self.at(name);
        std::fs::create_dir_all(&path).map_err(|_| Failure {
            what: "making a folder",
            status: -1,
        })?;
        Ok(path)
    }

    /// The contents of the file `name` in the folder, if it can be read.
    pub fn read(&self, name: &str) -> Option<Vec<u8>> {
        std::fs::read(self.0.join(name)).ok()
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// `len` bytes that differ from one to the next.
pub fn pattern(len: usize) -> Vec<u8> {
    (0..len).map(|i| (i % 251) as u8).collect()
}
