//! dd and sync: copying blocks between files and the standard streams,
//! with what dd reports, and syncing files, folders and the namespace.

use crate::{Check, Failure, ensure, holds, pattern, run, run_with, scratch, utility};

pub const CHECKS: &[(&str, Check)] = &[
    ("dd_copies_blocks", dd_copies_blocks),
    ("dd_streams_and_notrunc", dd_streams_and_notrunc),
    ("dd_names_what_is_wrong", dd_names_what_is_wrong),
    ("sync_syncs", sync_syncs),
    ("sync_names_what_is_missing", sync_names_what_is_missing),
];

/// `skip=` and `count=` choose the blocks copied to `of=`, and the
/// records, bytes and time go to standard error.
fn dd_copies_blocks() -> Result<(), Failure> {
    let dir = scratch("dd")?;
    let data = pattern(1000);
    let input = dir.write("in", &data)?;
    let output = dir.at("out");
    let arguments = [
        format!("if={input}"),
        format!("of={output}"),
        "bs=100".into(),
        "count=5".into(),
        "skip=1".into(),
        "conv=fsync".into(),
    ];
    let arguments: Vec<&str> = arguments.iter().map(String::as_str).collect();
    let run = run(&utility("dd"), &arguments)?;
    ensure(&run, "succeeding", run.status == 0 && run.out.is_empty())?;
    holds(
        "the blocks chosen",
        dir.read("out").as_deref() == Some(&data[100..600]),
    )?;
    ensure(
        &run,
        "the records, bytes and time",
        run.err
            .starts_with("5+0 records in\n5+0 records out\n500 bytes copied, ")
            && run.err.trim_end().ends_with("/s"),
    )
}

/// Standard input goes to standard output in blocks, the last partial;
/// `seek=` with `conv=notrunc` writes inside a file and keeps the rest.
fn dd_streams_and_notrunc() -> Result<(), Failure> {
    let dir = scratch("dd-streams")?;
    let data = pattern(250);
    let piped = run_with(&utility("dd"), &["bs=100", "status=noxfer"], Some(&data))?;
    ensure(&piped, "succeeding with streams", piped.status == 0)?;
    ensure(&piped, "the input on the output", piped.bytes == data)?;
    let records_in = piped.err.lines().next().unwrap_or_default();
    ensure(
        &piped,
        "a partial record and no transfer line",
        records_in.ends_with("+1 records in") && piped.err.lines().count() == 2,
    )?;
    let target = dir.write("target", &[b'.'; 50])?;
    let patch = dir.write("patch", b"0123456789")?;
    let arguments = [
        format!("if={patch}"),
        format!("of={target}"),
        "bs=10".into(),
        "seek=2".into(),
        "conv=notrunc".into(),
        "status=none".into(),
    ];
    let arguments: Vec<&str> = arguments.iter().map(String::as_str).collect();
    let patched = run(&utility("dd"), &arguments)?;
    ensure(
        &patched,
        "succeeding quietly",
        patched.status == 0 && patched.err.is_empty(),
    )?;
    let mut expected = vec![b'.'; 50];
    expected[20..30].copy_from_slice(b"0123456789");
    holds(
        "the file patched in place",
        dir.read("target") == Some(expected),
    )
}

/// An input that is not there is named and dd exits with 1; a bad
/// operand is refused with 2.
fn dd_names_what_is_wrong() -> Result<(), Failure> {
    let dir = scratch("dd-missing")?;
    let missing = dir.at("nothing");
    let operand = format!("if={missing}");
    let run_ = run(&utility("dd"), &[&operand])?;
    ensure(
        &run_,
        "failing and naming the input",
        run_.status == 1
            && run_.err.starts_with("dd: cannot open ")
            && run_.err.contains("nothing"),
    )?;
    let bad = run(&utility("dd"), &["bs=0"])?;
    ensure(&bad, "refusing a block size of 0", bad.status == 2)?;
    let unknown = run(&utility("dd"), &["colour=red"])?;
    ensure(&unknown, "refusing an unknown operand", unknown.status == 2)
}

/// sync syncs a file and a folder, and without paths the writable
/// entries of the namespace.
fn sync_syncs() -> Result<(), Failure> {
    let dir = scratch("sync")?;
    let file = dir.write("a", b"kept")?;
    let named = run(&utility("sync"), &[&file, &dir.path()])?;
    ensure(
        &named,
        "succeeding with paths",
        named.status == 0 && named.err.is_empty() && named.out.is_empty(),
    )?;
    let all = run(&utility("sync"), &[])?;
    ensure(
        &all,
        "succeeding without paths",
        all.status == 0 && all.err.is_empty(),
    )
}

/// A path that is not there is named, the others are still synced, and
/// sync exits with 1.
fn sync_names_what_is_missing() -> Result<(), Failure> {
    let dir = scratch("sync-missing")?;
    let file = dir.write("a", b"")?;
    let missing = dir.at("nothing");
    let run_ = run(&utility("sync"), &[&missing, &file])?;
    ensure(
        &run_,
        "failing and naming only the missing path",
        run_.status == 1
            && run_.err.starts_with(&format!("sync: {missing}: "))
            && run_.err.lines().count() == 1,
    )?;
    let bad = run(&utility("sync"), &["-x"])?;
    ensure(&bad, "refusing an unknown option", bad.status == 2)
}
