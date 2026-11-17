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
//!
//! Started from a portable package with `portable <folder> <package>
//! <id>`, or from a build with `build <folder> <program> <id>`, it checks
//! that it runs portable, with `<folder>` as its home and working folder,
//! which it can write to, and `<package>` or `<program>` as
//! `MIKA_EXECUTABLE`, with the package's files at `/apps/<id>` but no
//! cache there, or, for a build, nothing there.

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
        Some(mode @ ("portable" | "build")) => {
            let arg = |at: usize| args.get(at).map(String::as_str).unwrap_or("");
            portable_checks(mode == "build", arg(2), arg(3), arg(4))
        }
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

fn portable_checks(build: bool, folder: &str, executable: &str, id: &str) -> Result<(), u8> {
    let code = if build { 100 } else { 90 };
    check(var_is("MIKA_MODE", "portable"), code + 1)?;
    check(!folder.is_empty() && var_is("HOME", folder), code + 2)?;
    let cwd = env::current_dir().map_err(|_| code + 3)?;
    check(cwd == Path::new(folder), code + 3)?;
    check(var_is("MIKA_EXECUTABLE", executable), code + 4)?;
    check(
        fs::write("note", b"from the portable app").is_ok(),
        code + 5,
    )?;
    let files = format!("/apps/{id}");
    match build {
        false => {
            check(
                Path::new(&format!("{files}/manifest.yaml")).is_file(),
                code + 6,
            )?;
            check(!Path::new(&format!("{files}/cache")).exists(), code + 7)?;
        }
        true => check(!Path::new(&files).exists(), code + 6)?,
    }
    Ok(())
}
