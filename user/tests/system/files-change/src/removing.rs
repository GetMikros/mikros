//! The checks of rm, rmdir, mkdir and ln.

use crate::{Check, Failure, Run, ensure, holds, run, run_with, scratch, utility};
use std::path::Path;
use std::process::{Command, Stdio};

pub const CHECKS: &[(&str, Check)] = &[
    ("rm_removes_files", rm_removes_files),
    ("rm_removes_folders", rm_removes_folders),
    ("rm_asks_with_i", rm_asks_with_i),
    ("rm_names_what_is_missing", rm_names_what_is_missing),
    ("rm_refuses_the_system", rm_refuses_the_system),
    ("rm_bad_arguments", rm_bad_arguments),
    ("rmdir_removes_parents", rmdir_removes_parents),
    ("rmdir_keeps_full_folders", rmdir_keeps_full_folders),
    ("mkdir_creates_folders", mkdir_creates_folders),
    ("mkdir_names_what_exists", mkdir_names_what_exists),
    ("ln_creates_links", ln_creates_links),
    ("ln_names_what_is_missing", ln_names_what_is_missing),
    ("ln_bad_arguments", ln_bad_arguments),
];

/// Runs `program` with `arguments` in the folder `dir`.
fn run_in(dir: &str, program: &str, arguments: &[&str]) -> Result<Run, Failure> {
    let output = Command::new(program)
        .args(arguments)
        .current_dir(dir)
        .stdin(Stdio::null())
        .output()
        .map_err(|_| Failure {
            what: "starting the command",
            status: -1,
        })?;
    Ok(Run {
        status: output.status.code().unwrap_or(-1),
        out: String::from_utf8_lossy(&output.stdout).into_owned(),
        err: String::from_utf8_lossy(&output.stderr).into_owned(),
    })
}

fn mkdirs(path: &str) -> Result<(), Failure> {
    std::fs::create_dir_all(path).map_err(|_| Failure {
        what: "making folders",
        status: -1,
    })
}

/// `rm -v` removes files and names each.
fn rm_removes_files() -> Result<(), Failure> {
    let dir = scratch("rm-files")?;
    let a = dir.write("a", "1")?;
    let b = dir.write("b", "2")?;
    let rm = run(&utility("rm"), &["-v", &a, &b])?;
    ensure(&rm, "succeeding", rm.status == 0 && rm.err.is_empty())?;
    ensure(
        &rm,
        "naming each file",
        rm.out == format!("removed {a}\nremoved {b}\n"),
    )?;
    holds("removing the files", !dir.exists("a") && !dir.exists("b"))
}

/// A folder needs `-r`, or `-d` when it is empty.
fn rm_removes_folders() -> Result<(), Failure> {
    let dir = scratch("rm-folders")?;
    mkdirs(&dir.at("d/e"))?;
    mkdirs(&dir.at("empty"))?;
    dir.write("d/e/f", "x")?;
    let d = dir.at("d");
    let plain = run(&utility("rm"), &[&d])?;
    ensure(&plain, "failing on a folder", plain.status == 1)?;
    ensure(
        &plain,
        "saying it is a folder",
        plain.err == format!("rm: cannot remove {d}: is a folder\n"),
    )?;
    holds("keeping the folder", dir.exists("d/e/f"))?;
    let full = run(&utility("rm"), &["-d", &d])?;
    ensure(&full, "failing on a full folder with -d", full.status == 1)?;
    let empty = run(&utility("rm"), &["-d", &dir.at("empty")])?;
    ensure(&empty, "removing an empty folder", empty.status == 0)?;
    let recursive = run(&utility("rm"), &["-r", &d])?;
    ensure(&recursive, "removing recursively", recursive.status == 0)?;
    holds(
        "removing the folders",
        !dir.exists("d") && !dir.exists("empty"),
    )
}

/// `-i` removes only what the answer allows.
fn rm_asks_with_i() -> Result<(), Failure> {
    let dir = scratch("rm-asks")?;
    let a = dir.write("a", "1")?;
    let no = run_with(&utility("rm"), &["-i", &a], Some(b"n\n"))?;
    ensure(&no, "succeeding", no.status == 0)?;
    ensure(&no, "asking", no.err.starts_with("rm: remove file "))?;
    holds("keeping the file", dir.exists("a"))?;
    let yes = run_with(&utility("rm"), &["-i", &a], Some(b"y\n"))?;
    ensure(&yes, "succeeding", yes.status == 0)?;
    holds("removing the file", !dir.exists("a"))
}

