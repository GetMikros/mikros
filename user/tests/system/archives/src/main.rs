//! tar, zip and unzip as a system test. Each check runs them from where
//! the system installs them, in a folder of its own under `$TMPDIR`, or
//! `/tmp`: archives made, listed and extracted again, an error of each,
//! and archives made by the checks themselves whose entries lead out of
//! the folder they are extracted into. It exits with 0 when every check
//! passed, and otherwise with the number, from 1, of the first that
//! failed, after writing its name and failure to standard error.

mod tar;
mod zip;

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

pub const TAR: &str = "/system/bin/tar";
pub const ZIP: &str = "/system/bin/zip";
pub const UNZIP: &str = "/system/bin/unzip";

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
}

fn main() {
    let mut checks: Vec<(&str, Check)> = Vec::new();
    checks.extend_from_slice(tar::CHECKS);
    checks.extend_from_slice(zip::CHECKS);
    for (number, (name, check)) in checks.iter().enumerate() {
        if let Err(Failure { what, status }) = check() {
            eprintln!("{name} FAILED: {what} ({status})");
            std::process::exit(number as i32 + 1);
        }
    }
}

/// Runs `program` with `arguments` in the folder `dir` and, when there
/// is one, `input` on its standard input, until it exits.
pub fn run_in(
    dir: &Scratch,
    program: &str,
    arguments: &[&str],
    input: Option<&[u8]>,
) -> Result<Run, Failure> {
    let failed = |what| Failure { what, status: -1 };
    let mut child = Command::new(program)
        .args(arguments)
        .current_dir(&dir.0)
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
    })
}

/// Passes when `ok` holds, and otherwise fails with `what` and the
/// status of `run`.
pub fn ensure(run: &Run, what: &'static str, ok: bool) -> Result<(), Failure> {
    match ok {
        true => Ok(()),
        false => {
            // What the command wrote, to tell why in the serial log.
            eprintln!("out: {:?}\nerr: {:?}", run.out, run.err);
            Err(Failure {
                what,
                status: run.status,
            })
        }
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
    let dir = Path::new(&base).join(format!("archives-{name}-{}", std::process::id()));
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
    pub fn path(&self, name: &str) -> PathBuf {
        self.0.join(name)
    }

    /// Writes `contents` to the file `name` in the folder, making the
    /// folders that lead to it.
    pub fn write(&self, name: &str, contents: &[u8]) -> Result<(), Failure> {
        let path = self.path(name);
        let made = path
            .parent()
            .map_or(Ok(()), std::fs::create_dir_all)
            .and_then(|()| std::fs::write(&path, contents));
        made.map_err(|_| Failure {
            what: "writing a file",
            status: -1,
        })
    }

    /// The contents of the file `name` in the folder, if it can be read.
    pub fn read(&self, name: &str) -> Option<Vec<u8>> {
        std::fs::read(self.path(name)).ok()
    }

    /// The tree that the round trips store: a file, a file with a mode,
    /// a folder with a file, and a link.
    pub fn tree(&self) -> Result<(), Failure> {
        use std::os::unix::fs::PermissionsExt;
        self.write("src/a.txt", b"alpha\n")?;
        self.write("src/run", b"#!/system/bin/msh\n")?;
        self.write("src/sub/b.txt", &[b'b'; 5000])?;
        let made =
            std::fs::set_permissions(self.path("src/run"), std::fs::Permissions::from_mode(0o751))
                .and_then(|()| std::os::unix::fs::symlink("a.txt", self.path("src/link")));
        holds("making the tree", made.is_ok())
    }

    /// Whether `folder` holds what `tree` made, with the mode and the
    /// link.
    pub fn same_tree(&self, folder: &str) -> Result<(), Failure> {
        use std::os::unix::fs::PermissionsExt;
        let at = |name: &str| format!("{folder}/{name}");
        holds(
            "a.txt",
            self.read(&at("a.txt")).as_deref() == Some(b"alpha\n"),
        )?;
        holds(
            "sub/b.txt",
            self.read(&at("sub/b.txt")) == Some(vec![b'b'; 5000]),
        )?;
        let mode = std::fs::metadata(self.path(&at("run"))).map(|m| m.permissions().mode());
        holds("the mode of run", mode.is_ok_and(|m| m & 0o7777 == 0o751))?;
        let link = std::fs::read_link(self.path(&at("link")));
        holds("the link", link.is_ok_and(|l| l == Path::new("a.txt")))
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
