//! A program that the session shell of `tests/console.img` puts into a
//! package of the app `org.example.hello`, installs for its user, and
//! starts from the package without naming an app ID. It checks that it
//! runs as that app, installed locally: in the app's folder, which is
//! also its home, with the app's cache, the variables that say how the
//! app is installed, and a folder it can write to. It then starts
//! itself in `/tmp` with the argument `child`, and the child checks that
//! it has the same variables and the folder it was given. Either exits
//! with 0 when every check passed, and with the number of the first
//! check that failed otherwise.
//!
//! Started as the package's command `hello-probe` with the arguments
//! `command <folder>`, it checks that it runs as the app, with the app's
//! home, in `<folder>`, the shell's, which it can write to.

use std::env;
use std::fs;
use std::path::Path;
use std::process::{Command, ExitCode};

/// The app's folder, and the program that its manifest names.
const FOLDER: &str = "/apps/org.example.hello";
const EXECUTABLE: &str = "/apps/org.example.hello/bin/hello";
/// This program, as the app sees it.
const ME: &str = "/apps/org.example.hello/bin/probe";
/// The file it writes, which the shell finds in the app's `data`.
const NOTE: &str = "/apps/org.example.hello/note";

fn main() -> ExitCode {
    let args: Vec<String> = env::args().collect();
    let result = match args.get(1).map(String::as_str) {
        Some("child") => child_checks(),
        Some("command") => command_checks(args.get(2).map(String::as_str).unwrap_or("")),
        _ => checks(),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(code) => ExitCode::from(code),
    }
}

fn check(ok: bool, code: u8) -> Result<(), u8> {
    if ok { Ok(()) } else { Err(code) }
}

fn var_is(name: &str, value: &str) -> bool {
    env::var(name).is_ok_and(|v| v == value)
}

/// The variables that tell the app and its children how it is installed.
fn installed() -> bool {
    var_is("MIKA_MODE", "local") && var_is("MIKA_EXECUTABLE", EXECUTABLE)
}

fn checks() -> Result<(), u8> {
    check(installed(), 41)?;
    check(var_is("HOME", FOLDER), 42)?;
    check(var_is("XDG_CACHE_HOME", &format!("{FOLDER}/cache")), 43)?;
    let cwd = env::current_dir().map_err(|_| 44)?;
    check(cwd == Path::new(FOLDER), 44)?;
    check(fs::write(NOTE, b"from the app").is_ok(), 45)?;
    check(
        Path::new(&format!("{FOLDER}/package/manifest.yaml")).is_file(),
        46,
    )?;
    let status = Command::new(ME)
        .arg("child")
        .current_dir("/tmp")
        .status()
        .map_err(|_| 47)?;
    match status.code() {
        Some(0) => Ok(()),
        Some(code) => Err(u8::try_from(code).unwrap_or(48)),
        None => Err(48),
    }
}

fn child_checks() -> Result<(), u8> {
    check(installed(), 51)?;
    let cwd = env::current_dir().map_err(|_| 52)?;
    check(cwd == Path::new("/tmp"), 52)?;
    check(fs::read(NOTE).is_ok_and(|n| n == b"from the app"), 53)?;
    Ok(())
}

fn command_checks(folder: &str) -> Result<(), u8> {
    check(var_is("MIKA_MODE", "local"), 81)?;
    check(var_is("HOME", FOLDER), 82)?;
    let cwd = env::current_dir().map_err(|_| 83)?;
    check(!folder.is_empty() && cwd == Path::new(folder), 83)?;
    check(fs::write("from-command", b"from the command").is_ok(), 84)?;
    Ok(())
}
