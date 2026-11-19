//! The checks of cp and mv.

use crate::{Check, Failure, Scratch, ensure, holds, run, scratch, utility};
use std::os::unix::fs::MetadataExt;

pub const CHECKS: &[(&str, Check)] = &[
    ("cp_copies_a_file", cp_copies_a_file),
    ("cp_copies_into_a_folder", cp_copies_into_a_folder),
    ("cp_copies_folders_with_r", cp_copies_folders_with_r),
    ("cp_archive_keeps_links", cp_archive_keeps_links),
    ("cp_keeps_times_and_links", cp_keeps_times_and_links),
    ("cp_does_not_clobber", cp_does_not_clobber),
    ("cp_names_a_missing_source", cp_names_a_missing_source),
    ("cp_bad_arguments", cp_bad_arguments),
    ("mv_renames", mv_renames),
    ("mv_does_not_clobber", mv_does_not_clobber),
    ("mv_names_a_missing_source", mv_names_a_missing_source),
    (
        "mv_refuses_a_folder_into_itself",
        mv_refuses_a_folder_into_itself,
    ),
    ("mv_bad_arguments", mv_bad_arguments),
];

fn cp() -> String {
    utility("cp")
}

fn mv() -> String {
    utility("mv")
}

/// A folder with the file `a`, and the folder `x` that holds the file
/// `f` and the folder `y` with the file `g`.
fn tree(name: &str) -> Result<Scratch, Failure> {
    let dir = scratch(name)?;
    dir.write("a", "hello\n")?;
    std::fs::create_dir_all(dir.at("x/y")).map_err(|_| Failure {
        what: "making folders",
        status: -1,
    })?;
    dir.write("x/f", "one")?;
    dir.write("x/y/g", "two")?;
    Ok(dir)
}

/// A copy has the source's contents, and `-v` names both.
fn cp_copies_a_file() -> Result<(), Failure> {
    let dir = tree("cp-file")?;
    let (a, b) = (dir.at("a"), dir.at("b"));
    let copied = run(&cp(), &["-v", &a, &b])?;
    ensure(
        &copied,
        "succeeding",
        copied.status == 0 && copied.err.is_empty(),
    )?;
    ensure(
        &copied,
        "naming the copy",
        copied.out == format!("{a} -> {b}\n"),
    )?;
    holds(
        "the same contents",
        dir.read("b").as_deref() == Some("hello\n"),
    )?;
    let again = run(&cp(), &["--reflink=never", &dir.at("x/f"), &b])?;
    ensure(&again, "replacing a file", again.status == 0)?;
    holds("the new contents", dir.read("b").as_deref() == Some("one"))
}

/// Several sources go into the folder that the last operand or `-t`
/// names, under their own names.
fn cp_copies_into_a_folder() -> Result<(), Failure> {
    let dir = tree("cp-into")?;
    let (a, f, y) = (dir.at("a"), dir.at("x/f"), dir.at("x/y"));
    let copied = run(&cp(), &[&a, &f, &y])?;
    ensure(&copied, "succeeding", copied.status == 0)?;
    holds(
        "a in the folder",
        dir.read("x/y/a").as_deref() == Some("hello\n"),
    )?;
    holds(
        "f in the folder",
        dir.read("x/y/f").as_deref() == Some("one"),
    )?;
    let target = run(&cp(), &["-t", &dir.at("x"), &a])?;
    ensure(&target, "succeeding with -t", target.status == 0)?;
    holds(
        "a in the -t folder",
        dir.read("x/a").as_deref() == Some("hello\n"),
    )
}

/// `-r` copies a folder with what it holds; without it the folder is
/// refused, and a folder is not copied into itself.
fn cp_copies_folders_with_r() -> Result<(), Failure> {
    let dir = tree("cp-r")?;
    let (x, z) = (dir.at("x"), dir.at("z"));
    let refused = run(&cp(), &[&x, &z])?;
    ensure(
        &refused,
        "refusing a folder without -r",
        refused.status == 1,
    )?;
    ensure(
        &refused,
        "saying why",
        refused.err.starts_with("cp: cannot copy the folder ") && !dir.exists("z"),
    )?;
    let copied = run(&cp(), &["-r", &x, &z])?;
    ensure(&copied, "succeeding with -r", copied.status == 0)?;
    holds("the file", dir.read("z/f").as_deref() == Some("one"))?;
    holds(
        "the folder's file",
        dir.read("z/y/g").as_deref() == Some("two"),
    )?;
    let itself = run(&cp(), &["-R", &x, &dir.at("x/y")])?;
    ensure(&itself, "refusing a copy into itself", itself.status == 1)?;
    ensure(&itself, "saying so", itself.err.contains("into itself"))
}

