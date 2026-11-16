//! The command-line utilities as a system test: each check runs one from
//! where the system installs it, as a user's shell would, and looks at
//! its exit status and what it wrote. It exits with 0 when every check
//! passed, and otherwise with the number, from 1, of the first that
//! failed, after writing its name and failure to standard error.

use std::io::Write;
use std::process::{Command, Stdio};

const DEVICES: &str = "/system/bin/devices";
const CLEAR: &str = "/system/bin/clear";
const LESS: &str = "/system/bin/less";
const PS: &str = "/system/bin/ps";
const POWER: &str = "/system/bin/power";
const LOGS: &str = "/system/bin/logs";
const SERVICES: &str = "/system/bin/services";
const PERMISSIONS: &str = "/system/bin/permissions";
const ACCOUNTS: &str = "/system/bin/accounts";
/// An app ID that no program has, whose grants the checks set.
const NO_APP: &str = "system.no-such-app";
/// A file that every system holds with devices.
const DECLARATIONS: &str = "/system/share/msh/completions/devices.msh";

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
        ("ps_lists", ps_lists),
        ("ps_json", ps_json),
        ("ps_bad_arguments", ps_bad_arguments),
        ("power_not_authorized", power_not_authorized),
        ("power_bad_arguments", power_bad_arguments),
        ("logs_not_authorized", logs_not_authorized),
        ("logs_bad_arguments", logs_bad_arguments),
        ("services_lists", services_lists),
        ("services_json", services_json),
        ("services_not_authorized", services_not_authorized),
        ("services_bad_arguments", services_bad_arguments),
        ("permissions_deny_and_list", permissions_deny_and_list),
        (
            "permissions_refuses_bad_grants",
            permissions_refuses_bad_grants,
        ),
        ("permissions_not_authorized", permissions_not_authorized),
        ("permissions_bad_arguments", permissions_bad_arguments),
        ("accounts_lists", accounts_lists),
        ("accounts_needs_a_terminal", accounts_needs_a_terminal),
        ("accounts_unknown_user", accounts_unknown_user),
        ("accounts_bad_arguments", accounts_bad_arguments),
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

/// `ps` writes a header and a line for each process, among them this
/// test and init, which only a holder of `system.processes.read` sees.
fn ps_lists() -> Result<(), Failure> {
    let run = run(PS, &[])?;
    ensure(&run, "succeeding", run.status == 0 && run.err.is_empty())?;
    let mut lines = run.out.lines();
    let header = lines
        .next()
        .unwrap_or_default()
        .split_whitespace()
        .collect::<Vec<_>>();
    ensure(
        &run,
        "the header",
        header == ["PID", "PARENT", "USER", "APP", "ID", "PATH"],
    )?;
    let pid_of = |line: &str| {
        line.split_whitespace()
            .next()
            .and_then(|p| p.parse::<u32>().ok())
    };
    let pids: Vec<u32> = lines.filter_map(pid_of).collect();
    ensure(
        &run,
        "sorted process IDs",
        pids.windows(2).all(|w| w[0] < w[1]),
    )?;
    ensure(&run, "listing init", pids.contains(&1))?;
    ensure(
        &run,
        "listing this test",
        pids.contains(&std::process::id()),
    )
}

/// `ps --json` writes an object per process, and ps itself runs with
/// the app ID of its command manifest.
fn ps_json() -> Result<(), Failure> {
    let run = run(PS, &["--json"])?;
    ensure(&run, "succeeding with --json", run.status == 0)?;
    let text = run.out.trim_end();
    ensure(
        &run,
        "an array of objects",
        text.starts_with("[{") && text.ends_with("}]"),
    )?;
    let own = format!("{{\"pid\":{},", std::process::id());
    ensure(&run, "an object for this test", text.contains(&own))?;
    let ps = "\"app_id\":\"system.ps\",\"path\":\"/system/bin/ps\"";
    ensure(&run, "ps running as system.ps", text.contains(ps))?;
    ensure(&run, "no kernel object IDs", !text.contains("koid"))
}

/// An unknown option and an operand are refused with status 2.
fn ps_bad_arguments() -> Result<(), Failure> {
    for arguments in [&["--nope"][..], &["all"]] {
        let run = run(PS, arguments)?;
        ensure(&run, "refusing the arguments", run.status == 2)?;
        ensure(&run, "naming the command", run.err.starts_with("ps: "))?;
    }
    Ok(())
}

/// Without a text console to ask an administrator on, `power off` is not
/// authorized, and the machine stays up.
fn power_not_authorized() -> Result<(), Failure> {
    let run = run(POWER, &["off"])?;
    ensure(&run, "failing", run.status == 1 && run.out.is_empty())?;
    ensure(&run, "saying why", run.err == "power: not authorized\n")
}

/// No action, another action and a second operand are refused with
/// status 2.
fn power_bad_arguments() -> Result<(), Failure> {
    for arguments in [&[][..], &["sideways"], &["off", "now"]] {
        let run = run(POWER, arguments)?;
        ensure(&run, "refusing the arguments", run.status == 2)?;
        ensure(&run, "naming the command", run.err.starts_with("power: "))?;
    }
    Ok(())
}

