//! The checks of echo, yes, head, tail, tee and wc. echo has no error of
//! its own to check: every argument it does not take as an option is
//! text.

use crate::{Check, Failure, ensure, file, path, refused, run, run_with, utility, wrote};
use std::process::{Command, Stdio};
use std::time::Duration;

pub fn checks() -> Vec<(&'static str, Check)> {
    vec![
        ("echo_writes_its_operands", echo_writes_its_operands),
        (
            "yes_repeats_until_the_pipe_closes",
            yes_repeats_until_the_pipe_closes,
        ),
        (
            "yes_refuses_an_unknown_option",
            yes_refuses_an_unknown_option,
        ),
        ("head_writes_the_first_lines", head_writes_the_first_lines),
        ("head_names_a_missing_file", head_names_a_missing_file),
        ("tail_writes_the_last_lines", tail_writes_the_last_lines),
        ("tail_names_a_missing_file", tail_names_a_missing_file),
        ("tail_follows_a_growing_file", tail_follows_a_growing_file),
        ("tee_copies_to_files", tee_copies_to_files),
        (
            "tee_names_a_file_it_cannot_open",
            tee_names_a_file_it_cannot_open,
        ),
        ("wc_counts", wc_counts),
        ("wc_names_a_missing_file", wc_names_a_missing_file),
    ]
}

/// Twelve numbered lines.
fn numbered() -> Vec<u8> {
    (1..=12)
        .flat_map(|i| format!("{i}\n").into_bytes())
        .collect()
}

fn echo_writes_its_operands() -> Result<(), Failure> {
    let echo = utility("echo");
    let run = run(&echo, &["a", "b  c"])?;
    wrote(&run, "the operands and a line feed", "a b  c\n")?;
    let run = run_with(&echo, &["-ne", "x\\ty"], None)?;
    wrote(&run, "escapes and no line feed", "x\ty")?;
    let run = run_with(&echo, &["-x", "-n"], None)?;
    wrote(&run, "other arguments as text", "-x -n\n")
}

/// yes writes its line until head has read three, and ends with 0 when
/// the pipe closes.
fn yes_repeats_until_the_pipe_closes() -> Result<(), Failure> {
    let failed = |what| Failure { what, status: -1 };
    let mut yes = Command::new(utility("yes"))
        .args(["ab", "c"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|_| failed("starting yes"))?;
    let lines = yes.stdout.take().ok_or(failed("the pipe of yes"))?;
    let head = Command::new(utility("head"))
        .args(["-n", "3"])
        .stdin(lines)
        .stdout(Stdio::piped())
        .output()
        .map_err(|_| failed("running head"))?;
    let out = String::from_utf8_lossy(&head.stdout);
    if out != "ab c\nab c\nab c\n" {
        let _ = yes.kill();
        return Err(failed("three lines of yes"));
    }
    let ended = yes
        .wait_with_output()
        .map_err(|_| failed("waiting for yes"))?;
    let status = ended.status.code().unwrap_or(-1);
    match status == 0 && ended.stderr.is_empty() {
        true => Ok(()),
        false => Err(Failure {
            what: "yes ending quietly with 0",
            status,
        }),
    }
}

fn yes_refuses_an_unknown_option() -> Result<(), Failure> {
    let run = run(&utility("yes"), &["--nope"])?;
    refused(&run, "refusing --nope", "yes", 2)?;
    ensure(&run, "writing nothing", run.out.is_empty())
}

fn head_writes_the_first_lines() -> Result<(), Failure> {
    let head = utility("head");
    let numbers = file("head-numbers", &numbered())?;
    let run = run(&head, &[&numbers])?;
    let ten: String = (1..=10).map(|i| format!("{i}\n")).collect();
    wrote(&run, "the first ten lines", &ten)?;
    let run = run_with(&head, &["-n", "-9", &numbers], None)?;
    wrote(&run, "all but the last nine", "1\n2\n3\n")?;
    let run = run_with(&head, &["-c", "3"], Some(b"abcdef"))?;
    wrote(&run, "three bytes of standard input", "abc")?;
    let other = file("head-other", b"x\ny\n")?;
    let run = run_with(&head, &["-n1", &numbers, &other], None)?;
    let titled = format!("==> {numbers} <==\n1\n\n==> {other} <==\nx\n");
    wrote(&run, "titles above several files", &titled)
}

fn head_names_a_missing_file() -> Result<(), Failure> {
    let head = utility("head");
    let numbers = file("head-numbers", &numbered())?;
    let missing = path("head-missing");
    let run = run(&head, &["-q", "-n2", &missing, &numbers])?;
    refused(&run, "failing", "head", 1)?;
    ensure(&run, "naming the file", run.err.contains("head-missing"))?;
    ensure(&run, "still writing the other", run.out == "1\n2\n")?;
    let run = run_with(&head, &["-n", "x", &numbers], None)?;
    refused(&run, "refusing a bad count", "head", 2)
}

fn tail_writes_the_last_lines() -> Result<(), Failure> {
    let tail = utility("tail");
    let numbers = file("tail-numbers", &numbered())?;
    let run = run(&tail, &[&numbers])?;
    let ten: String = (3..=12).map(|i| format!("{i}\n")).collect();
    wrote(&run, "the last ten lines", &ten)?;
    let run = run_with(&tail, &["-n", "-2", &numbers], None)?;
    wrote(&run, "the last two", "11\n12\n")?;
    let run = run_with(&tail, &["-n", "+11", &numbers], None)?;
    wrote(&run, "from line 11 on", "11\n12\n")?;
    let run = run_with(&tail, &["-c", "4"], Some(b"abcdef\n"))?;
    wrote(&run, "four bytes of standard input", "def\n")?;
    let run = run_with(&tail, &["-n1"], Some(b"a\nb"))?;
    wrote(&run, "a last line without a line feed", "b")
}

fn tail_names_a_missing_file() -> Result<(), Failure> {
    let tail = utility("tail");
    let numbers = file("tail-numbers", &numbered())?;
    let missing = path("tail-missing");
    let run = run(&tail, &["-q", "-n1", &missing, &numbers])?;
    refused(&run, "failing", "tail", 1)?;
    ensure(&run, "naming the file", run.err.contains("tail-missing"))?;
    ensure(&run, "still writing the other", run.out == "12\n")?;
    let run = run_with(&tail, &["-c", "+x", &numbers], None)?;
    refused(&run, "refusing a bad count", "tail", 2)
}

/// With `-f`, what is added to the file while a process runs is written,
/// and tail ends after that process with `--pid`.
fn tail_follows_a_growing_file() -> Result<(), Failure> {
    let failed = |what| Failure { what, status: -1 };
    let followed = file("tail-followed", b"a\n")?;
    let exe = std::env::current_exe().map_err(|_| failed("finding the test"))?;
    let mut sleeper = Command::new(exe)
        .arg(crate::SLEEP)
        .spawn()
        .map_err(|_| failed("starting a process to outlive"))?;
    let pid = sleeper.id().to_string();
    let tail = Command::new(utility("tail"))
        .args(["-f", "--pid", &pid, &followed])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|_| failed("starting tail"))?;
    std::thread::sleep(Duration::from_secs(1));
    let added = std::fs::OpenOptions::new()
        .append(true)
        .open(&followed)
        .and_then(|mut f| std::io::Write::write_all(&mut f, b"b\n"));
    let _ = sleeper.wait();
    let output = tail
        .wait_with_output()
        .map_err(|_| failed("waiting for tail"))?;
    added.map_err(|_| failed("adding to the file"))?;
    let status = output.status.code().unwrap_or(-1);
    match status == 0 && output.stdout == b"a\nb\n" {
        true => Ok(()),
        false => Err(Failure {
            what: "the line added, then an end after the process",
            status,
        }),
    }
}

