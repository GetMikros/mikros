//! A process of the app `org.example.daemon` that starts before ada
//! allows or denies any folder for it, and so has a pending entry for
//! each (MIKSPEC-0094.R10). It lists them with the mode that a pending
//! entry shows, is refused a folder that ada denied, goes on through the
//! gate once she allows another, and loses what it opened there when she
//! denies it again (MIKSPEC-0094.R11). It is started as `bin/idle
//! pending`, and told what to try next on its input.

use super::{DAEMON, Failed, ensure, failed, grants};
use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};

pub const PICTURES: &str = "user.pictures.access";

/// The process, which `global-check` drives.
pub struct Pending {
    child: Child,
    input: ChildStdin,
    output: BufReader<ChildStdout>,
}

impl Pending {
    /// Starts the process of `program` and waits until it has looked at
    /// its entries.
    pub fn start(program: &str) -> Result<Pending, Failed> {
        let mut child = Command::new(program)
            .arg("pending")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .or(failed(62, "starting the pending process"))?;
        let input = child.stdin.take().ok_or(Failed(62, "no input".into()))?;
        let output = child.stdout.take().ok_or(Failed(62, "no output".into()))?;
        let mut pending = Pending {
            child,
            input,
            output: BufReader::new(output),
        };
        pending.reply("ready", 62)?;
        Ok(pending)
    }

    /// Waits for the line `expected`.
    fn reply(&mut self, expected: &str, code: u8) -> Result<(), Failed> {
        let mut line = String::new();
        self.output
            .read_line(&mut line)
            .or(failed(code, "reading the pending process"))?;
        ensure(
            line.trim() == expected,
            code,
            &format!("the pending process said {line:?}, not {expected:?}"),
        )
    }

    fn tell(&mut self, word: &str, expected: &str, code: u8) -> Result<(), Failed> {
        writeln!(self.input, "{word}")
            .and_then(|()| self.input.flush())
            .or(failed(code, "telling the pending process"))?;
        self.reply(expected, code)
    }

    /// Denies the pictures folder for the app, which needs no
    /// administrator for ada's own grant that nobody decided, and
    /// checks that the process is refused it at once.
    pub fn denied(&mut self) -> Result<(), Failed> {
        let denied = Command::new("permissions")
            .args(["deny", DAEMON, PICTURES])
            .status();
        ensure(
            denied.is_ok_and(|s| s.success()),
            63,
            "denying the pictures folder",
        )?;
        self.tell("pictures", "denied", 64)
    }

    /// Allows the documents folder, which an administrator allows, and
    /// checks that the process goes on through its entry.
    pub fn allowed(&mut self) -> Result<(), Failed> {
        // The runner types ada's password once the process manager has
        // logged the question for the pictures folder, which came first.
        grants::allow()?;
        self.tell("documents", "allowed", 65)
    }

    /// Checks, once the documents folder is denied again, that what the
    /// process opened there is closed, and that it ends well.
    pub fn revoked(mut self) -> Result<(), Failed> {
        self.tell("after", "closed", 66)?;
        match self.child.wait() {
            Ok(status) if status.success() => Ok(()),
            other => failed(66, format!("the pending process ended with {other:?}")),
        }
    }
}