/// A missing path is named, the others are still removed, and `-f`
/// ignores it.
fn rm_names_what_is_missing() -> Result<(), Failure> {
    let dir = scratch("rm-missing")?;
    let missing = dir.at("nothing");
    let a = dir.write("a", "1")?;
    let rm = run(&utility("rm"), &[&missing, &a])?;
    ensure(&rm, "failing", rm.status == 1)?;
    ensure(
        &rm,
        "naming the missing path",
        rm.err
            .starts_with(&format!("rm: cannot remove {missing}: ")),
    )?;
    holds("still removing the file", !dir.exists("a"))?;
    let forced = run(&utility("rm"), &["-f", &missing])?;
    ensure(
        &forced,
        "ignoring it with -f",
        forced.status == 0 && forced.err.is_empty(),
    )
}

/// The root and the system version are refused and named.
fn rm_refuses_the_system() -> Result<(), Failure> {
    let system = run(&utility("rm"), &["-r", "/system/bin"])?;
    ensure(&system, "failing", system.status == 1)?;
    ensure(
        &system,
        "naming the system version",
        system.err
            == "rm: refusing to remove /system/bin: it is part of the read-only system version\n",
    )?;
    holds("keeping /system/bin", Path::new("/system/bin").is_dir())?;
    let root = run(&utility("rm"), &["/"])?;
    ensure(&root, "failing", root.status == 1)?;
    ensure(
        &root,
        "naming the root",
        root.err == "rm: refusing to remove /: it is the root folder\n",
    )
}

/// No operand is refused with status 2, unless with `-f`.
fn rm_bad_arguments() -> Result<(), Failure> {
    for arguments in [&[][..], &["--nope"]] {
        let rm = run(&utility("rm"), arguments)?;
        ensure(&rm, "refusing the arguments", rm.status == 2)?;
        ensure(&rm, "naming the command", rm.err.starts_with("rm: "))?;
    }
    let forced = run(&utility("rm"), &["-f"])?;
    ensure(&forced, "nothing to do with -f", forced.status == 0)
}

/// `rmdir -p` removes the folders that the path names.
fn rmdir_removes_parents() -> Result<(), Failure> {
    let dir = scratch("rmdir-parents")?;
    mkdirs(&dir.at("a/b/c"))?;
    let rmdir = run_in(&dir.at(""), &utility("rmdir"), &["-pv", "a/b/c"])?;
    ensure(&rmdir, "succeeding", rmdir.status == 0)?;
    ensure(
        &rmdir,
        "naming each folder",
        rmdir.out == "removed folder a/b/c\nremoved folder a/b\nremoved folder a\n",
    )?;
    holds("removing the folders", !dir.exists("a"))
}

/// A folder that holds something is named and kept.
fn rmdir_keeps_full_folders() -> Result<(), Failure> {
    let dir = scratch("rmdir-full")?;
    mkdirs(&dir.at("a"))?;
    dir.write("a/f", "x")?;
    let a = dir.at("a");
    let rmdir = run(&utility("rmdir"), &[&a])?;
    ensure(&rmdir, "failing", rmdir.status == 1)?;
    ensure(
        &rmdir,
        "naming the folder",
        rmdir
            .err
            .starts_with(&format!("rmdir: cannot remove {a}: ")),
    )?;
    holds("keeping the folder", dir.exists("a/f"))?;
    let none = run(&utility("rmdir"), &[])?;
    ensure(&none, "refusing no operand", none.status == 2)
}

/// `mkdir -pv` creates and names the folders above, and `-m` sets the
/// mode.
fn mkdir_creates_folders() -> Result<(), Failure> {
    let dir = scratch("mkdir")?;
    let c = dir.at("a/b/c");
    let mkdir = run(&utility("mkdir"), &["-pv", &c])?;
    ensure(&mkdir, "succeeding", mkdir.status == 0)?;
    let named = format!(
        "created folder {}\ncreated folder {}\ncreated folder {c}\n",
        dir.at("a"),
        dir.at("a/b")
    );
    ensure(&mkdir, "naming each folder", mkdir.out == named)?;
    holds("creating the folders", Path::new(&c).is_dir())?;
    let again = run(&utility("mkdir"), &["-p", &c])?;
    ensure(&again, "accepting a folder with -p", again.status == 0)?;
    let m = dir.at("m");
    let mode = run(&utility("mkdir"), &["-m", "u=rwx,go=", &m])?;
    ensure(&mode, "succeeding with -m", mode.status == 0)?;
    use std::os::unix::fs::PermissionsExt;
    let set = std::fs::metadata(&m).is_ok_and(|m| m.permissions().mode() & 0o7777 == 0o700);
    holds("setting the mode", set)?;
    let bad = run(&utility("mkdir"), &["-m", "u+q", &dir.at("n")])?;
    ensure(&bad, "refusing a bad mode", bad.status == 2)?;
    holds("creating nothing with a bad mode", !dir.exists("n"))
}

