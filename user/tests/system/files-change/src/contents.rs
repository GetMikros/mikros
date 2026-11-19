//! The checks of cat and touch.

use crate::{Check, Failure, ensure, holds, run, run_with, scratch, utility};
use std::os::unix::fs::MetadataExt;

pub const CHECKS: &[(&str, Check)] = &[
    ("cat_copies_files_and_input", cat_copies_files_and_input),
    ("cat_numbers_and_shows", cat_numbers_and_shows),
    ("cat_names_what_is_missing", cat_names_what_is_missing),
    ("cat_bad_arguments", cat_bad_arguments),
    ("touch_creates_files", touch_creates_files),
    ("touch_sets_times", touch_sets_times),
    ("touch_names_what_fails", touch_names_what_fails),
    ("touch_bad_arguments", touch_bad_arguments),
];

/// 2001-02-03T04:05:06Z, in seconds since 1970.
const FEBRUARY: i64 = 981_173_106;

/// The modified and access times of `path`, in seconds since 1970.
fn times(path: &str) -> Result<(i64, i64), Failure> {
    let metadata = std::fs::metadata(path).map_err(|_| Failure {
        what: "reading the times",
        status: -1,
    })?;
    Ok((metadata.mtime(), metadata.atime()))
}

/// Files, standard input and `-` among them, in the order given.
fn cat_copies_files_and_input() -> Result<(), Failure> {
    let dir = scratch("cat-copies")?;
    let a = dir.write("a", "one\n")?;
    let b = dir.write("b", "two")?;
    let cat = utility("cat");
    let files = run(&cat, &[&a, &b])?;
    ensure(
        &files,
        "succeeding",
        files.status == 0 && files.err.is_empty(),
    )?;
    ensure(&files, "copying both files", files.out == "one\ntwo")?;
    let input = run_with(&cat, &[], Some(b"in\x00put\n"))?;
    ensure(&input, "copying standard input", input.out == "in\x00put\n")?;
    let dash = run_with(&cat, &[&a, "-", &b], Some(b"mid\n"))?;
    ensure(&dash, "succeeding with -", dash.status == 0)?;
    ensure(&dash, "reading - in its place", dash.out == "one\nmid\ntwo")
}

fn cat_numbers_and_shows() -> Result<(), Failure> {
    let dir = scratch("cat-shows")?;
    let file = dir.write("f", "x\n\n\ny\tz\n")?;
    let cat = utility("cat");
    let numbered = run(&cat, &["-n", &file])?;
    ensure(&numbered, "succeeding with -n", numbered.status == 0)?;
    ensure(
        &numbered,
        "numbering every line",
        numbered.out == "     1\tx\n     2\t\n     3\t\n     4\ty\tz\n",
    )?;
    let squeezed = run(&cat, &["-sb", &file])?;
    ensure(
        &squeezed,
        "squeezing and numbering lines that are not empty",
        squeezed.out == "     1\tx\n\n     2\ty\tz\n",
    )?;
    let all = run(&cat, &["-A", &file])?;
    ensure(
        &all,
        "showing ends and tabs",
        all.out == "x$\n$\n$\ny^Iz$\n",
    )
}

/// A file that is not there and a folder are named, the other file is
/// still copied, and cat exits with 1.
fn cat_names_what_is_missing() -> Result<(), Failure> {
    let dir = scratch("cat-missing")?;
    let file = dir.write("f", "kept\n")?;
    let missing = dir.at("nothing");
    let folder = dir.at("");
    let run = run(&utility("cat"), &[&missing, &folder, &file])?;
    ensure(&run, "failing", run.status == 1)?;
    let named = format!("cat: cannot open {missing}: ");
    ensure(&run, "naming the missing file", run.err.starts_with(&named))?;
    let lines: Vec<&str> = run.err.lines().collect();
    ensure(
        &run,
        "naming the folder",
        lines.len() == 2 && lines[1].starts_with("cat: cannot ") && lines[1].contains(&folder),
    )?;
    ensure(&run, "still copying the file", run.out == "kept\n")
}

fn cat_bad_arguments() -> Result<(), Failure> {
    let run = run(&utility("cat"), &["--nope"])?;
    ensure(&run, "refusing the option", run.status == 2)?;
    ensure(&run, "naming the command", run.err.starts_with("cat: "))
}

