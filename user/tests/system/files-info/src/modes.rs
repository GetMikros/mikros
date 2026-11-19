//! chmod: octal and symbolic modes, the note on bits other than the
//! execute bits, `-v`, `-R` and `--reference`, and what it cannot change.

use crate::{Check, Failure, ensure, holds, run, scratch, utility};
use std::os::unix::fs::PermissionsExt;

pub const CHECKS: &[(&str, Check)] = &[
    ("chmod_sets_modes", chmod_sets_modes),
    ("chmod_notes_other_bits", chmod_notes_other_bits),
    (
        "chmod_recursive_and_reference",
        chmod_recursive_and_reference,
    ),
    ("chmod_names_what_is_missing", chmod_names_what_is_missing),
];

/// The permission bits of `path`'s stored mode, or `None`.
fn mode(path: &str) -> Option<u32> {
    std::fs::metadata(path)
        .ok()
        .map(|m| m.permissions().mode() & 0o7777)
}

/// An octal mode and a symbolic one that change execute bits set the
/// whole mode, without a note.
fn chmod_sets_modes() -> Result<(), Failure> {
    let dir = scratch("chmod")?;
    let file = dir.write("a", b"")?;
    let octal = run(&utility("chmod"), &["640", &file])?;
    ensure(&octal, "succeeding", octal.status == 0)?;
    holds("the octal mode", mode(&file) == Some(0o640))?;
    let symbolic = run(&utility("chmod"), &["u+x,g=u", &file])?;
    ensure(
        &symbolic,
        "succeeding without a note",
        symbolic.status == 0 && symbolic.err.is_empty() && symbolic.out.is_empty(),
    )?;
    holds("the symbolic mode", mode(&file) == Some(0o770))
}

/// A change that leaves the execute bits as they were is noted once,
/// and `-v` names each file and its modes.
fn chmod_notes_other_bits() -> Result<(), Failure> {
    let dir = scratch("chmod-note")?;
    let a = dir.write("a", b"")?;
    let b = dir.write("b", b"")?;
    for path in [&a, &b] {
        holds(
            "setting a mode",
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).is_ok(),
        )?;
    }
    let run = run(&utility("chmod"), &["-v", "a-w", &a, &b])?;
    ensure(&run, "succeeding", run.status == 0)?;
    holds(
        "the new modes",
        mode(&a) == Some(0o400) && mode(&b) == Some(0o400),
    )?;
    ensure(
        &run,
        "naming each change",
        run.out
            == format!(
                "mode of {a} changed from 0600 to 0400\nmode of {b} changed from 0600 to 0400\n"
            ),
    )?;
    ensure(
        &run,
        "noting once that only execute bits count",
        run.err.matches("execute bits").count() == 1 && run.err.starts_with("chmod: "),
    )
}

/// `-R` changes a folder and what it holds, and `--reference` takes the
/// mode of another file.
fn chmod_recursive_and_reference() -> Result<(), Failure> {
    let dir = scratch("chmod-recursive")?;
    let folder = dir.folder("f")?;
    let inner = dir.write("f/inner", b"")?;
    let recursive = run(&utility("chmod"), &["-R", "700", &folder])?;
    ensure(&recursive, "succeeding with -R", recursive.status == 0)?;
    holds(
        "the folder and its file changed",
        mode(&folder) == Some(0o700) && mode(&inner) == Some(0o700),
    )?;
    let other = dir.write("other", b"")?;
    let reference = run(&utility("chmod"), &["--reference", &inner, &other])?;
    ensure(
        &reference,
        "succeeding with --reference",
        reference.status == 0,
    )?;
    holds("the reference's mode", mode(&other) == Some(0o700))
}

/// A path that is not there is named and the others still change; a
/// mode that is none is refused with 2.
fn chmod_names_what_is_missing() -> Result<(), Failure> {
    let dir = scratch("chmod-missing")?;
    let file = dir.write("a", b"")?;
    let missing = dir.at("nothing");
    let run_ = run(&utility("chmod"), &["755", &missing, &file])?;
    ensure(&run_, "failing", run_.status == 1)?;
    ensure(
        &run_,
        "naming the missing path",
        run_.err.starts_with("chmod: cannot access ") && run_.err.contains("nothing"),
    )?;
    holds("still changing the file", mode(&file) == Some(0o755))?;
    let bad = run(&utility("chmod"), &["u+q", &file])?;
    ensure(
        &bad,
        "refusing a bad mode",
        bad.status == 2 && bad.err.contains("invalid mode u+q"),
    )
}
