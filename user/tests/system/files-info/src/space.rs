//! du and df: the space of folders and files, counted once per inode,
//! and the volumes that hold them.

use crate::{Check, Failure, ensure, run, scratch, utility};

pub const CHECKS: &[(&str, Check)] = &[
    ("du_measures_folders", du_measures_folders),
    ("du_counts_links_once", du_counts_links_once),
    ("du_names_what_is_missing", du_names_what_is_missing),
    ("df_shows_volumes", df_shows_volumes),
    ("df_names_what_is_missing", df_names_what_is_missing),
];

/// `-a` shows files with the folders, after what they hold, `-s` only
/// the total, `-c` a grand total, and `--json` an object per path.
fn du_measures_folders() -> Result<(), Failure> {
    let dir = scratch("du")?;
    let top = dir.path();
    dir.folder("sub")?;
    dir.write("sub/f", &[7; 5000])?;
    let all = run(&utility("du"), &["-a", "--apparent-size", &top])?;
    ensure(&all, "succeeding", all.status == 0 && all.err.is_empty())?;
    let lines: Vec<&str> = all.out.lines().collect();
    ensure(&all, "the file, its folder and the top", lines.len() == 3)?;
    ensure(
        &all,
        "the file in units of 1024 bytes",
        lines[0] == format!("5\t{top}/sub/f"),
    )?;
    ensure(
        &all,
        "the folders after what they hold",
        lines[1].ends_with(&format!("\t{top}/sub")) && lines[2].ends_with(&format!("\t{top}")),
    )?;
    let summary = run(&utility("du"), &["-s", "-c", &top])?;
    let lines: Vec<&str> = summary.out.lines().collect();
    ensure(
        &summary,
        "a total and a grand total",
        summary.status == 0
            && lines.len() == 2
            && lines[0].ends_with(&format!("\t{top}"))
            && lines[1].ends_with("\ttotal"),
    )?;
    let json = run(&utility("du"), &["--json", "-s", "--apparent-size", &top])?;
    ensure(
        &json,
        "an object for the path",
        json.status == 0
            && json
                .out
                .starts_with(&format!("[{{\"path\":\"{top}\",\"size\":"))
            && json.out.trim_end().ends_with("}]"),
    )
}

/// A file with two names is counted under one of them only.
fn du_counts_links_once() -> Result<(), Failure> {
    let dir = scratch("du-links")?;
    let file = dir.write("f", &[1; 3000])?;
    if std::fs::hard_link(&file, dir.at("g")).is_err() {
        // A file system without hard links has nothing to count twice.
        return Ok(());
    }
    let run = run(&utility("du"), &["-a", "--apparent-size", &dir.path()])?;
    ensure(&run, "succeeding", run.status == 0)?;
    let named = run
        .out
        .lines()
        .filter(|l| l.ends_with("/f") || l.ends_with("/g"))
        .count();
    ensure(&run, "one of the names", named == 1)
}

/// A path that is not there is named and du exits with 1; `-s` with
/// `-a` is refused with 2.
fn du_names_what_is_missing() -> Result<(), Failure> {
    let dir = scratch("du-missing")?;
    let missing = dir.at("nothing");
    let run_ = run(&utility("du"), &[&missing, &dir.path()])?;
    ensure(&run_, "failing", run_.status == 1)?;
    ensure(
        &run_,
        "naming the missing path",
        run_.err.starts_with("du: cannot access ") && run_.err.contains("nothing"),
    )?;
    ensure(
        &run_,
        "still measuring the folder",
        run_.out.lines().count() == 1,
    )?;
    let both = run(&utility("du"), &["-s", "-a", &dir.path()])?;
    ensure(&both, "refusing -s with -a", both.status == 2)
}

/// df shows its header, the volume of a folder, writable, and `--json`
/// an object for it.
fn df_shows_volumes() -> Result<(), Failure> {
    let dir = scratch("df")?;
    let all = run(&utility("df"), &[])?;
    ensure(
        &all,
        "succeeding without paths",
        all.status == 0 && all.out.starts_with("PATH "),
    )?;
    let one = run(&utility("df"), &["-h", "-T", &dir.path()])?;
    ensure(&one, "succeeding with a path", one.status == 0)?;
    let lines: Vec<&str> = one.out.lines().collect();
    ensure(
        &one,
        "the header and the volume",
        lines.len() == 2
            && lines[0].starts_with("PATH ")
            && lines[0].contains(" SIZE ")
            && lines[0].ends_with(" READ-ONLY")
            && lines[1].starts_with('/')
            && lines[1].ends_with(" no"),
    )?;
    let json = run(&utility("df"), &["--json", &dir.path()])?;
    ensure(
        &json,
        "an object for the volume",
        json.status == 0
            && json.out.starts_with("[{\"path\":\"/")
            && json.out.contains("\"read_only\":false}]"),
    )
}

/// A path that is not there is named and df exits with 1.
fn df_names_what_is_missing() -> Result<(), Failure> {
    let dir = scratch("df-missing")?;
    let missing = dir.at("nothing");
    let run_ = run(&utility("df"), &[&missing])?;
    ensure(
        &run_,
        "failing and naming the path",
        run_.status == 1 && run_.err.starts_with(&format!("df: {missing}: ")),
    )?;
    let bad = run(&utility("df"), &["-x"])?;
    ensure(&bad, "refusing an unknown option", bad.status == 2)
}
