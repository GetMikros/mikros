//! The checks of zip and unzip (MIKSPEC-0098.R58, MIKSPEC-0098.R59,
//! MIKSPEC-0098.R60).

use crate::{Check, Failure, UNZIP, ZIP, ensure, holds, run_in, scratch};

pub const CHECKS: &[(&str, Check)] = &[
    ("zip_round_trip", zip_round_trip),
    ("zip_replaces_and_removes", zip_replaces_and_removes),
    ("zip_names_what_is_missing", zip_names_what_is_missing),
    ("unzip_lists_json_and_pipes", unzip_lists_json_and_pipes),
    (
        "unzip_names_a_missing_archive",
        unzip_names_a_missing_archive,
    ),
    (
        "unzip_refuses_entries_that_lead_out",
        unzip_refuses_entries_that_lead_out,
    ),
    ("unzip_bad_arguments", unzip_bad_arguments),
];

/// Creates an archive, lists and checks it and extracts it elsewhere,
/// with its contents, mode and link as they were.
fn zip_round_trip() -> Result<(), Failure> {
    let dir = scratch("zip-round-trip")?;
    dir.tree()?;
    let created = run_in(&dir, ZIP, &["-ry", "a.zip", "src"], None)?;
    ensure(
        &created,
        "creating",
        created.status == 0 && created.err.is_empty(),
    )?;
    ensure(
        &created,
        "naming each entry",
        created.out.lines().count() == 6,
    )?;
    let listed = run_in(&dir, UNZIP, &["-l", "a.zip"], None)?;
    let names: Vec<&str> = listed
        .out
        .lines()
        .skip(1)
        .filter_map(|l| l.split_whitespace().last())
        .collect();
    ensure(
        &listed,
        "listing every entry",
        names
            == [
                "src/",
                "src/a.txt",
                "src/link",
                "src/run",
                "src/sub/",
                "src/sub/b.txt",
            ],
    )?;
    let tested = run_in(&dir, UNZIP, &["-tq", "a.zip"], None)?;
    ensure(
        &tested,
        "checking",
        tested.status == 0 && tested.out.is_empty(),
    )?;
    let extracted = run_in(&dir, UNZIP, &["-q", "-d", "out", "a.zip"], None)?;
    ensure(
        &extracted,
        "extracting",
        extracted.status == 0 && extracted.out.is_empty(),
    )?;
    dir.same_tree("out/src")
}

/// Adding a file of the same name replaces its entry, and -d removes
/// entries.
fn zip_replaces_and_removes() -> Result<(), Failure> {
    let dir = scratch("zip-update")?;
    dir.write("a", b"first")?;
    dir.write("b", b"other")?;
    let created = run_in(&dir, ZIP, &["-q", "a.zip", "a", "b"], None)?;
    ensure(&created, "creating", created.status == 0)?;
    dir.write("a", b"second")?;
    let updated = run_in(&dir, ZIP, &["a.zip", "a"], None)?;
    ensure(
        &updated,
        "updating",
        updated.status == 0 && updated.out.contains("updating: a"),
    )?;
    let piped = run_in(&dir, UNZIP, &["-p", "a.zip", "a"], None)?;
    ensure(&piped, "the new contents", piped.out == "second")?;
    let removed = run_in(&dir, ZIP, &["-d", "a.zip", "b"], None)?;
    ensure(&removed, "removing", removed.status == 0)?;
    let listed = run_in(&dir, UNZIP, &["-l", "a.zip", "--json"], None)?;
    ensure(
        &listed,
        "one entry left",
        listed.out.starts_with("[{\"path\":\"a\",") && !listed.out.contains("\"b\""),
    )
}

fn zip_names_what_is_missing() -> Result<(), Failure> {
    let dir = scratch("zip-missing")?;
    dir.write("a", b"here")?;
    let run = run_in(&dir, ZIP, &["-q", "a.zip", "none", "a"], None)?;
    ensure(&run, "failing", run.status == 1)?;
    ensure(
        &run,
        "naming the path",
        run.err.starts_with("zip: cannot access none: "),
    )?;
    let listed = run_in(&dir, UNZIP, &["-l", "a.zip"], None)?;
    ensure(
        &listed,
        "still storing the other",
        listed.out.ends_with("  a\n"),
    )
}

