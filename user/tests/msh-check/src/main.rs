//! A program that runs msh, the shell, for the process manager tests.
//! Run with the single argument `world`, it runs msh's scripts below
//! with `std::process::Command`, checks what each wrote to standard
//! output and error and its exit status, prints a greeting and exits
//! with 0, or with the number of the first check that failed, after
//! printing what msh did instead. spawn-std is the program that the
//! scripts start.

use std::io::Write;
use std::process::{Command, ExitCode, Stdio};

/// The shell, as the boot image holds it.
const MSH: &str = "/usr/bin/msh";
/// A script with an option `--n`, which prints `$n + 1` and `$args`.
const OPTIONS: &str = "/usr/share/msh/options.msh";
const GREETING: &str = "a greeting from the boot file system\n";

/// What msh wrote, and how it ended.
#[derive(Debug, PartialEq)]
struct Ran {
    stdout: String,
    stderr: String,
    code: Option<i32>,
}

fn ran(stdout: &str, stderr: &str, code: i32) -> Ran {
    Ran {
        stdout: stdout.into(),
        stderr: stderr.into(),
        code: Some(code),
    }
}

/// Runs msh with `args`, giving it `input` on standard input.
fn msh(args: &[&str], input: &str) -> Ran {
    let child = Command::new(MSH)
        .args(args)
        .env_clear()
        .env("PATH", "/usr/bin")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn();
    let mut child = match child {
        Ok(child) => child,
        Err(e) => return ran("", &format!("not started: {e}"), -1),
    };
    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin.write_all(input.as_bytes());
    }
    match child.wait_with_output() {
        Ok(out) => Ran {
            stdout: String::from_utf8_lossy(&out.stdout).into(),
            stderr: String::from_utf8_lossy(&out.stderr).into(),
            code: out.status.code(),
        },
        Err(e) => ran("", &format!("not waited for: {e}"), -1),
    }
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args != ["world"] {
        return ExitCode::from(200);
    }
    let greeting = "spawn-std echo < /usr/share/hello/greeting.txt";
    let piped = format!("{greeting} | spawn-std echo");
    let captured = format!("let g = ({greeting} | lines)\nprint $g.0");
    let unparenthesized = format!("let g = {greeting} | lines\nprint $g.0");
    let checks: Vec<(Vec<&str>, &str, Ran)> = vec![
        (vec!["-c", "print hello"], "", ran("hello\n", "", 0)),
        (
            vec!["-c", "nosuch"],
            "",
            ran("", "msh: -c:1: nosuch: command not found\n", 127),
        ),
        // A script on standard input.
        (vec![], "print (1 + 2)\nexit 4\n", ran("3\n", "", 4)),
        (vec![OPTIONS, "--n", "41", "x"], "", ran("42 x\n", "", 0)),
        (
            vec![OPTIONS],
            "",
            ran(
                "",
                &format!("msh: {OPTIONS}: the option `--n` is missing\n"),
                1,
            ),
        ),
        (
            vec!["-c", "print $args", "a", "b"],
            "",
            ran("a\nb\n", "", 0),
        ),
        // Pipes between programs, and redirections.
        (vec!["-c", &piped], "", ran(GREETING, "", 0)),
        (
            vec!["-c", "msh -c nosuch 2>&1 | spawn-std echo"],
            "",
            ran(
                "msh: -c:1: nosuch: command not found\n",
                "msh: -c:1: msh: exited with status 127\n",
                127,
            ),
        ),
        (
            vec!["-c", "print x | spawn-std echo >&2"],
            "",
            ran("", "x\n", 0),
        ),
        // A program's output as a value.
        (vec!["-c", &captured], "", ran(GREETING, "", 0)),
        (vec!["-c", &unparenthesized], "", ran(GREETING, "", 0)),
        (
            vec!["-c", "env.FOO = 'bar'\nspawn-std env FOO"],
            "",
            ran("bar\n", "", 0),
        ),
        // `env.NAME=value` before a command sets it for that command only.
        (
            vec![
                "-c",
                "env.FOO=bar spawn-std env FOO\nprint ($env.FOO ?? 'unset')",
            ],
            "",
            ran("bar\nunset\n", "", 0),
        ),
        (
            vec!["-c", "print /usr/share/hello/*.txt"],
            "",
            ran("/usr/share/hello/greeting.txt\n", "", 0),
        ),
        (
            vec!["-c", "cd /usr/share\npwd"],
            "",
            ran("/usr/share\n", "", 0),
        ),
        // A program starts in msh's current folder.
        (
            vec!["-c", "cd /usr/share\nspawn-std cwd"],
            "",
            ran("/usr/share\n", "", 0),
        ),
        (
            vec!["-c", "run spawn-std exit 3"],
            "",
            ran("", "msh: -c:1: spawn-std: exited with status 3\n", 3),
        ),
        // A job in the background, ended by `kill` and waited for by `fg`.
        (
            vec!["-c", "spawn-std sleep &\nkill %1\nfg"],
            "",
            ran("", "msh: -c:3: %+: ended by signal 15\n", 143),
        ),
        (vec!["-c", "exec spawn-std exit 5"], "", ran("", "", 5)),
    ];
    for (step, (args, input, expected)) in checks.into_iter().enumerate() {
        let got = msh(&args, input);
        if got != expected {
            println!("msh {args:?} gave {got:?}, not {expected:?}");
            return ExitCode::from(step as u8 + 1);
        }
    }
    println!("msh, world");
    ExitCode::SUCCESS
}
