//! stat: the facts of files, folders and links, in lines, formats and
//! JSON, and the paths it cannot reach.

use crate::{Check, Failure, ensure, holds, run, scratch, utility};

pub const CHECKS: &[(&str, Check)] = &[
    ("stat_shows_a_file", stat_shows_a_file),
    ("stat_formats_and_json", stat_formats_and_json),
    ("stat_folders_and_links", stat_folders_and_links),
    ("stat_names_what_is_missing", stat_names_what_is_missing),
];

/// A file's lines show its name, type, size, what the caller may do,
/// its stored mode and its access control list.
fn stat_shows_a_file() -> Result<(), Failure> {
    let dir = scratch("stat")?;
    let file = dir.write("a", b"12345")?;
    let run = run(&utility("stat"), &[&file])?;
    ensure(&run, "succeeding", run.status == 0 && run.err.is_empty())?;
    let has = |line: &str| run.out.lines().any(|l| l == line);
    ensure(&run, "the name", has(&format!("    Name: {file}")))?;
    ensure(&run, "the type", has("    Type: file"))?;
    ensure(&run, "the size", has("    Size: 5"))?;
    let starts = |start: &str| run.out.lines().any(|l| l.starts_with(start));
    ensure(&run, "what the caller may do", starts("  Access: rw"))?;
    ensure(&run, "the owner", starts("   Owner: "))?;
    ensure(&run, "the stored mode", starts("    Mode: 0"))?;
    ensure(&run, "the modified time", starts("Modified: 20"))?;
    ensure(&run, "the access control list", starts("     ACL: "))?;
    ensure(&run, "the owner's entry", run.out.contains("owner:rw"))?;
    ensure(&run, "no default list for a file", !starts(" Default:"))
}

/// `-c` writes only the directives asked for, and `--json` an array
/// with an object per path.
fn stat_formats_and_json() -> Result<(), Failure> {
    let dir = scratch("stat-format")?;
    let file = dir.write("a", b"12345")?;
    let format = run(&utility("stat"), &["-c", "%n %s %F %h %a", &file])?;
    ensure(&format, "succeeding with -c", format.status == 0)?;
    let expected_start = format!("{file} 5 file 1 rw");
    ensure(
        &format,
        "the directives",
        format.out.starts_with(&expected_start) && format.out.lines().count() == 1,
    )?;
    let json = run(&utility("stat"), &["--json", &file, &file])?;
    ensure(&json, "succeeding with --json", json.status == 0)?;
    ensure(
        &json,
        "an object per path",
        json.out.starts_with("[{\"name\":")
            && json.out.trim_end().ends_with("}]")
            && json.out.matches("\"type\":\"file\",\"size\":5,").count() == 2,
    )?;
    ensure(
        &json,
        "the stored mode and the lists",
        json.out.contains("\"stored_mode\":\"0")
            && json.out.contains("\"acl\":[\"")
            && json.out.contains("\"owner:rw"),
    )
}

/// A folder shows its default list, and a link its target, or with -L
/// what it leads to.
fn stat_folders_and_links() -> Result<(), Failure> {
    let dir = scratch("stat-kinds")?;
    let folder = dir.folder("f")?;
    let file = dir.write("a", b"12345")?;
    let shown = run(&utility("stat"), &[&folder])?;
    ensure(
        &shown,
        "a folder",
        shown.status == 0 && shown.out.contains("    Type: folder\n"),
    )?;
    ensure(
        &shown,
        "its default list",
        shown.out.contains("\n Default: "),
    )?;
    let link = dir.at("l");
    holds(
        "making a link",
        std::os::unix::fs::symlink(&file, &link).is_ok(),
    )?;
    let plain = run(&utility("stat"), &[&link])?;
    ensure(&plain, "succeeding with a link", plain.status == 0)?;
    ensure(
        &plain,
        "the link and its target",
        plain.out.contains("    Type: link\n") && plain.out.contains(&format!(" -> {file}\n")),
    )?;
    let followed = run(&utility("stat"), &["-L", &link])?;
    ensure(
        &followed,
        "the file behind the link",
        followed.status == 0 && followed.out.contains("    Type: file\n"),
    )
}

/// A path that is not there is named, the others are still shown, stat
/// exits with 1, and without paths with 2.
fn stat_names_what_is_missing() -> Result<(), Failure> {
    let dir = scratch("stat-missing")?;
    let file = dir.write("a", b"")?;
    let missing = dir.at("nothing");
    let run_ = run(&utility("stat"), &[&missing, &file])?;
    ensure(&run_, "failing", run_.status == 1)?;
    ensure(
        &run_,
        "naming the missing path",
        run_.err.starts_with("stat: cannot stat ") && run_.err.contains("nothing"),
    )?;
    ensure(
        &run_,
        "still showing the file",
        run_.out.contains(&format!("    Name: {file}\n")),
    )?;
    let bare = run(&utility("stat"), &[])?;
    ensure(&bare, "refusing no paths", bare.status == 2)
}