/// A folder that exists is named.
fn mkdir_names_what_exists() -> Result<(), Failure> {
    let dir = scratch("mkdir-exists")?;
    let a = dir.at("a");
    mkdirs(&a)?;
    let mkdir = run(&utility("mkdir"), &[&a])?;
    ensure(&mkdir, "failing", mkdir.status == 1)?;
    ensure(
        &mkdir,
        "naming the folder",
        mkdir
            .err
            .starts_with(&format!("mkdir: cannot create folder {a}: ")),
    )
}

/// Hard links share contents, and symbolic links hold their targets, as
/// given or, with `-r`, relative.
fn ln_creates_links() -> Result<(), Failure> {
    let dir = scratch("ln")?;
    let a = dir.write("a", "contents")?;
    let hard = run(&utility("ln"), &["-v", &a, &dir.at("h")])?;
    ensure(&hard, "creating a hard link", hard.status == 0)?;
    ensure(
        &hard,
        "naming it",
        hard.out == format!("{} -> {a}\n", dir.at("h")),
    )?;
    holds(
        "sharing the contents",
        dir.read("h").as_deref() == Some("contents"),
    )?;
    let soft = run(&utility("ln"), &["-s", "a", &dir.at("s")])?;
    ensure(&soft, "creating a symbolic link", soft.status == 0)?;
    let target = std::fs::read_link(dir.at("s")).ok();
    holds("holding its target", target == Some("a".into()))?;
    let exists = run(&utility("ln"), &["-s", "b", &dir.at("s")])?;
    ensure(&exists, "failing on a name that exists", exists.status == 1)?;
    let forced = run(&utility("ln"), &["-sf", "b", &dir.at("s")])?;
    ensure(&forced, "replacing it with -f", forced.status == 0)?;
    let target = std::fs::read_link(dir.at("s")).ok();
    holds("holding the new target", target == Some("b".into()))?;
    mkdirs(&dir.at("sub"))?;
    let relative = run(&utility("ln"), &["-sr", &a, &dir.at("sub")])?;
    ensure(&relative, "creating a relative link", relative.status == 0)?;
    let target = std::fs::read_link(dir.at("sub/a")).ok();
    holds("a relative target", target == Some("../a".into()))
}

/// A hard link to a missing target is named.
fn ln_names_what_is_missing() -> Result<(), Failure> {
    let dir = scratch("ln-missing")?;
    let missing = dir.at("nothing");
    let name = dir.at("l");
    let ln = run(&utility("ln"), &[&missing, &name])?;
    ensure(&ln, "failing", ln.status == 1)?;
    ensure(
        &ln,
        "naming the link and its target",
        ln.err
            .starts_with(&format!("ln: cannot create link {name} -> {missing}: ")),
    )?;
    holds("creating nothing", !dir.exists("l"))
}

/// No operand, `-r` without `-s` and `-t` with `-T` are refused with
/// status 2; several targets need a folder.
fn ln_bad_arguments() -> Result<(), Failure> {
    for arguments in [&[][..], &["-r", "a", "b"], &["-T", "-t", "x", "a"]] {
        let ln = run(&utility("ln"), arguments)?;
        ensure(&ln, "refusing the arguments", ln.status == 2)?;
        ensure(&ln, "naming the command", ln.err.starts_with("ln: "))?;
    }
    let dir = scratch("ln-folder")?;
    let a = dir.write("a", "1")?;
    let c = dir.at("c");
    let ln = run(&utility("ln"), &[&a, &a, &c])?;
    ensure(&ln, "failing without a folder", ln.status == 1)?;
    ensure(
        &ln,
        "naming it",
        ln.err == format!("ln: {c} is not a folder\n"),
    )
}
