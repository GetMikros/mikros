//! The checks of find, each in a tree of its own in the folder of the
//! checks.

use crate::{Failure, Folder, Run, ensure, failed, refused, run, wrote};
use std::fs;
use std::path::Path;

const FIND: &str = "/system/bin/find";
const BASENAME: &str = "/system/utilities/basename";

/// A tree `name` in `dir`: `a.txt` of five bytes, `b.log` empty,
/// `sub/c.txt` of 2000 bytes, an empty folder `sub/empty` and a link
/// `l` to `a.txt`; and its path.
fn tree(dir: &Folder, name: &str) -> Result<String, Failure> {
    let root = dir.path.join(name);
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(root.join("sub/empty")).map_err(|_| failed("making the tree"))?;
    fs::write(root.join("a.txt"), b"12345").map_err(|_| failed("writing a.txt"))?;
    fs::write(root.join("b.log"), b"").map_err(|_| failed("writing b.log"))?;
    fs::write(root.join("sub/c.txt"), [b'c'; 2000]).map_err(|_| failed("writing c.txt"))?;
    std::os::unix::fs::symlink("a.txt", root.join("l")).map_err(|_| failed("making a link"))?;
    Ok(root.to_string_lossy().into_owned())
}

/// The lines that `run` wrote, each with `root` taken off its start.
fn lines(run: &Run, root: &str) -> Vec<String> {
    run.out
        .lines()
        .map(|l| l.strip_prefix(root).unwrap_or(l).to_string())
        .collect()
}

fn found(run: &Run, root: &str, what: &'static str, wanted: &[&str]) -> Result<(), Failure> {
    let ok = run.status == 0 && run.err.is_empty() && lines(run, root) == wanted;
    ensure(run, what, ok)
}

pub fn names_and_types(dir: &Folder) -> Result<(), Failure> {
    let root = tree(dir, "find-names")?;
    let all = run(FIND, &[&root])?;
    let every = [
        "",
        "/a.txt",
        "/b.log",
        "/l",
        "/sub",
        "/sub/c.txt",
        "/sub/empty",
    ];
    found(&all, &root, "every entry, in the order of names", &every)?;
    let run_ = run(FIND, &[&root, "-name", "*.txt"])?;
    found(&run_, &root, "-name", &["/a.txt", "/sub/c.txt"])?;
    let run_ = run(FIND, &[&root, "-iname", "*.TXT", "-path", "*/sub/*"])?;
    found(&run_, &root, "-iname and -path", &["/sub/c.txt"])?;
    let run_ = run(FIND, &[&root, "-type", "d"])?;
    found(&run_, &root, "-type d", &["", "/sub", "/sub/empty"])?;
    let run_ = run(FIND, &[&root, "-type", "l"])?;
    found(&run_, &root, "-type l without following", &["/l"])?;
    let run_ = run(FIND, &["-L", &root, "-type", "f", "-name", "l"])?;
    found(&run_, &root, "-type f through the link with -L", &["/l"])?;
    let run_ = run(
        FIND,
        &[
            &root,
            "-mindepth",
            "1",
            "-maxdepth",
            "1",
            "(",
            "-type",
            "d",
            "-o",
            "-name",
            "b*",
            ")",
        ],
    )?;
    found(
        &run_,
        &root,
        "depths, -o and parentheses",
        &["/b.log", "/sub"],
    )?;
    let run_ = run(
        FIND,
        &[
            &root, "!", "-type", "d", "-not", "-name", "*.txt", "-print0",
        ],
    )?;
    let wanted = format!("{root}/b.log\0{root}/l\0");
    wrote(&run_, "! -not and -print0", &wanted)
}

pub fn sizes_and_empty(dir: &Folder) -> Result<(), Failure> {
    let root = tree(dir, "find-sizes")?;
    let run_ = run(FIND, &[&root, "-type", "f", "-size", "+1K"])?;
    found(&run_, &root, "-size +1K", &["/sub/c.txt"])?;
    let run_ = run(FIND, &[&root, "-type", "f", "-size", "5"])?;
    found(&run_, &root, "-size in bytes", &["/a.txt"])?;
    let run_ = run(FIND, &[&root, "-empty"])?;
    found(&run_, &root, "-empty", &["/b.log", "/sub/empty"])?;
    let run_ = run(FIND, &[&root, "-type", "f", "-mmin", "-60", "-mtime", "0"])?;
    found(
        &run_,
        &root,
        "-mmin and -mtime",
        &["/a.txt", "/b.log", "/sub/c.txt"],
    )?;
    let run_ = run(
        FIND,
        &[
            &root,
            "-newer",
            &format!("{root}/sub/c.txt"),
            "-name",
            "*.zzz",
        ],
    )?;
    found(&run_, &root, "-newer", &[])
}