fn unzip_lists_json_and_pipes() -> Result<(), Failure> {
    let dir = scratch("unzip-json")?;
    dir.write("f", &[b'x'; 1000])?;
    let created = run_in(&dir, ZIP, &["-q9", "a.zip", "f"], None)?;
    ensure(&created, "creating", created.status == 0)?;
    let json = run_in(&dir, UNZIP, &["-l", "--json", "a.zip"], None)?;
    ensure(
        &json,
        "the entry's object",
        json.status == 0
            && json
                .out
                .starts_with("[{\"path\":\"f\",\"size\":1000,\"compressed\":")
            && json.out.contains("\"modified\":\""),
    )?;
    let piped = run_in(&dir, UNZIP, &["-p", "a.zip"], None)?;
    ensure(
        &piped,
        "the contents",
        piped.status == 0 && piped.out == "x".repeat(1000),
    )
}

fn unzip_names_a_missing_archive() -> Result<(), Failure> {
    let dir = scratch("unzip-missing")?;
    let run = run_in(&dir, UNZIP, &["none.zip"], None)?;
    ensure(&run, "failing", run.status == 1)?;
    ensure(
        &run,
        "naming the archive",
        run.err.starts_with("unzip: cannot open none.zip: "),
    )
}

/// The CRC-32 of `data`.
fn crc32(data: &[u8]) -> u32 {
    let mut crc = !0u32;
    for &byte in data {
        crc ^= u32::from(byte);
        for _ in 0..8 {
            crc = (crc >> 1) ^ (0xedb8_8320 & (crc & 1).wrapping_neg());
        }
    }
    !crc
}

/// A zip archive of stored entries.
fn archive(entries: &[(&str, &[u8])]) -> Vec<u8> {
    let mut out = Vec::new();
    let mut central = Vec::new();
    for (name, data) in entries {
        let offset = out.len() as u32;
        let mut fixed = Vec::new();
        fixed.extend(10u16.to_le_bytes());
        fixed.extend(0u16.to_le_bytes());
        fixed.extend(0u16.to_le_bytes());
        fixed.extend([0, 0, 0x21, 0x5b]);
        fixed.extend(crc32(data).to_le_bytes());
        fixed.extend((data.len() as u32).to_le_bytes());
        fixed.extend((data.len() as u32).to_le_bytes());
        fixed.extend((name.len() as u16).to_le_bytes());
        fixed.extend(0u16.to_le_bytes());
        out.extend(0x0403_4b50u32.to_le_bytes());
        out.extend(&fixed);
        out.extend(name.as_bytes());
        out.extend(*data);
        central.extend(0x0201_4b50u32.to_le_bytes());
        central.extend(10u16.to_le_bytes());
        central.extend(&fixed);
        central.extend([0; 10]);
        central.extend(offset.to_le_bytes());
        central.extend(name.as_bytes());
    }
    let start = out.len() as u32;
    let size = central.len() as u32;
    out.extend(central);
    out.extend(0x0605_4b50u32.to_le_bytes());
    out.extend([0; 4]);
    out.extend((entries.len() as u16).to_le_bytes());
    out.extend((entries.len() as u16).to_le_bytes());
    out.extend(size.to_le_bytes());
    out.extend(start.to_le_bytes());
    out.extend([0; 2]);
    out
}

/// An archive whose first entry leads out of the folder: it is refused
/// and named, the next entry is extracted, and unzip exits with 1.
fn unzip_refuses_entries_that_lead_out() -> Result<(), Failure> {
    let dir = scratch("unzip-traversal")?;
    dir.write(
        "bad.zip",
        &archive(&[("../escape", b"bad"), ("ok", b"yes")]),
    )?;
    let run = run_in(&dir, UNZIP, &["-q", "-d", "out", "bad.zip"], None)?;
    ensure(&run, "failing", run.status == 1)?;
    ensure(
        &run,
        "naming the entry",
        run.err.contains("../escape: refused"),
    )?;
    holds("not writing outside", dir.read("escape").is_none())?;
    holds(
        "extracting the next",
        dir.read("out/ok").as_deref() == Some(b"yes"),
    )
}

fn unzip_bad_arguments() -> Result<(), Failure> {
    let dir = scratch("unzip-arguments")?;
    let run = run_in(&dir, UNZIP, &["-lt", "a.zip"], None)?;
    ensure(&run, "exiting with 2", run.status == 2)?;
    ensure(&run, "naming the problem", run.err.starts_with("unzip: "))
}
