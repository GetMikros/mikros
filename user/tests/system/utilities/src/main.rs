//! The command-line utilities as a system test: each check runs one from
//! where the system installs it, as a user's shell would, and looks at
//! its exit status and what it wrote. It exits with 0 when every check
//! passed, and otherwise with the number, from 1, of the first that
//! failed, after writing its name and failure to standard error.

use std::io::Write;
use std::process::{Command, Stdio};

const DEVICES: &str = "/usr/bin/devices";
const CLEAR: &str = "/usr/bin/clear";
const LESS: &str = "/usr/bin/less";
/// A file that every system holds with devices.
const DECLARATIONS: &str = "/usr/share/msh/completions/devices.msh";

/// What failed, and the exit status of the utility, or -1 when it did not
/// exit.
struct Failure {
    what: &'static str,
    status: i32,
}

type Check = fn() -> Result<(), Failure>;

/// What a run of a utility gave.
struct Run {
    status: i32,
    out: String,
    err: String,
}

fn main() {
    let checks: &[(&str, Check)] = &[
        ("devices_lists", devices_lists),
        ("devices_class", devices_class),
        ("devices_json", devices_json),
        ("devices_help", devices_help),
        ("devices_bad_arguments", devices_bad_arguments),
        ("clear_clears", clear_clears),
        ("clear_bad_arguments", clear_bad_arguments),
        ("less_copies", less_copies),
        ("less_bad_arguments", less_bad_arguments),
    ];
    for (number, (name, check)) in checks.iter().enumerate() {
        if let Err(Failure { what, status }) = check() {
            eprintln!("{name} FAILED: {what} ({status})");
            std::process::exit(number as i32 + 1);
        }
    }
}

/// Runs `program` with `arguments` until it exits.
fn run(program: &str, arguments: &[&str]) -> Result<Run, Failure> {
    run_with(program, arguments, None)
}

/// Runs `program` with `arguments` and, when there is one, `input` on
/// its standard input, until it exits.
fn run_with(program: &str, arguments: &[&str], input: Option<&[u8]>) -> Result<Run, Failure> {
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
        .map_err(|_| failed("starting the utility"))?;
    if let (Some(input), Some(mut stdin)) = (input, child.stdin.take()) {
        stdin
            .write_all(input)
            .map_err(|_| failed("writing the input"))?;
    }
    let output = child
        .wait_with_output()
        .map_err(|_| failed("waiting for the utility"))?;
    Ok(Run {
        status: output.status.code().unwrap_or(-1),
        out: String::from_utf8_lossy(&output.stdout).into_owned(),
        err: String::from_utf8_lossy(&output.stderr).into_owned(),
    })
}

/// Passes when `ok` holds, and otherwise fails with `what` and the
/// status of `run`.
fn ensure(run: &Run, what: &'static str, ok: bool) -> Result<(), Failure> {
    match ok {
        true => Ok(()),
        false => Err(Failure {
            what,
            status: run.status,
        }),
    }
}

/// `<class>/<name>` at the start of a line of `devices`.
fn key(line: &str) -> &str {
    line.split_once("  ").map_or(line, |(key, _)| key)
}

/// Every line of `devices` is a `<class>/<name>` of a name that starts
/// with its class, two spaces and a description, and a disk is among
/// them.
fn devices_lists() -> Result<(), Failure> {
    let run = run(DEVICES, &[])?;
    ensure(&run, "succeeding", run.status == 0 && run.err.is_empty())?;
    ensure(&run, "listing some device", !run.out.is_empty())?;
    for line in run.out.lines() {
        let well_formed = line.split_once("  ").is_some_and(|(key, description)| {
            key.split_once('/').is_some_and(|(class, name)| {
                !class.is_empty() && name.starts_with(class) && name.len() > class.len()
            }) && !description.is_empty()
        });
        ensure(&run, "a line of a name and a description", well_formed)?;
    }
    let disk = run.out.lines().any(|l| l.starts_with("storage/"));
    ensure(&run, "listing a disk", disk)
}

/// `devices storage` lists the disks that `devices` lists, and a class
/// that no device has lists nothing.
fn devices_class() -> Result<(), Failure> {
    let all = run(DEVICES, &[])?;
    let disks = run(DEVICES, &["storage"])?;
    ensure(&disks, "succeeding with a class", disks.status == 0)?;
    let expected: Vec<&str> = all
        .out
        .lines()
        .filter(|l| l.starts_with("storage/"))
        .collect();
    let got: Vec<&str> = disks.out.lines().collect();
    ensure(
        &disks,
        "listing the disks",
        !got.is_empty() && got == expected,
    )?;
    let none = run(DEVICES, &["nosuchclass"])?;
    ensure(
        &none,
        "listing no device",
        none.status == 0 && none.out.is_empty(),
    )
}

