//! The checks of grep, which the system installs in `/system/bin`.

use crate::{Check, Failure, ensure, file, path, refused, run, run_with, wrote};

const GREP: &str = "/system/bin/grep";

pub fn checks() -> Vec<(&'static str, Check)> {
    vec![
        ("grep_selects_lines", grep_selects_lines),
        ("grep_takes_its_options", grep_takes_its_options),
        ("grep_selects_nothing", grep_selects_nothing),
        ("grep_names_a_missing_file", grep_names_a_missing_file),
        ("grep_refuses_bad_patterns", grep_refuses_bad_patterns),
        ("grep_searches_folders", grep_searches_folders),
        ("grep_writes_json", grep_writes_json),
    ]
}

const FRUIT: &[u8] = b"apple\nBanana\ncherry\nbanana split\n";

/// The matching lines, with their numbers.
fn grep_selects_lines() -> Result<(), Failure> {
    let fruit = file("grep-fruit", FRUIT)?;
    let found = run(GREP, &["-n", "an", &fruit])?;
    wrote(&found, "the lines with an", "2:Banana\n4:banana split\n")
}

/// Ignoring case, counting, inverting with an extended expression, and
/// reading standard input.
fn grep_takes_its_options() -> Result<(), Failure> {
    let fruit = file("grep-fruit", FRUIT)?;
    let counted = run(GREP, &["-ic", "banana", &fruit])?;
    wrote(&counted, "counting without case", "2\n")?;
    let inverted = run(GREP, &["-v", "-E", "an|rr", &fruit])?;
    wrote(&inverted, "the lines that do not match", "apple\n")?;
    let piped = run_with(GREP, &["-x", "b.*"], Some(b"bx\nab\n"))?;
    wrote(&piped, "whole lines of standard input", "bx\n")
}

/// No line matches: nothing is written, and grep exits with 1.
fn grep_selects_nothing() -> Result<(), Failure> {
    let fruit = file("grep-fruit", FRUIT)?;
    let none = run(GREP, &["kiwi", &fruit])?;
    ensure(
        &none,
        "exiting with 1 and writing nothing",
        none.status == 1 && none.out.is_empty() && none.err.is_empty(),
    )
}

/// A missing file is named, the other is still searched, and grep exits
/// with 2.
fn grep_names_a_missing_file() -> Result<(), Failure> {
    let fruit = file("grep-fruit", FRUIT)?;
    let missing = path("grep-nothing");
    let found = run(GREP, &["cherry", &missing, &fruit])?;
    refused(&found, "failing with 2", "grep", 2)?;
    ensure(
        &found,
        "naming the missing file",
        found.err.contains("grep-nothing"),
    )?;
    ensure(
        &found,
        "still searching the other file",
        found.out == format!("{fruit}:cherry\n"),
    )
}

/// A back-reference, which grep cannot match, and an unmatched
/// parenthesis are refused with 2.
fn grep_refuses_bad_patterns() -> Result<(), Failure> {
    let fruit = file("grep-fruit", FRUIT)?;
    let back = run(GREP, &[r"\(a\)\1", &fruit])?;
    refused(&back, "refusing a back-reference", "grep", 2)?;
    ensure(&back, "saying why", back.err.contains("back-references"))?;
    let open = run(GREP, &["-E", "(a", &fruit])?;
    refused(&open, "refusing an unmatched parenthesis", "grep", 2)
}

/// `-r` searches the files below a folder whose names `--include`
/// matches.
fn grep_searches_folders() -> Result<(), Failure> {
    let tree = path("grep-tree");
    std::fs::create_dir_all(format!("{tree}/a")).map_err(|_| Failure {
        what: "making a folder for the check",
        status: -1,
    })?;
    file("grep-tree/a/x.txt", b"needle\n")?;
    file("grep-tree/b.log", b"needle\n")?;
    let found = run(GREP, &["-r", "--include", "*.txt", "needle", &tree])?;
    wrote(
        &found,
        "the line of the included file, with its path",
        &format!("{tree}/a/x.txt:needle\n"),
    )
}

/// `--json` writes an object for each selected line.
fn grep_writes_json() -> Result<(), Failure> {
    let fruit = file("grep-fruit", FRUIT)?;
    let json = run(GREP, &["--json", "cherry", &fruit])?;
    ensure(
        &json,
        "an array of one object",
        json.status == 0
            && json.out.starts_with("[{\"path\":")
            && json.out.ends_with(",\"line\":3,\"text\":\"cherry\"}]\n"),
    )
}