/// No administrator answers here, so logs prints no record.
fn logs_not_authorized() -> Result<(), Failure> {
    let run = run(LOGS, &["-n", "3"])?;
    ensure(&run, "failing", run.status == 1 && run.out.is_empty())?;
    ensure(&run, "saying why", run.err == "logs: not authorized\n")
}

/// Following with JSON, values that are not numbers or severities, a
/// missing value and an operand are refused with status 2, before any
/// authorization is asked.
fn logs_bad_arguments() -> Result<(), Failure> {
    let refused: [&[&str]; 6] = [
        &["-f", "--json"],
        &["-n", "few"],
        &["--pid", "-1"],
        &["--severity", "fatal"],
        &["--app"],
        &["system.logs"],
    ];
    for arguments in refused {
        let run = run(LOGS, arguments)?;
        ensure(&run, "refusing the arguments", run.status == 2)?;
        ensure(&run, "writing nothing", run.out.is_empty())?;
        ensure(&run, "naming the command", run.err.starts_with("logs: "))?;
    }
    Ok(())
}

/// The header, then a line per service: the log service runs with a
/// process ID, and a service that does not run shows `-`.
fn services_lists() -> Result<(), Failure> {
    let run = run(SERVICES, &[])?;
    ensure(&run, "succeeding", run.status == 0 && run.err.is_empty())?;
    let mut lines = run.out.lines();
    let header = lines.next().unwrap_or("").split_whitespace();
    ensure(
        &run,
        "a header line",
        header.eq(["NAME", "STATE", "PID", "RESTARTS"]),
    )?;
    let rows: Vec<Vec<&str>> = lines.map(|l| l.split_whitespace().collect()).collect();
    ensure(&run, "four columns", rows.iter().all(|r| r.len() == 4))?;
    let log = rows.iter().find(|r| r[0] == "logsvc");
    ensure(
        &run,
        "the log service running",
        log.is_some_and(|r| r[1] == "running" && r[2].parse::<u32>().is_ok_and(|p| p > 0)),
    )?;
    let consistent = rows.iter().all(|r| (r[1] == "running") == (r[2] != "-"));
    ensure(&run, "process IDs of the running services only", consistent)
}

fn services_json() -> Result<(), Failure> {
    let run = run(SERVICES, &["--json"])?;
    ensure(&run, "succeeding with --json", run.status == 0)?;
    let text = run.out.trim_end();
    ensure(
        &run,
        "one array",
        text.starts_with("[{") && text.ends_with("}]"),
    )?;
    ensure(
        &run,
        "the log service's object",
        text.contains("{\"name\":\"logsvc\",\"state\":\"running\",\"on_demand\":false,"),
    )?;
    ensure(
        &run,
        "every member",
        ["\"pid\":", "\"restarts\":", "\"exit_status\":"]
            .iter()
            .all(|k| text.contains(k)),
    )
}

/// No administrator answers here, so nothing is stopped.
fn services_not_authorized() -> Result<(), Failure> {
    let run = run(SERVICES, &["stop", "logsvc"])?;
    ensure(&run, "failing", run.status == 1 && run.out.is_empty())?;
    ensure(&run, "saying why", run.err == "services: not authorized\n")
}

/// An unknown subcommand and a missing or extra operand are refused with
/// status 2, before any authorization is asked.
fn services_bad_arguments() -> Result<(), Failure> {
    let refused: [&[&str]; 3] = [&["list"], &["stop"], &["stop", "logsvc", "termsvc"]];
    for arguments in refused {
        let run = run(SERVICES, arguments)?;
        ensure(&run, "refusing the arguments", run.status == 2)?;
        ensure(&run, "writing nothing", run.out.is_empty())?;
        ensure(
            &run,
            "naming the command",
            run.err.starts_with("services: "),
        )?;
    }
    Ok(())
}

/// Denying for the caller's own user needs no administrator: the grant
/// then shows in the list and its JSON.
fn permissions_deny_and_list() -> Result<(), Failure> {
    let deny = run(PERMISSIONS, &["deny", NO_APP, "user.camera.use"])?;
    ensure(&deny, "denying", deny.status == 0 && deny.err.is_empty())?;
    let list = run(PERMISSIONS, &[])?;
    ensure(&list, "listing", list.status == 0)?;
    let mut lines = list.out.lines();
    let header = lines.next().unwrap_or("");
    ensure(
        &list,
        "a header line",
        header
            .split_whitespace()
            .eq(["APP", "ID", "PERMISSION", "DECISION"]),
    )?;
    let row = [NO_APP, "user.camera.use", "denied"];
    ensure(
        &list,
        "the denied grant",
        lines.any(|l| l.split_whitespace().eq(row)),
    )?;
    let json = run(PERMISSIONS, &["--json"])?;
    let object = format!(
        "\"app_id\":\"{NO_APP}\",\"permission\":\"user.camera.use\",\"decision\":\"denied\"}}"
    );
    ensure(&json, "the grant in JSON", json.out.contains(&object))
}