/// `devices --json` writes an array with an object per device, in the
/// order of the lines.
fn devices_json() -> Result<(), Failure> {
    let lines = run(DEVICES, &[])?;
    let json = run(DEVICES, &["--json"])?;
    ensure(&json, "succeeding with --json", json.status == 0)?;
    let text = json.out.trim_end();
    ensure(
        &json,
        "an array of objects",
        text.starts_with("[{") && text.ends_with("}]"),
    )?;
    let objects: Vec<&str> = text[1..text.len() - 1].split("},{").collect();
    let keys: Vec<&str> = lines.out.lines().map(key).collect();
    ensure(&json, "an object per device", objects.len() == keys.len())?;
    for (object, key) in objects.iter().zip(keys) {
        let (class, name) = key.split_once('/').unwrap_or_default();
        let members = format!("\"class\":\"{class}\",\"name\":\"{name}\",\"description\":\"");
        ensure(&json, "the device's members", object.contains(&members))?;
    }
    Ok(())
}

/// `devices --help` shows its usage and succeeds.
fn devices_help() -> Result<(), Failure> {
    let run = run(DEVICES, &["--help"])?;
    let usage = run.out.starts_with("Usage: devices") && run.out.contains("--json");
    ensure(&run, "showing the usage", run.status == 0 && usage)
}

/// An unknown option and a second class are refused with status 2 and an
/// error after the command's name.
fn devices_bad_arguments() -> Result<(), Failure> {
    for arguments in [&["--nope"][..], &["storage", "input"]] {
        let run = run(DEVICES, arguments)?;
        ensure(&run, "refusing the arguments", run.status == 2)?;
        ensure(&run, "writing nothing", run.out.is_empty())?;
        ensure(&run, "naming the command", run.err.starts_with("devices: "))?;
    }
    Ok(())
}

/// `clear` moves the cursor to the top left and erases the screen and the
/// scrollback, and writes nothing else.
fn clear_clears() -> Result<(), Failure> {
    let run = run(CLEAR, &[])?;
    ensure(&run, "succeeding", run.status == 0 && run.err.is_empty())?;
    ensure(
        &run,
        "writing the sequences",
        run.out == "\x1b[H\x1b[2J\x1b[3J",
    )
}

/// An unknown option and an operand are refused with status 2 and an
/// error after the command's name.
fn clear_bad_arguments() -> Result<(), Failure> {
    for arguments in [&["--nope"][..], &["screen"]] {
        let run = run(CLEAR, arguments)?;
        ensure(&run, "refusing the arguments", run.status == 2)?;
        ensure(&run, "writing nothing", run.out.is_empty())?;
        ensure(&run, "naming the command", run.err.starts_with("clear: "))?;
    }
    Ok(())
}

/// Without a terminal for its output, `less` copies a file, and its
/// standard input, as they are.
fn less_copies() -> Result<(), Failure> {
    let file = run(LESS, &[DECLARATIONS])?;
    let expected = std::fs::read_to_string(DECLARATIONS).unwrap_or_default();
    ensure(&file, "succeeding with a file", file.status == 0)?;
    ensure(
        &file,
        "copying the file",
        !expected.is_empty() && file.out == expected,
    )?;
    let input = "one\ntwo\t\x1b[1mbold\x1b[0m\nno line feed";
    let piped = run_with(LESS, &[], Some(input.as_bytes()))?;
    ensure(&piped, "succeeding with its input", piped.status == 0)?;
    ensure(&piped, "copying its input", piped.out == input)
}

/// A file that does not exist fails with status 1 and an error that
/// names it, and a second file is refused with status 2.
fn less_bad_arguments() -> Result<(), Failure> {
    let missing = run(LESS, &["/no/such/file"])?;
    ensure(&missing, "failing on a missing file", missing.status == 1)?;
    let named = missing.err.starts_with("less: cannot open /no/such/file");
    ensure(
        &missing,
        "naming the missing file",
        named && missing.out.is_empty(),
    )?;
    let two = run(LESS, &[DECLARATIONS, DECLARATIONS])?;
    ensure(&two, "refusing a second file", two.status == 2)?;
    ensure(&two, "naming the command", two.err.starts_with("less: "))
}
