//! The checks of tar (MIKSPEC-0098.R57, MIKSPEC-0098.R60).

use crate::{Check, Failure, TAR, ensure, holds, run_in, scratch};

pub const CHECKS: &[(&str, Check)] = &[
    ("tar_round_trip", tar_round_trip),
    ("tar_gzip_round_trip", tar_gzip_round_trip),
    (
        "tar_lists_long_lines_and_json",
        tar_lists_long_lines_and_json,
    ),
    ("tar_names_a_missing_archive", tar_names_a_missing_archive),
    (
        "tar_refuses_entries_that_lead_out",
        tar_refuses_entries_that_lead_out,
    ),
    ("tar_bad_arguments", tar_bad_arguments),
];

/// Creates an archive, lists it and extracts it elsewhere, with its
/// contents, mode and link as they were.
fn tar_round_trip() -> Result<(), Failure> {
    let dir = scratch("tar-round-trip")?;
    dir.tree()?;
    let created = run_in(&dir, TAR, &["-cf", "a.tar", "src"], None)?;
    ensure(
        &created,
        "creating",
        created.status == 0 && created.err.is_empty(),
    )?;
    let listed = run_in(&dir, TAR, &["-tf", "a.tar"], None)?;
    ensure(
        &listed,
        "listing every entry",
        listed.out == "src/\nsrc/a.txt\nsrc/link\nsrc/run\nsrc/sub/\nsrc/sub/b.txt\n",
    )?;
    holds(
        "making the target",
        std::fs::create_dir(dir.path("out")).is_ok(),
    )?;
    let extracted = run_in(&dir, TAR, &["-xvf", "a.tar", "-C", "out"], None)?;
    ensure(&extracted, "extracting", extracted.status == 0)?;
    ensure(
        &extracted,
        "naming each entry",
        extracted.out.lines().count() == 6,
    )?;
    dir.same_tree("out/src")
}

/// A gzip archive written to standard output is read from standard
/// input, gzip found without -z.
fn tar_gzip_round_trip() -> Result<(), Failure> {
    let dir = scratch("tar-gzip")?;
    dir.tree()?;
    let gzip = std::process::Command::new(TAR)
        .args(["-cz", "-C", "src", "a.txt", "sub"])
        .current_dir(dir.path(""))
        .output()
        .map_err(|_| Failure {
            what: "running tar for its bytes",
            status: -1,
        })?;
    holds("creating to standard output", gzip.status.success())?;
    holds("gzip's magic", gzip.stdout.starts_with(&[0x1f, 0x8b]))?;
    holds(
        "making the target",
        std::fs::create_dir(dir.path("out")).is_ok(),
    )?;
    let extracted = run_in(&dir, TAR, &["-x", "-C", "out"], Some(&gzip.stdout))?;
    ensure(
        &extracted,
        "extracting from standard input",
        extracted.status == 0,
    )?;
    holds(
        "a.txt",
        dir.read("out/a.txt").as_deref() == Some(b"alpha\n"),
    )?;
    holds(
        "sub/b.txt",
        dir.read("out/sub/b.txt") == Some(vec![b'b'; 5000]),
    )
}

fn tar_lists_long_lines_and_json() -> Result<(), Failure> {
    let dir = scratch("tar-list")?;
    dir.tree()?;
    let created = run_in(
        &dir,
        TAR,
        &["-cf", "a.tar", "-C", "src", "a.txt", "link"],
        None,
    )?;
    ensure(&created, "creating", created.status == 0)?;
    let long = run_in(&dir, TAR, &["-tvf", "a.tar"], None)?;
    let lines: Vec<&str> = long.out.lines().collect();
    ensure(
        &long,
        "a line per entry",
        long.status == 0 && lines.len() == 2,
    )?;
    ensure(
        &long,
        "the type, size and name",
        lines[0].starts_with("- 6 ") && lines[0].ends_with(" a.txt"),
    )?;
    ensure(
        &long,
        "the link's target",
        lines[1].ends_with(" link -> a.txt"),
    )?;
    let json = run_in(&dir, TAR, &["-tf", "a.tar", "--json", "link"], None)?;
    ensure(
        &json,
        "the link's object",
        json.status == 0
            && json
                .out
                .starts_with("[{\"path\":\"link\",\"type\":\"link\",\"size\":0,")
            && json.out.ends_with("\"target\":\"a.txt\"}]\n"),
    )
}

fn tar_names_a_missing_archive() -> Result<(), Failure> {
    let dir = scratch("tar-missing")?;
    let run = run_in(&dir, TAR, &["-xf", "none.tar"], None)?;
    ensure(&run, "failing", run.status == 1)?;
    ensure(
        &run,
        "naming the archive",
        run.err.starts_with("tar: cannot open none.tar: "),
    )
}

/// A ustar header block of a file `name` with `size` bytes.
fn header(name: &str, size: usize) -> Vec<u8> {
    let mut block = vec![0u8; 512];
    block[..name.len()].copy_from_slice(name.as_bytes());
    block[100..108].copy_from_slice(b"0000644\0");
    block[108..116].copy_from_slice(b"0000000\0");
    block[116..124].copy_from_slice(b"0000000\0");
    block[124..136].copy_from_slice(format!("{size:011o}\0").as_bytes());
    block[136..148].copy_from_slice(b"14707060000\0");
    block[156] = b'0';
    block[257..265].copy_from_slice(b"ustar\x0000");
    block[148..156].fill(b' ');
    let sum: u32 = block.iter().map(|&b| u32::from(b)).sum();
    block[148..155].copy_from_slice(format!("{sum:06o}\0").as_bytes());
    block
}

/// An archive whose first entry leads out of the folder: it is refused
/// and named, the next entry is extracted, and tar exits with 1.
fn tar_refuses_entries_that_lead_out() -> Result<(), Failure> {
    let dir = scratch("tar-traversal")?;
    let mut archive = Vec::new();
    for (name, data) in [("../escape", b"bad"), ("ok", b"yes")] {
        archive.extend(header(name, data.len()));
        let mut block = data.to_vec();
        block.resize(512, 0);
        archive.extend(block);
    }
    archive.resize(archive.len() + 1024, 0);
    holds(
        "making the target",
        std::fs::create_dir(dir.path("out")).is_ok(),
    )?;
    let run = run_in(&dir, TAR, &["-x", "-C", "out"], Some(&archive))?;
    ensure(&run, "failing", run.status == 1)?;
    ensure(
        &run,
        "naming the entry",
        run.err.contains("../escape: refused"),
    )?;
    ensure(&run, "not writing outside", dir.read("escape").is_none())?;
    ensure(
        &run,
        "extracting the next",
        dir.read("out/ok").as_deref() == Some(b"yes"),
    )
}

fn tar_bad_arguments() -> Result<(), Failure> {
    let dir = scratch("tar-arguments")?;
    let run = run_in(&dir, TAR, &["-cx", "a"], None)?;
    ensure(&run, "exiting with 2", run.status == 2)?;
    ensure(&run, "naming the problem", run.err.starts_with("tar: "))
}