/// The broker refuses unknown, system and admin permissions.
fn permissions_refuses_bad_grants() -> Result<(), Failure> {
    let unknown = run(PERMISSIONS, &["deny", NO_APP, "no.such"])?;
    ensure(
        &unknown,
        "failing on an unknown permission",
        unknown.status == 1,
    )?;
    ensure(
        &unknown,
        "naming it",
        unknown.err == "permissions: no.such: no such permission\n",
    )?;
    for permission in ["system.devices.manage", "admin.time.set"] {
        let refused = run(PERMISSIONS, &["deny", NO_APP, permission])?;
        ensure(
            &refused,
            "failing on a system or admin permission",
            refused.status == 1,
        )?;
        let said = format!("permissions: {permission}: a system or admin permission\n");
        ensure(&refused, "saying so", refused.err == said)?;
    }
    Ok(())
}

/// Allowing always needs an administrator, for whom the broker asks and
/// who does not answer here.
fn permissions_not_authorized() -> Result<(), Failure> {
    let run = run(PERMISSIONS, &["allow", NO_APP, "user.camera.use"])?;
    ensure(&run, "failing", run.status == 1 && run.out.is_empty())?;
    ensure(
        &run,
        "saying why",
        run.err == "permissions: not authorized\n",
    )
}

/// An unknown subcommand and a missing or extra operand are refused with
/// status 2.
fn permissions_bad_arguments() -> Result<(), Failure> {
    let refused: [&[&str]; 3] = [
        &["grant", "a", "b"],
        &["allow", NO_APP],
        &["deny", NO_APP, "user.camera.use", "ada", "bob"],
    ];
    for arguments in refused {
        let run = run(PERMISSIONS, arguments)?;
        ensure(&run, "refusing the arguments", run.status == 2)?;
        ensure(
            &run,
            "naming the command",
            run.err.starts_with("permissions: "),
        )?;
    }
    Ok(())
}

/// The header, then a line per user with an ID, a name and a kind, and
/// the same users in JSON.
fn accounts_lists() -> Result<(), Failure> {
    let run = run(ACCOUNTS, &[])?;
    ensure(&run, "succeeding", run.status == 0 && run.err.is_empty())?;
    let mut lines = run.out.lines();
    let header = lines.next().unwrap_or("").split_whitespace();
    ensure(
        &run,
        "a header line",
        header.eq(["ID", "NAME", "KIND", "FULL", "NAME"]),
    )?;
    let users: Vec<Vec<&str>> = lines.map(|l| l.split_whitespace().collect()).collect();
    let well_formed = users.iter().all(|u| {
        u.len() >= 4 && u[0].parse::<u32>().is_ok() && matches!(u[2], "standard" | "admin")
    });
    ensure(&run, "a user per line", well_formed)?;
    let json = run_with(ACCOUNTS, &["--json"], None)?;
    ensure(&json, "succeeding with --json", json.status == 0)?;
    let objects = json.out.matches("{\"id\":").count();
    ensure(&json, "an object per user", objects == users.len())
}

/// Passwords come only from a terminal, so nothing is changed or added
/// without one.
fn accounts_needs_a_terminal() -> Result<(), Failure> {
    for arguments in [&["password"][..], &["add", "nobody-here"]] {
        let run = run(ACCOUNTS, arguments)?;
        ensure(&run, "failing", run.status == 1 && run.out.is_empty())?;
        ensure(
            &run,
            "saying why",
            run.err == "accounts: standard input is not a terminal\n",
        )?;
    }
    Ok(())
}

fn accounts_unknown_user() -> Result<(), Failure> {
    for arguments in [
        &["delete", "nobody-here"][..],
        &["kind", "nobody-here", "admin"],
    ] {
        let run = run(ACCOUNTS, arguments)?;
        ensure(&run, "failing", run.status == 1)?;
        ensure(
            &run,
            "naming the user",
            run.err == "accounts: nobody-here: no such user\n",
        )?;
    }
    Ok(())
}

/// An unknown subcommand, a missing or extra operand, an unknown kind, an
/// option of another subcommand, an invalid name and an empty full name
/// are refused with status 2.
fn accounts_bad_arguments() -> Result<(), Failure> {
    let refused: [&[&str]; 7] = [
        &["rename", "a"],
        &["add"],
        &["delete", "a", "b"],
        &["kind", "a", "root"],
        &["delete", "a", "--admin"],
        &["add", "Not-Valid"],
        &["full-name", "a", ""],
    ];
    for arguments in refused {
        let run = run(ACCOUNTS, arguments)?;
        ensure(&run, "refusing the arguments", run.status == 2)?;
        ensure(&run, "writing nothing", run.out.is_empty())?;
        ensure(
            &run,
            "naming the command",
            run.err.starts_with("accounts: "),
        )?;
    }
    Ok(())
}
