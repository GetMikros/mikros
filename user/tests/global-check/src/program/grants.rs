//! A folder grant (MIKSPEC-0050.R38): the app `org.example.daemon` gets
//! ada's documents folder once she allows `user.documents.access` for it,
//! and loses it, open files included, when she denies it again, which
//! the revoking SetGrant waits for (MIKSPEC-0037.R34). Both go through
//! the `permissions` command, which may set grants.

use super::{DAEMON, Failed, ensure, failed};
use std::io::{BufRead, BufReader, Write};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

pub const DOCUMENTS: &str = "user.documents.access";

/// Runs `permissions` with `verb` for the app's documents folder, for
/// the caller's user.
fn permissions(verb: &str, code: u8) -> Result<(), Failed> {
    match Command::new("permissions")
        .args([verb, DAEMON, DOCUMENTS])
        .status()
    {
        Ok(status) if status.success() => Ok(()),
        other => failed(code, format!("permissions {verb}: {other:?}")),
    }
}

/// Allows the folder to the app, which an administrator allows.
pub fn allow() -> Result<(), Failed> {
    permissions("allow", 51)
}

/// Starts the app holding a file of the folder open, denies the folder,
/// and checks that SetGrant waited for the revocation, which the app
/// then finds in effect.
pub fn revoke_while_open(program: &str) -> Result<(), Failed> {
    let mut child = Command::new(program)
        .arg("hold")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .or(failed(52, "starting the app"))?;
    let mut out = BufReader::new(child.stdout.take().ok_or(Failed(52, "no output".into()))?);
    let mut line = String::new();
    out.read_line(&mut line).or(failed(52, "reading the app"))?;
    ensure(
        line.trim() == "ready",
        52,
        &format!("the app said {line:?}"),
    )?;
    let start = Instant::now();
    permissions("deny", 53)?;
    ensure(
        start.elapsed() < Duration::from_secs(4),
        53,
        "SetGrant waited for the 5 s limit",
    )?;
    let mut input = child.stdin.take().ok_or(Failed(54, "no input".into()))?;
    input.write_all(b"go\n").or(failed(54, "telling the app"))?;
    drop(input);
    match child.wait() {
        Ok(status) if status.success() => Ok(()),
        Ok(status) => match status.code().and_then(|c| u8::try_from(c).ok()) {
            Some(code) => failed(code, "the app holding the folder"),
            None => failed(54, format!("the app holding the folder: {status:?}")),
        },
        Err(e) => failed(54, format!("the app holding the folder: {e}")),
    }
}