fn tee_copies_to_files() -> Result<(), Failure> {
    let tee = utility("tee");
    let one = path("tee-one");
    let two = file("tee-two", b"old\n")?;
    let run = run_with(&tee, &[&one, "-a", &two], Some(b"text\n"))?;
    wrote(&run, "the input on standard output", "text\n")?;
    let one_text = std::fs::read(&one).unwrap_or_default();
    let two_text = std::fs::read(&two).unwrap_or_default();
    ensure(&run, "the input in the new file", one_text == b"text\n")?;
    ensure(
        &run,
        "the input added to the other",
        two_text == b"old\ntext\n",
    )?;
    let run = run_with(&tee, &[&one], Some(b"new\n"))?;
    let one_text = std::fs::read(&one).unwrap_or_default();
    ensure(&run, "the file replaced without -a", one_text == b"new\n")
}

fn tee_names_a_file_it_cannot_open() -> Result<(), Failure> {
    let unreachable = path("tee-no-folder/file");
    let run = run_with(&utility("tee"), &[&unreachable], Some(b"text\n"))?;
    refused(&run, "failing", "tee", 1)?;
    ensure(&run, "naming the file", run.err.contains("tee-no-folder"))?;
    ensure(
        &run,
        "still copying to standard output",
        run.out == "text\n",
    )
}

fn wc_counts() -> Result<(), Failure> {
    let wc = utility("wc");
    let text = file("wc-text", "one two\nthree\ngrüße\n".as_bytes())?;
    let run = run(&wc, &[&text])?;
    wrote(
        &run,
        "lines, words and bytes",
        &format!(" 3  4 22 {text}\n"),
    )?;
    let run = run_with(&wc, &["-lm"], Some("äb\nc".as_bytes()))?;
    wrote(&run, "lines and characters of standard input", "1 4\n")?;
    let run = run_with(&wc, &["--json", &text], None)?;
    let json = format!(
        "[{{\"path\":\"{text}\",\"lines\":3,\"words\":4,\"bytes\":22,\"characters\":20}}]\n"
    );
    wrote(&run, "an object per file", &json)
}

fn wc_names_a_missing_file() -> Result<(), Failure> {
    let wc = utility("wc");
    let text = file("wc-text", b"a b\n")?;
    let missing = path("wc-missing");
    let run = run(&wc, &["-w", &text, &missing])?;
    refused(&run, "failing", "wc", 1)?;
    ensure(&run, "naming the file", run.err.contains("wc-missing"))?;
    let counted = format!("2 {text}\n2 total\n");
    ensure(&run, "counting the other, with a total", run.out == counted)
}
