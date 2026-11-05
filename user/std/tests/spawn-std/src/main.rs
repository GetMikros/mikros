//! A program that starts processes with `std::process::Command`, for
//! the process manager tests. Run with the single argument `world`, it
//! runs the checks below, starting the C hello world and itself as
//! children, prints a greeting and exits with 0, or with the number of
//! the first check that failed. Its children run it with one of these:
//!
//! - `exit <n>`: exits with `n`;
//! - `echo`: copies its standard input to its standard output;
//! - `env <name>`: prints the variable `name`, or `unset`;
//! - `sleep`: sleeps until it is killed.

#![feature(process_setsid)]

use std::io::{ErrorKind, Read, Write};
use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::process::{Command, ExitCode, Stdio};
use std::time::Duration;

/// This program, as the boot image holds it.
const ME: &str = "/usr/bin/spawn-std";
/// The C hello world, which prints a greeting for its one argument.
const HELLO: &str = "/usr/bin/hello";
const SIGKILL: i32 = 9;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    match args.as_slice() {
        ["world"] => match checks() {
            Ok(()) => {
                println!("spawned, world");
                ExitCode::SUCCESS
            }
            Err(step) => ExitCode::from(step),
        },
        ["exit", n] => ExitCode::from(n.parse::<u8>().unwrap_or(255)),
        ["echo"] => {
            let mut input = Vec::new();
            let copied = std::io::stdin().read_to_end(&mut input).is_ok()
                && std::io::stdout().write_all(&input).is_ok();
            if copied {
                ExitCode::SUCCESS
            } else {
                ExitCode::FAILURE
            }
        }
        ["env", name] => {
            println!("{}", std::env::var(name).as_deref().unwrap_or("unset"));
            ExitCode::SUCCESS
        }
        ["sleep"] => loop {
            std::thread::sleep(Duration::from_secs(60));
        },
        _ => ExitCode::from(200),
    }
}

/// Runs the checks, and returns the number of the first that failed.
fn checks() -> Result<(), u8> {
    let mut step = 0;
    let mut check = |ok: bool| {
        step += 1;
        if ok { Ok(()) } else { Err(step) }
    };
    // A C program's output and status.
    let hello = Command::new(HELLO).arg("world").output();
    check(hello.as_ref().is_ok_and(|o| o.status.success()))?;
    check(hello.is_ok_and(|o| o.stdout == b"hello, world\n"))?;
    // An exit code.
    let status = Command::new(ME).args(["exit", "7"]).status();
    check(status.is_ok_and(|s| s.code() == Some(7)))?;
    // Standard input through a pipe, and standard output read back.
    let echoed = echo(b"through a pipe");
    check(echoed.as_deref() == Some(&b"through a pipe"[..]))?;
    // The environment the child gets.
    let env = Command::new(ME)
        .args(["env", "SPAWN_STD"])
        .env("SPAWN_STD", "given")
        .output();
    check(env.is_ok_and(|o| o.stdout == b"given\n"))?;
    let cleared = Command::new(ME).args(["env", "PATH"]).env_clear().output();
    check(cleared.is_ok_and(|o| o.stdout == b"unset\n"))?;
    // A program found through the child's PATH, which this process does
    // not have.
    let found = Command::new("hello")
        .arg("world")
        .env("PATH", "/nowhere:/usr/bin")
        .output();
    check(found.is_ok_and(|o| o.stdout == b"hello, world\n"))?;
    let missing = Command::new("hello").env("PATH", "/nowhere").status();
    check(missing.is_err_and(|e| e.kind() == ErrorKind::NotFound))?;
    // A missing program.
    let missing = Command::new("/usr/bin/none").status();
    check(missing.is_err_and(|e| e.kind() == ErrorKind::NotFound))?;
    // Killing a child, and waiting for it.
    let killed = Command::new(ME).arg("sleep").spawn().and_then(|mut child| {
        let id = child.id();
        child.kill()?;
        Ok((id, child.wait()?))
    });
    check(killed.is_ok_and(|(id, s)| id != 0 && s.signal() == Some(SIGKILL)))?;
    // Options that need fork are not supported.
    let unsupported = |command: &mut Command| {
        command
            .stdin(Stdio::null())
            .status()
            .is_err_and(|e| e.kind() == ErrorKind::Unsupported)
    };
    let mut pre_exec = Command::new(ME);
    // SAFETY: the closure does nothing.
    unsafe { pre_exec.pre_exec(|| Ok(())) };
    check(unsupported(&mut pre_exec))?;
    check(unsupported(Command::new(ME).uid(0)))?;
    check(unsupported(Command::new(ME).gid(0)))?;
    check(unsupported(Command::new(ME).setsid(true)))?;
    // Neither can a child be given a working directory or process group.
    check(unsupported(Command::new(ME).current_dir("/usr")))?;
    check(unsupported(Command::new(ME).process_group(0)))?;
    Ok(())
}

/// What the `echo` child printed for `input`.
fn echo(input: &[u8]) -> Option<Vec<u8>> {
    let mut child = Command::new(ME)
        .arg("echo")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .ok()?;
    child.stdin.take()?.write_all(input).ok()?;
    let output = child.wait_with_output().ok()?;
    output.status.success().then_some(output.stdout)
}
