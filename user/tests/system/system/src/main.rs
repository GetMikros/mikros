//! The system commands as a system test: each check runs one from where
//! the system installs it, and looks at its exit status and what it
//! wrote. It exits with 0 when every check passed, and otherwise with the
//! number, from 1, of the first that failed, after writing its name and
//! failure to standard error.
//!
//! `true` and `false` fail in no other way than by their status, so they
//! have no check of an error.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

const DATE: &str = "/system/bin/date";
const ENV: &str = "/system/bin/env";
const SLEEP: &str = "/system/bin/sleep";
const TRUE: &str = "/system/bin/true";
const FALSE: &str = "/system/bin/false";
const TIMEOUT: &str = "/system/bin/timeout";
const UPTIME: &str = "/system/bin/uptime";
const HOSTNAME: &str = "/system/bin/hostname";
const UNAME: &str = "/system/bin/uname";
const WHOAMI: &str = "/system/bin/whoami";

/// What failed, and the exit status of the command, or -1 when it did not
/// exit.
struct Failure {
    what: &'static str,
    status: i32,
}

type Check = fn() -> Result<(), Failure>;

/// What a run of a command gave.
struct Run {
    status: i32,
    out: String,
    err: String,
}

fn main() {
    let checks: &[(&str, Check)] = &[
        ("date_writes_times", date_writes_times),
        ("date_writes_now", date_writes_now),
        ("date_reads_a_file", date_reads_a_file),
        ("date_refuses_bad_dates", date_refuses_bad_dates),
        ("env_writes_the_environment", env_writes_the_environment),
        ("env_runs_a_command", env_runs_a_command),
        ("env_names_what_is_missing", env_names_what_is_missing),
        ("sleep_waits", sleep_waits),
        ("sleep_refuses_bad_durations", sleep_refuses_bad_durations),
        ("true_and_false_exit", true_and_false_exit),
        ("timeout_lets_a_command_end", timeout_lets_a_command_end),
        ("timeout_ends_a_command", timeout_ends_a_command),
        (
            "timeout_refuses_bad_arguments",
            timeout_refuses_bad_arguments,
        ),
        ("uptime_writes_the_time_up", uptime_writes_the_time_up),
        ("uptime_bad_arguments", uptime_bad_arguments),
        ("hostname_writes_the_name", hostname_writes_the_name),
        ("hostname_takes_no_operand", hostname_takes_no_operand),
        ("uname_writes_the_fields", uname_writes_the_fields),
        ("uname_bad_arguments", uname_bad_arguments),
        ("whoami_names_the_user", whoami_names_the_user),
        ("whoami_bad_arguments", whoami_bad_arguments),
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
    let failed = |what| Failure { what, status: -1 };
    let output = Command::new(program)
        .args(arguments)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .map_err(|_| failed("running the command"))?;
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

/// Each run of `program` with one of `refused` exits with 2, writes
/// nothing and names the command in its error.
fn refuses(program: &str, name: &str, refused: &[&[&str]]) -> Result<(), Failure> {
    for arguments in refused {
        let run = run(program, arguments)?;
        ensure(&run, "refusing the arguments", run.status == 2)?;
        ensure(&run, "writing nothing", run.out.is_empty())?;
        ensure(
            &run,
            "naming the command",
            run.err.starts_with(&format!("{name}: ")),
        )?;
    }
    Ok(())
}

/// Whether `text` has the form of `pattern`, in which `9` stands for a
/// digit and every other character for itself.
fn shaped(text: &str, pattern: &str) -> bool {
    text.len() == pattern.len()
        && text.bytes().zip(pattern.bytes()).all(|(t, p)| match p {
            b'9' => t.is_ascii_digit(),
            p => t == p,
        })
}

/// A new folder for the checks, with the file `f`.
fn folder() -> Result<PathBuf, Failure> {
    let failed = |what| Failure { what, status: -1 };
    let base = std::env::var("TMPDIR").unwrap_or_else(|_| "/tmp".into());
    let dir = Path::new(&base).join(format!("system-{}", std::process::id()));
    std::fs::create_dir_all(&dir).map_err(|_| failed("making the folder"))?;
    std::fs::write(dir.join("f"), b"x").map_err(|_| failed("writing a file"))?;
    Ok(dir)
}

/// Times given as seconds since 1970, in UTC, in a format, the forms of
/// the options and JSON.
fn date_writes_times() -> Result<(), Failure> {
    let format = run(DATE, &["-u", "-d", "@0", "+%F_%T %a %j %s %Z"])?;
    ensure(
        &format,
        "succeeding",
        format.status == 0 && format.err.is_empty(),
    )?;
    ensure(
        &format,
        "the format",
        format.out == "1970-01-01_00:00:00 Thu 001 0 UTC\n",
    )?;
    let at = "@1794733687";
    let forms: [(&[&str], &str); 5] = [
        (&["-u", "-d", at], "2026-11-15 09:08:07 UTC\n"),
        (&["-u", "-d", at, "-I"], "2026-11-15\n"),
        (
            &["-u", "-d", at, "-Iseconds"],
            "2026-11-15T09:08:07+00:00\n",
        ),
        (
            &["-u", "-d", at, "--rfc-3339=seconds"],
            "2026-11-15 09:08:07+00:00\n",
        ),
        (&["-u", "-d", at, "-R"], "Sun, 15 Nov 2026 09:08:07 +0000\n"),
    ];
    for (arguments, expected) in forms {
        let run = run(DATE, arguments)?;
        ensure(&run, "each form", run.status == 0 && run.out == expected)?;
    }
    let given = run(DATE, &["-u", "-d", "2026-11-15 10:08:07+01:00", "+%s"])?;
    ensure(&given, "a date with an offset", given.out == "1794733687\n")?;
    let json = run(DATE, &["-u", "-d", at, "--json"])?;
    ensure(
        &json,
        "the object",
        json.status == 0
            && json.out
                == "{\"utc\":\"2026-11-15T09:08:07Z\",\"local\":\"2026-11-15T09:08:07+00:00\",\"zone\":\"UTC\"}\n",
    )
}

/// Without options, the time now in local time with its zone.
fn date_writes_now() -> Result<(), Failure> {
    let run = run(DATE, &[])?;
    ensure(&run, "succeeding", run.status == 0 && run.err.is_empty())?;
    let line = run.out.strip_suffix('\n').unwrap_or_default();
    let (time, zone) = line.split_at(line.len().min(19));
    ensure(
        &run,
        "the time and the zone",
        shaped(time, "9999-99-99 99:99:99") && zone.len() > 1 && zone.starts_with(' '),
    )
}

/// `-r` writes a file's modified time.
fn date_reads_a_file() -> Result<(), Failure> {
    let dir = folder()?;
    let file = dir.join("f");
    let modified = std::fs::metadata(&file)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|m| m.duration_since(std::time::UNIX_EPOCH).ok());
    let run = run(DATE, &["-r", file.to_str().unwrap_or_default(), "+%s"])?;
    let _ = std::fs::remove_dir_all(&dir);
    ensure(&run, "succeeding with -r", run.status == 0)?;
    let expected = modified.map(|m| format!("{}\n", m.as_secs()));
    ensure(
        &run,
        "the modified time",
        Some(&run.out) == expected.as_ref(),
    )
}

/// Dates that are none, a format without `+` and two forms are refused
/// with status 2, before any time is set, and a missing file fails with
/// 1 and is named.
fn date_refuses_bad_dates() -> Result<(), Failure> {
    let refused: [&[&str]; 5] = [
        &["-d", "tomorrow"],
        &["-s", "2026-02-30"],
        &["%s"],
        &["-R", "-I"],
        &["-Ihours"],
    ];
    refuses(DATE, "date", &refused)?;
    let missing = run(DATE, &["-r", "/no/such/file"])?;
    ensure(&missing, "failing on a missing file", missing.status == 1)?;
    ensure(
        &missing,
        "naming the file",
        missing.err.starts_with("date: /no/such/file: "),
    )
}

/// The environment, emptied and set, one variable per line or ended by
/// NUL.
fn env_writes_the_environment() -> Result<(), Failure> {
    let lines = run(ENV, &["-i", "A=1", "B=x=y"])?;
    ensure(
        &lines,
        "succeeding",
        lines.status == 0 && lines.err.is_empty(),
    )?;
    ensure(&lines, "the variables", lines.out == "A=1\nB=x=y\n")?;
    let null = run(ENV, &["-i", "-0", "A=1", "B=2"])?;
    ensure(
        &null,
        "the variables ended by NUL",
        null.out == "A=1\0B=2\0",
    )?;
    let inherited = run(ENV, &[])?;
    ensure(
        &inherited,
        "the inherited environment",
        inherited.status == 0 && inherited.out.lines().all(|l| l.contains('=')),
    )
}

/// A command runs with the environment changed, and env exits with its
/// status. The process manager adds `TMPDIR` to every environment, also
/// to one that -i emptied.
fn env_runs_a_command() -> Result<(), Failure> {
    let run = run(ENV, &["-i", "X=1", ENV, "-u", "X", "Y=2"])?;
    let lines: Vec<&str> = run
        .out
        .lines()
        .filter(|l| !l.starts_with("TMPDIR="))
        .collect();
    ensure(
        &run,
        "running env in env",
        run.status == 0 && lines == ["Y=2"],
    )?;
    let failing = self::run(ENV, &[FALSE])?;
    ensure(&failing, "the command's status", failing.status == 1)?;
    let dir = folder()?;
    let modified = std::fs::metadata(dir.join("f"))
        .and_then(|m| m.modified())
        .ok()
        .and_then(|m| m.duration_since(std::time::UNIX_EPOCH).ok());
    let path = dir.to_str().unwrap_or_default();
    // date finds `f` by its relative path only in the folder.
    let moved = self::run(ENV, &["-C", path, DATE, "-r", "f", "+%s"])?;
    let _ = std::fs::remove_dir_all(&dir);
    ensure(&moved, "running in the folder", moved.status == 0)?;
    let expected = modified.map(|m| format!("{}\n", m.as_secs()));
    ensure(
        &moved,
        "the file in the folder",
        Some(&moved.out) == expected.as_ref(),
    )
}

/// A command that is not there fails with 127 and is named; a setting
/// without a name and `-0` with a command are refused with 2.
fn env_names_what_is_missing() -> Result<(), Failure> {
    let missing = run(ENV, &["/no/such/program"])?;
    ensure(&missing, "failing with 127", missing.status == 127)?;
    ensure(
        &missing,
        "naming the program",
        missing.err.starts_with("env: /no/such/program: "),
    )?;
    refuses(ENV, "env", &[&["=1"], &["-0", TRUE], &["--nope"]])
}

/// sleep waits for the sum of its durations.
fn sleep_waits() -> Result<(), Failure> {
    let started = Instant::now();
    let run = run(SLEEP, &["0.1", ".1s"])?;
    let waited = started.elapsed();
    ensure(&run, "succeeding", run.status == 0 && run.out.is_empty())?;
    ensure(&run, "waiting", waited >= Duration::from_millis(200))
}

fn sleep_refuses_bad_durations() -> Result<(), Failure> {
    refuses(SLEEP, "sleep", &[&["1x"], &["-1"], &[]])
}

/// `true` and `false` exit with 0 and 1, whatever their arguments, and
/// `--help` alone shows the usage.
fn true_and_false_exit() -> Result<(), Failure> {
    for (program, status) in [(TRUE, 0), (FALSE, 1)] {
        for arguments in [&[][..], &["-x", "--nope", "word"]] {
            let run = run(program, arguments)?;
            ensure(&run, "the status", run.status == status)?;
            ensure(
                &run,
                "writing nothing",
                run.out.is_empty() && run.err.is_empty(),
            )?;
        }
    }
    let help = run(TRUE, &["--help"])?;
    ensure(
        &help,
        "showing the usage",
        help.status == 0 && help.out.starts_with("Usage: true"),
    )?;
    // false keeps its status after its help.
    let help = run(FALSE, &["--help"])?;
    ensure(
        &help,
        "showing false's usage",
        help.status == 1 && help.out.starts_with("Usage: false"),
    )
}

/// A command that ends in time gives its own status.
fn timeout_lets_a_command_end() -> Result<(), Failure> {
    let ended = run(TIMEOUT, &["5", TRUE])?;
    ensure(
        &ended,
        "succeeding",
        ended.status == 0 && ended.err.is_empty(),
    )?;
    let failed = run(TIMEOUT, &["5s", FALSE])?;
    ensure(&failed, "the command's status", failed.status == 1)?;
    let forever = run(TIMEOUT, &["0", SLEEP, "0.1"])?;
    ensure(&forever, "no limit with 0", forever.status == 0)
}

/// A command that runs too long is signalled, and timeout exits with 124,
/// or with the command's status with `--preserve-status`.
fn timeout_ends_a_command() -> Result<(), Failure> {
    let started = Instant::now();
    let run = run(TIMEOUT, &["0.2", SLEEP, "30"])?;
    ensure(&run, "exiting with 124", run.status == 124)?;
    ensure(
        &run,
        "ending it early",
        started.elapsed() < Duration::from_secs(10),
    )?;
    let killed = self::run(TIMEOUT, &["-s", "KILL", "0.2", SLEEP, "30"])?;
    ensure(&killed, "exiting with 124 after KILL", killed.status == 124)?;
    let preserved = self::run(TIMEOUT, &["--preserve-status", "0.2", SLEEP, "30"])?;
    ensure(
        &preserved,
        "the status of the ended command",
        preserved.status == 128 + 15,
    )
}

/// Bad durations and signals are refused with 2, and a command that is
/// not there fails with 127 and is named.
fn timeout_refuses_bad_arguments() -> Result<(), Failure> {
    let refused: [&[&str]; 4] = [
        &["soon", TRUE],
        &["-s", "NOPE", "1", TRUE],
        &["-k", "x", "1", TRUE],
        &["1"],
    ];
    refuses(TIMEOUT, "timeout", &refused)?;
    let missing = run(TIMEOUT, &["1", "/no/such/program"])?;
    ensure(&missing, "failing with 127", missing.status == 127)?;
    ensure(
        &missing,
        "naming the program",
        missing.err.starts_with("timeout: /no/such/program: "),
    )
}

/// The time and how long the system has run, in words, its start and
/// JSON, with no load averages.
fn uptime_writes_the_time_up() -> Result<(), Failure> {
    let line = run(UPTIME, &[])?;
    ensure(&line, "succeeding", line.status == 0 && line.err.is_empty())?;
    ensure(
        &line,
        "the time and how long it runs",
        line.out.len() > 12
            && shaped(&line.out[..12], "99:99:99 up ")
            && !line.out.contains("load"),
    )?;
    let pretty = run(UPTIME, &["-p"])?;
    ensure(
        &pretty,
        "the words",
        pretty.status == 0 && pretty.out.starts_with("up "),
    )?;
    let since = run(UPTIME, &["-s"])?;
    ensure(
        &since,
        "the start",
        since.status == 0 && shaped(since.out.trim_end(), "9999-99-99 99:99:99"),
    )?;
    let json = run(UPTIME, &["--json"])?;
    let up = json
        .out
        .split("\"up\":")
        .nth(1)
        .and_then(|rest| rest.split(',').next())
        .and_then(|n| n.parse::<u64>().ok());
    ensure(
        &json,
        "the object",
        json.status == 0
            && json.out.starts_with("{\"time\":\"")
            && json.out.contains(",\"since\":\""),
    )?;
    ensure(&json, "the seconds up", up.is_some_and(|up| up > 0))
}

fn uptime_bad_arguments() -> Result<(), Failure> {
    refuses(UPTIME, "uptime", &[&["-p", "-s"], &["now"], &["--nope"]])
}

/// One valid host name, which `-s` and `uname -n` give as well.
fn hostname_writes_the_name() -> Result<(), Failure> {
    let run = run(HOSTNAME, &[])?;
    ensure(&run, "succeeding", run.status == 0 && run.err.is_empty())?;
    let name = run.out.strip_suffix('\n').unwrap_or_default();
    let valid = (1..=63).contains(&name.len())
        && name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
        && !name.starts_with('-')
        && !name.ends_with('-');
    ensure(&run, "a valid host name", valid)?;
    let short = self::run(HOSTNAME, &["-s"])?;
    ensure(&short, "the same short name", short.out == run.out)?;
    let node = self::run(UNAME, &["-n"])?;
    ensure(&node, "the node name of uname", node.out == run.out)
}

/// The host name is set in its file, not by an operand.
fn hostname_takes_no_operand() -> Result<(), Failure> {
    refuses(HOSTNAME, "hostname", &[&["newname"], &["--nope"]])
}

/// Mikros, the fields in their order, and JSON.
fn uname_writes_the_fields() -> Result<(), Failure> {
    let plain = run(UNAME, &[])?;
    ensure(
        &plain,
        "succeeding",
        plain.status == 0 && plain.err.is_empty(),
    )?;
    ensure(&plain, "the system's name", plain.out == "Mikros\n")?;
    let some = run(UNAME, &["-o", "-pms"])?;
    ensure(
        &some,
        "the fields in order",
        some.out == "Mikros x86_64 x86_64 Mikros\n",
    )?;
    let all = run(UNAME, &["-a"])?;
    let fields: Vec<&str> = all.out.split_whitespace().collect();
    ensure(&all, "succeeding with -a", all.status == 0)?;
    ensure(
        &all,
        "every field",
        fields.len() == 7
            && fields[0] == "Mikros"
            && fields[2] == fields[3]
            && fields[4..] == ["x86_64", "x86_64", "Mikros"],
    )?;
    let release = run(UNAME, &["-r"])?;
    ensure(
        &release,
        "the version of -a",
        release.out.trim_end() == fields[2],
    )?;
    let json = run(UNAME, &["--json"])?;
    ensure(
        &json,
        "the object",
        json.status == 0
            && json.out.starts_with("{\"system\":\"Mikros\",\"node\":\"")
            && json
                .out
                .ends_with(",\"machine\":\"x86_64\",\"os\":\"Mikros\"}\n"),
    )
}

fn uname_bad_arguments() -> Result<(), Failure> {
    refuses(UNAME, "uname", &[&["-x"], &["extra"]])
}

/// The name of this test's user, `system` for user ID 0, and the same
/// in JSON with the ID.
fn whoami_names_the_user() -> Result<(), Failure> {
    let plain = run(WHOAMI, &[])?;
    ensure(
        &plain,
        "succeeding",
        plain.status == 0 && plain.err.is_empty(),
    )?;
    let name = plain.out.strip_suffix('\n').unwrap_or_default();
    ensure(
        &plain,
        "a name",
        !name.is_empty() && !name.contains(char::is_whitespace),
    )?;
    let json = run(WHOAMI, &["--json"])?;
    let prefix = format!("{{\"name\":\"{name}\",\"id\":");
    let id = json
        .out
        .strip_prefix(&prefix)
        .and_then(|rest| rest.strip_suffix("}\n"))
        .and_then(|id| id.parse::<u32>().ok());
    ensure(
        &json,
        "the name and an ID",
        json.status == 0 && id.is_some(),
    )?;
    ensure(
        &json,
        "system for user ID 0",
        id != Some(0) || name == "system",
    )
}

fn whoami_bad_arguments() -> Result<(), Failure> {
    refuses(WHOAMI, "whoami", &[&["someone"], &["--nope"]])
}