/// `-a` copies links as links, and the folder's contents.
fn cp_archive_keeps_links() -> Result<(), Failure> {
    let dir = tree("cp-a")?;
    std::os::unix::fs::symlink("f", dir.at("x/l")).map_err(|_| Failure {
        what: "making a link",
        status: -1,
    })?;
    let copied = run(&cp(), &["-a", &dir.at("x"), &dir.at("z")])?;
    ensure(
        &copied,
        "succeeding with -a",
        copied.status == 0 && copied.err.is_empty(),
    )?;
    let link = std::fs::read_link(dir.at("z/l")).ok();
    holds(
        "the link as a link",
        link.is_some_and(|t| t.as_os_str() == "f"),
    )?;
    holds(
        "the folder's file",
        dir.read("z/y/g").as_deref() == Some("two"),
    )
}

/// `-p` keeps the modified time, and `-l` and `-s` make links.
fn cp_keeps_times_and_links() -> Result<(), Failure> {
    let dir = tree("cp-p")?;
    let (a, b) = (dir.at("a"), dir.at("b"));
    let kept = run(&cp(), &["-p", &a, &b])?;
    ensure(
        &kept,
        "succeeding with -p",
        kept.status == 0 && kept.err.is_empty(),
    )?;
    let modified = |p: &str| {
        std::fs::metadata(p)
            .ok()
            .map(|m| (m.mtime(), m.mtime_nsec()))
    };
    holds(
        "the same modified time",
        modified(&a).is_some() && modified(&a) == modified(&b),
    )?;
    let hard = run(&cp(), &["-l", &a, &dir.at("h")])?;
    ensure(&hard, "succeeding with -l", hard.status == 0)?;
    let inode = |p: &str| std::fs::metadata(p).ok().map(|m| m.ino());
    holds(
        "a hard link",
        inode(&a).is_some() && inode(&a) == inode(&dir.at("h")),
    )?;
    let soft = run(&cp(), &["-s", &a, &dir.at("s")])?;
    ensure(&soft, "succeeding with -s", soft.status == 0)?;
    holds(
        "a link to the file",
        dir.read("s").as_deref() == Some("hello\n"),
    )?;
    let link = std::fs::symlink_metadata(dir.at("s")).is_ok_and(|m| m.file_type().is_symlink());
    holds("a symbolic link", link)
}

/// `-n` leaves a destination as it is, and so does `-u` when the
/// destination is newer.
fn cp_does_not_clobber() -> Result<(), Failure> {
    let dir = tree("cp-n")?;
    let b = dir.write("b", "old")?;
    let kept = run(&cp(), &["-n", &dir.at("a"), &b])?;
    ensure(&kept, "succeeding with -n", kept.status == 0)?;
    holds("the old contents", dir.read("b").as_deref() == Some("old"))?;
    let update = run(&cp(), &["-u", &dir.at("a"), &b])?;
    ensure(&update, "succeeding with -u", update.status == 0)?;
    holds(
        "still the old contents",
        dir.read("b").as_deref() == Some("old"),
    )
}

/// A missing source is named, the other sources are still copied, and
/// cp exits with 1.
fn cp_names_a_missing_source() -> Result<(), Failure> {
    let dir = tree("cp-missing")?;
    let missing = dir.at("nothing");
    let copied = run(&cp(), &[&missing, &dir.at("a"), &dir.at("x")])?;
    ensure(&copied, "failing", copied.status == 1)?;
    ensure(
        &copied,
        "naming the missing source",
        copied.err.starts_with("cp: cannot access ") && copied.err.contains(&missing),
    )?;
    holds(
        "still copying a",
        dir.read("x/a").as_deref() == Some("hello\n"),
    )
}