pub fn exec(dir: &Folder) -> Result<(), Failure> {
    let root = tree(dir, "find-exec")?;
    let each = run(
        FIND,
        &[&root, "-name", "*.txt", "-exec", BASENAME, "{}", ";"],
    )?;
    wrote(&each, "-exec ... ; once per entry", "a.txt\nc.txt\n")?;
    let batch = run(
        FIND,
        &[&root, "-name", "*.txt", "-exec", BASENAME, "-a", "{}", "+"],
    )?;
    wrote(&batch, "-exec ... {} + with every entry", "a.txt\nc.txt\n")?;
    let false_ = run(
        FIND,
        &[
            &root, "-name", "a.txt", "-exec", BASENAME, ";", "-o", "-name", "a.txt", "-print",
        ],
    )?;
    let ok = false_.status == 0 && lines(&false_, &root) == ["/a.txt"];
    ensure(&false_, "a failing -exec is false", ok)?;
    let missing = run(
        FIND,
        &[
            &root,
            "-name",
            "a.txt",
            "-exec",
            "/system/bin/no-such-command",
            "{}",
            ";",
        ],
    )?;
    refused(
        &missing,
        "a command that does not start",
        1,
        "find: cannot run ",
        "no-such-command",
    )
}

pub fn json(dir: &Folder) -> Result<(), Failure> {
    let root = tree(dir, "find-json")?;
    let run_ = run(FIND, &[&root, "-name", "a.txt", "--json"])?;
    let start =
        format!("[{{\"path\":\"{root}/a.txt\",\"type\":\"file\",\"size\":5,\"modified\":\"");
    let ok = run_.status == 0 && run_.out.starts_with(&start) && run_.out.ends_with("Z\"}]\n");
    ensure(&run_, "an array of one object", ok)?;
    let run_ = run(FIND, &["--json", &root, "-name", "*.zzz"])?;
    wrote(&run_, "an empty array", "[]\n")?;
    let run_ = run(FIND, &[&root, "-maxdepth", "1", "-type", "l", "--json"])?;
    let ok = run_
        .out
        .starts_with(&format!("[{{\"path\":\"{root}/l\",\"type\":\"link\","));
    ensure(&run_, "a link as a link", run_.status == 0 && ok)
}

pub fn delete(dir: &Folder) -> Result<(), Failure> {
    let root = tree(dir, "find-delete")?;
    let run_ = run(FIND, &[&root, "-name", "*.txt", "-delete"])?;
    wrote(&run_, "deleting quietly", "")?;
    let gone = !Path::new(&format!("{root}/a.txt")).exists()
        && !Path::new(&format!("{root}/sub/c.txt")).exists();
    ensure(&run_, "the .txt files gone", gone)?;
    let kept = Path::new(&format!("{root}/b.log")).exists();
    ensure(&run_, "the others kept", kept)?;
    let run_ = run(FIND, &[&root, "-path", "*/sub*", "-delete"])?;
    let gone = !Path::new(&format!("{root}/sub")).exists();
    ensure(
        &run_,
        "a folder after its entries",
        run_.status == 0 && gone,
    )?;
    let run_ = run(FIND, &[&root, "-name", "x", "-o", "-delete"])?;
    let gone = !Path::new(&root).exists();
    ensure(&run_, "the whole tree", run_.status == 0 && gone)
}

/// A folder that cannot be read is named and the walk goes on: here
/// `gone`, which `-exec` removes after find reached it and before find
/// reads it, as a folder that the caller may not read is hard to make
/// in a test.
pub fn unreadable_folder(dir: &Folder) -> Result<(), Failure> {
    let root = tree(dir, "find-unreadable")?;
    fs::create_dir_all(format!("{root}/gone/x")).map_err(|_| failed("making gone"))?;
    let arguments = [
        root.as_str(),
        "-name",
        "gone",
        "-exec",
        FIND,
        "{}",
        "-delete",
        ";",
        "-o",
        "-name",
        "*.txt",
        "-print",
    ];
    let run_ = run(FIND, &arguments)?;
    refused(
        &run_,
        "naming the folder",
        1,
        "find: cannot read folder ",
        "gone",
    )?;
    let after = lines(&run_, &root);
    ensure(&run_, "going on", after == ["/a.txt", "/sub/c.txt"])?;
    let run_ = run(
        FIND,
        &[&format!("{root}/nothing"), &format!("{root}/b.log")],
    )?;
    refused(
        &run_,
        "naming a path that is not there",
        1,
        "find: cannot access ",
        "nothing",
    )?;
    ensure(
        &run_,
        "still walking the other",
        lines(&run_, &root) == ["/b.log"],
    )
}

pub fn bad_arguments(dir: &Folder) -> Result<(), Failure> {
    let root = dir.at("");
    for (arguments, what, holds) in [
        (
            &["-nope"][..],
            "an unknown primary",
            "unknown primary -nope",
        ),
        (&["-type", "x"][..], "an unknown type", "-type"),
        (
            &["-exec", "ls"][..],
            "an -exec without its end",
            "-exec needs",
        ),
        (&["(", "-print"][..], "a ( without )", "( has no )"),
        (
            &["-print", "x"][..],
            "a path after the expression",
            "paths must come before",
        ),
        (
            &["--nope"][..],
            "an unknown option",
            "unknown option --nope",
        ),
    ] {
        let mut all = vec![root.as_str()];
        all.extend_from_slice(arguments);
        refused(&run(FIND, &all)?, what, 2, "find: ", holds)?;
    }
    let help = run(FIND, &["--help"])?;
    let ok = help.status == 0 && help.out.contains("-name <pattern>");
    ensure(&help, "help with the primaries", ok)
}