/// A file that does not exist is made, empty, unless `-c`; one that
/// exists keeps its contents.
fn touch_creates_files() -> Result<(), Failure> {
    let dir = scratch("touch-creates")?;
    let touch = utility("touch");
    let new = dir.at("new");
    let kept = dir.write("kept", "contents")?;
    let made = run(&touch, &[&new, &kept])?;
    ensure(&made, "succeeding", made.status == 0 && made.err.is_empty())?;
    ensure(
        &made,
        "making the file",
        dir.read("new").as_deref() == Some(""),
    )?;
    let contents = dir.read("kept");
    ensure(
        &made,
        "keeping contents",
        contents.as_deref() == Some("contents"),
    )?;
    let none = dir.at("none");
    let not_made = run(&touch, &["-c", &none])?;
    ensure(&not_made, "succeeding with -c", not_made.status == 0)?;
    ensure(&not_made, "making nothing with -c", !dir.exists("none"))
}

/// `-d` and `-t` set both times, `-a` and `-m` only one, and `-r` copies
/// another file's.
fn touch_sets_times() -> Result<(), Failure> {
    let dir = scratch("touch-times")?;
    let touch = utility("touch");
    let f = dir.write("f", "")?;
    let g = dir.write("g", "")?;
    let utc = run(&touch, &["-d", "2001-02-03T04:05:06Z", &f])?;
    ensure(
        &utc,
        "succeeding with -d",
        utc.status == 0 && utc.err.is_empty(),
    )?;
    holds(
        "setting both times with -d",
        times(&f)? == (FEBRUARY, FEBRUARY),
    )?;
    let stamp = run(&touch, &["-t", "200102030405.06", &g])?;
    ensure(&stamp, "succeeding with -t", stamp.status == 0)?;
    let local = run(&touch, &["-d", "2001-02-03 04:05:06", &f])?;
    ensure(&local, "succeeding with a local -d", local.status == 0)?;
    holds(
        "-t and -d reading local time alike",
        times(&f)? == times(&g)?,
    )?;
    let only_access = run(&touch, &["-a", "-d", "@1000000000", &f])?;
    ensure(&only_access, "succeeding with -a", only_access.status == 0)?;
    let (modified, access) = times(&f)?;
    holds("setting only the access time", access == 1_000_000_000)?;
    holds("keeping the modified time", modified == times(&g)?.0)?;
    let only_modified = run(&touch, &["-m", "-d", "@2000000000", &g])?;
    ensure(
        &only_modified,
        "succeeding with -m",
        only_modified.status == 0,
    )?;
    holds(
        "setting only the modified time",
        times(&g)?.0 == 2_000_000_000,
    )?;
    let reference = run(&touch, &["-r", &g, &f])?;
    ensure(&reference, "succeeding with -r", reference.status == 0)?;
    holds("copying the times with -r", times(&f)? == times(&g)?)
}

/// A file in a folder that does not exist and a missing reference are
/// named, the other files are still made, and touch exits with 1.
fn touch_names_what_fails() -> Result<(), Failure> {
    let dir = scratch("touch-fails")?;
    let touch = utility("touch");
    let lost = dir.at("no/file");
    let made = dir.at("made");
    let partly = run(&touch, &[&lost, &made])?;
    ensure(&partly, "failing", partly.status == 1)?;
    let named = format!("touch: cannot touch {lost}: ");
    ensure(&partly, "naming the file", partly.err.starts_with(&named))?;
    ensure(&partly, "still making the other", dir.exists("made"))?;
    let missing = dir.at("nothing");
    let reference = run(&touch, &["-r", &missing, &made])?;
    ensure(
        &reference,
        "failing on a missing reference",
        reference.status == 1,
    )?;
    let named = format!("touch: cannot access {missing}: ");
    ensure(
        &reference,
        "naming the reference",
        reference.err.starts_with(&named),
    )
}

/// No file, an invalid date or stamp, and two sources of times are
/// refused with status 2, and no file is made.
fn touch_bad_arguments() -> Result<(), Failure> {
    let dir = scratch("touch-arguments")?;
    let f = dir.at("f");
    let refused: [&[&str]; 5] = [
        &[],
        &["-d", "next week", &f],
        &["-t", "2001", &f],
        &["-d", "now", "-t", "200102030405", &f],
        &["--nope", &f],
    ];
    for arguments in refused {
        let run = run(&utility("touch"), arguments)?;
        ensure(&run, "refusing the arguments", run.status == 2)?;
        ensure(&run, "naming the command", run.err.starts_with("touch: "))?;
        ensure(&run, "making no file", !dir.exists("f"))?;
    }
    Ok(())
}