/// One operand, -t with -T, and an unknown `--reflink` are refused with
/// status 2; several sources need a folder.
fn cp_bad_arguments() -> Result<(), Failure> {
    let dir = tree("cp-arguments")?;
    let a = dir.at("a");
    let x = dir.at("x");
    let refused: [&[&str]; 3] = [&[&a], &["-T", "-t", &x, &a], &["--reflink=maybe", &a, &x]];
    for arguments in refused {
        let run = run(&cp(), arguments)?;
        ensure(&run, "refusing the arguments", run.status == 2)?;
        ensure(&run, "naming the command", run.err.starts_with("cp: "))?;
    }
    let several = run(&cp(), &[&a, &a, &dir.at("x/f")])?;
    ensure(&several, "failing without a folder", several.status == 1)?;
    ensure(
        &several,
        "saying so",
        several.err.contains("is not a folder"),
    )
}

/// `mv` renames, and `-v` says so.
fn mv_renames() -> Result<(), Failure> {
    let dir = tree("mv")?;
    let (a, b) = (dir.at("a"), dir.at("b"));
    let moved = run(&mv(), &["-v", &a, &b])?;
    ensure(
        &moved,
        "succeeding",
        moved.status == 0 && moved.err.is_empty(),
    )?;
    ensure(
        &moved,
        "naming the move",
        moved.out == format!("renamed {a} -> {b}\n"),
    )?;
    holds("the source gone", !dir.exists("a"))?;
    holds(
        "the contents moved",
        dir.read("b").as_deref() == Some("hello\n"),
    )?;
    let into = run(&mv(), &[&b, &dir.at("x")])?;
    ensure(&into, "moving into a folder", into.status == 0)?;
    holds(
        "in the folder",
        dir.read("x/b").as_deref() == Some("hello\n"),
    )?;
    let folder = run(&mv(), &[&dir.at("x"), &dir.at("w")])?;
    ensure(&folder, "renaming a folder", folder.status == 0)?;
    holds(
        "the folder's file",
        dir.read("w/y/g").as_deref() == Some("two"),
    )
}

/// `-n` leaves both files as they are.
fn mv_does_not_clobber() -> Result<(), Failure> {
    let dir = tree("mv-n")?;
    let b = dir.write("b", "old")?;
    let kept = run(&mv(), &["-n", &dir.at("a"), &b])?;
    ensure(&kept, "succeeding with -n", kept.status == 0)?;
    holds("the old contents", dir.read("b").as_deref() == Some("old"))?;
    holds("the source kept", dir.exists("a"))
}

/// A missing source is named, the others still move, and mv exits
/// with 1.
fn mv_names_a_missing_source() -> Result<(), Failure> {
    let dir = tree("mv-missing")?;
    let missing = dir.at("nothing");
    let moved = run(&mv(), &[&missing, &dir.at("a"), &dir.at("x")])?;
    ensure(&moved, "failing", moved.status == 1)?;
    ensure(
        &moved,
        "naming the missing source",
        moved.err.starts_with("mv: cannot access ") && moved.err.contains(&missing),
    )?;
    holds(
        "still moving a",
        dir.read("x/a").as_deref() == Some("hello\n"),
    )
}

fn mv_refuses_a_folder_into_itself() -> Result<(), Failure> {
    let dir = tree("mv-itself")?;
    let moved = run(&mv(), &[&dir.at("x"), &dir.at("x/y")])?;
    ensure(&moved, "failing", moved.status == 1)?;
    ensure(&moved, "saying so", moved.err.contains("into itself"))?;
    holds("the folder kept", dir.read("x/f").as_deref() == Some("one"))
}

/// One operand, and two operands with -T and a third, are refused with
/// status 2.
fn mv_bad_arguments() -> Result<(), Failure> {
    let dir = tree("mv-arguments")?;
    let a = dir.at("a");
    let refused: [&[&str]; 2] = [&[&a], &["-T", &a, &dir.at("x"), &dir.at("b")]];
    for arguments in refused {
        let run = run(&mv(), arguments)?;
        ensure(&run, "refusing the arguments", run.status == 2)?;
        ensure(&run, "naming the command", run.err.starts_with("mv: "))?;
    }
    holds("nothing moved", dir.exists("a"))
}
