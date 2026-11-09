//! `cargo xtask check`: formatting, lints and the commit rules.

use anyhow::{Context, Result, bail};
use std::process::Command;

/// Commits may change at most this many lines.
const MAX_LINES: u64 = 700;
/// Commits should change at most this many lines.
const SOFT_MAX_LINES: u64 = 500;

#[derive(clap::Args)]
pub struct Args {
    /// The branch the commits to check are based on.
    #[arg(long, default_value = "origin/main")]
    pub base: String,
}

fn cargo() -> Command {
    let mut command = Command::new(std::env::var("CARGO").unwrap_or_else(|_| "cargo".to_owned()));
    command.current_dir(crate::workspace_root());
    command
}

/// Cargo in the user workspace, whose own configuration Cargo reads from
/// the directory it runs in.
fn user_cargo() -> Command {
    let mut command = cargo();
    command.current_dir(crate::workspace_root().join("user"));
    command
}

/// Runs `command` and returns whether it succeeded.
fn passes(mut command: Command) -> Result<bool> {
    Ok(command.status().context("failed to run cargo")?.success())
}

/// Runs git with `args` in the workspace and returns its output.
fn git(args: &[&str]) -> Result<String> {
    let output = Command::new("git")
        .current_dir(crate::workspace_root())
        .args(args)
        .output()
        .context("failed to run git")?;
    if !output.status.success() {
        bail!(
            "`git {}` failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(String::from_utf8(output.stdout)?)
}

/// Whether `id` is a document ID such as `MIKSPEC-0012` or a requirement ID
/// such as `MIKSPEC-0012.R4`.
fn is_spec_id(id: &str) -> bool {
    let Some(rest) = id.strip_prefix("MIKSPEC-") else {
        return false;
    };
    let (document, requirement) = match rest.split_once(".R") {
        Some((document, requirement)) => (document, Some(requirement)),
        None => (rest, None),
    };
    let digits = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit());
    document.len() == 4
        && digits(document)
        && requirement.is_none_or(|r| digits(r) && !r.starts_with('0'))
}

/// Checks the values of a commit's `Spec` trailers, one per trailer.
fn check_trailers(values: &[&str]) -> Result<(), String> {
    if values.is_empty() {
        return Err("has no Spec: trailer".to_owned());
    }
    for value in values {
        for id in value.split(',').map(str::trim) {
            if !is_spec_id(id) {
                return Err(format!("has a malformed ID `{id}` in its Spec: trailer"));
            }
        }
    }
    Ok(())
}

/// The value of the `Tested` trailer, which the last commit of a range
/// carries once the whole of `cargo xtask test`, with its QEMU runs, passed
/// on its tree.
const TESTED: &str = "cargo xtask test";

/// Whether a commit that changes `paths` makes its range need the `Tested`
/// trailer on its last commit: every commit does unless it changes only
/// Markdown files and the specification submodule.
fn needs_tested(paths: &[&str]) -> bool {
    paths
        .iter()
        .any(|path| *path != "spec" && !path.ends_with(".md"))
}

/// Checks the values of a commit's `Tested` trailers.
fn check_tested(values: &[&str]) -> Result<(), String> {
    if values.iter().any(|value| value.trim() == TESTED) {
        Ok(())
    } else if values.is_empty() {
        Err(format!("has no `Tested: {TESTED}` trailer"))
    } else {
        Err(format!("has a Tested: trailer other than `{TESTED}`"))
    }
}

/// Whether a path holds a lock file, which does not count towards the size
/// of a commit.
fn is_lock_file(path: &str) -> bool {
    path.rsplit('/')
        .next()
        .is_some_and(|name| name.ends_with(".lock"))
}

/// The lines changed by a commit, from `git show --numstat` output, leaving
/// out lock files and the paths for which `generated` is true. Binary files
/// count as no lines.
fn changed_lines(numstat: &str, generated: &dyn Fn(&str) -> bool) -> u64 {
    numstat
        .lines()
        .filter_map(|line| {
            let mut fields = line.splitn(3, '\t');
            let added = fields.next()?.parse::<u64>().ok()?;
            let deleted = fields.next()?.parse::<u64>().ok()?;
            let path = fields.next()?;
            (!is_lock_file(path) && !generated(path)).then_some(added + deleted)
        })
        .sum()
}

/// The paths of a commit marked as generated with the `linguist-generated`
/// attribute.
fn generated_paths(commit: &str) -> Result<Vec<String>> {
    let paths = git(&["diff-tree", "--no-commit-id", "-r", "--name-only", commit])?;
    if paths.trim().is_empty() {
        return Ok(Vec::new());
    }
    let mut args = vec!["check-attr", "linguist-generated", "--"];
    args.extend(paths.lines());
    let attrs = git(&args)?;
    Ok(attrs
        .lines()
        .filter_map(|line| line.strip_suffix(": linguist-generated: set"))
        .chain(
            attrs
                .lines()
                .filter_map(|l| l.strip_suffix(": linguist-generated: true")),
        )
        .map(str::to_owned)
        .collect())
}

/// Checks the commits since `base`, returning a description of each problem:
/// each commit's `Spec` trailer and size, and the `Tested` trailer of the
/// last one, unless every commit of the range changes only Markdown files
/// and the specification submodule.
fn check_commits(base: &str) -> Result<Vec<String>> {
    let range = format!("{base}..HEAD");
    let commits = git(&["rev-list", "--no-merges", "--reverse", &range])?;
    let mut problems = Vec::new();
    let mut range_needs_tested = false;
    for commit in commits.lines() {
        let subject = git(&["log", "-1", "--format=%h %s", commit])?;
        let subject = subject.trim();
        let paths = git(&["diff-tree", "--no-commit-id", "-r", "--name-only", commit])?;
        let submodule_only = paths.lines().eq(["spec"]);

        let trailers = git(&[
            "log",
            "-1",
            "--format=%(trailers:key=Spec,valueonly,unfold)",
            commit,
        ])?;
        let values: Vec<&str> = trailers.lines().filter(|l| !l.trim().is_empty()).collect();
        if !(submodule_only && values.is_empty())
            && let Err(problem) = check_trailers(&values)
        {
            problems.push(format!("{subject}: {problem}"));
        }

        let changed: Vec<&str> = paths.lines().collect();
        range_needs_tested |= needs_tested(&changed);

        let generated = generated_paths(commit)?;
        let numstat = git(&["show", "--numstat", "--format=", commit])?;
        let lines = changed_lines(&numstat, &|path| generated.iter().any(|g| g == path));
        if lines > MAX_LINES {
            problems.push(format!(
                "{subject}: changes {lines} lines, more than {MAX_LINES}"
            ));
        } else if lines > SOFT_MAX_LINES {
            println!("warning  {subject}: changes {lines} lines, more than {SOFT_MAX_LINES}");
        }
    }
    if let Some(last) = commits.lines().last()
        && range_needs_tested
    {
        let subject = git(&["log", "-1", "--format=%h %s", last])?;
        let tested = git(&[
            "log",
            "-1",
            "--format=%(trailers:key=Tested,valueonly,unfold)",
            last,
        ])?;
        let values: Vec<&str> = tested.lines().filter(|l| !l.trim().is_empty()).collect();
        if let Err(problem) = check_tested(&values) {
            problems.push(format!("{}: {problem}", subject.trim()));
        }
    }
    Ok(problems)
}

pub fn run(args: &Args) -> Result<()> {
    let mut problems = Vec::new();

    let mut fmt = cargo();
    fmt.args(["fmt", "--all", "--check"]);
    if !passes(fmt)? {
        problems.push("the code is not formatted with rustfmt".to_owned());
    }

    // A module with a folder of its own must be `mod.rs` in that folder,
    // never a file of the same name beside it.
    const MODULE_LAYOUT: [&str; 2] = ["-D", "clippy::self_named_module_files"];

    // The UEFI code of the boot manager and the bootloader is only compiled
    // for the UEFI target, and the kernel's for its own target.
    let clippy_runs: [&[&str]; 3] = [
        &[
            "clippy",
            "--workspace",
            "--all-targets",
            "--",
            "-D",
            "warnings",
        ],
        &[
            "clippy",
            "--package",
            "bootmenu",
            "--package",
            "mikrosboot",
            "--target",
            "x86_64-unknown-uefi",
            "--",
            "-D",
            "warnings",
        ],
        &[
            "clippy",
            "--package",
            "kernel-x86_64",
            "--target",
            crate::build::KERNEL_TARGET,
            "--",
            "-D",
            "warnings",
        ],
    ];
    for clippy_args in clippy_runs {
        let mut clippy = cargo();
        clippy.args(clippy_args).args(MODULE_LAYOUT);
        if !passes(clippy)? {
            problems.push(format!(
                "`cargo {}` reported warnings",
                clippy_args.join(" ")
            ));
        }
    }

    let mut fmt = user_cargo();
    fmt.args(["fmt", "--all", "--check"]);
    if !passes(fmt)? {
        problems.push("the user workspace is not formatted with rustfmt".to_owned());
    }

    // User programs are only compiled for the user target. The test init
    // has code that only fuzz builds compile.
    let target = crate::workspace_root().join(crate::build::USER_TARGET);
    let target = target.to_str().context("the workspace path is not UTF-8")?;
    let user_clippy_runs: [&[&str]; 2] = [
        &["clippy", "--workspace", "--target", target],
        &[
            "clippy",
            "--package",
            "test-init",
            "--features",
            "fuzz",
            "--target",
            target,
        ],
    ];
    for clippy_args in user_clippy_runs {
        let mut clippy = user_cargo();
        clippy
            .args(clippy_args)
            .args(["--", "-D", "warnings"])
            .args(MODULE_LAYOUT);
        if !passes(clippy)? {
            problems.push(format!(
                "`cargo {}` in the user workspace reported warnings",
                clippy_args.join(" ")
            ));
        }
    }

    problems.extend(check_commits(&args.base)?);

    if problems.is_empty() {
        println!("ok       all checks passed");
        return Ok(());
    }
    for problem in &problems {
        println!("failed   {problem}");
    }
    bail!("{} check(s) failed", problems.len())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_document_and_requirement_ids() {
        assert!(is_spec_id("MIKSPEC-0012"));
        assert!(is_spec_id("MIKSPEC-0012.R4"));
        assert!(is_spec_id("MIKSPEC-0073.R21"));
        for bad in [
            "MIKSPEC-12",
            "MIKSPEC-0012.R",
            "MIKSPEC-0012.R04",
            "mikspec-0012",
            "MIKSPEC-0012R4",
            "0012",
        ] {
            assert!(!is_spec_id(bad), "{bad}");
        }
    }

    #[test]
    fn checks_trailer_values() {
        assert_eq!(
            check_trailers(&["MIKSPEC-0023.R7, MIKSPEC-0045.R5"]),
            Ok(())
        );
        assert_eq!(check_trailers(&["MIKSPEC-0023.R7", "MIKSPEC-0004"]), Ok(()));
        assert!(check_trailers(&[]).unwrap_err().contains("no Spec"));
        assert!(check_trailers(&["MIKSPEC-0023.R7 MIKSPEC-0045.R5"]).is_err());
        assert!(check_trailers(&["none"]).is_err());
    }

    #[test]
    fn needs_the_tested_trailer_unless_only_markdown_and_spec_change() {
        assert!(!needs_tested(&["spec"]));
        assert!(!needs_tested(&["README.md", "docs/build.md", "spec"]));
        assert!(!needs_tested(&[]));
        assert!(needs_tested(&["README.md", "xtask/src/check.rs"]));
        assert!(needs_tested(&[".github/workflows/ci.yml"]));
    }

    #[test]
    fn checks_tested_trailer_values() {
        assert_eq!(check_tested(&["cargo xtask test"]), Ok(()));
        assert_eq!(check_tested(&[" cargo xtask test "]), Ok(()));
        assert!(check_tested(&[]).unwrap_err().contains("no `Tested"));
        assert!(check_tested(&["cargo xtask test --host"]).is_err());
        assert!(check_tested(&["yes"]).is_err());
    }

    #[test]
    fn counts_lines_without_lock_binary_and_generated_files() {
        let numstat = "10\t5\tsrc/a.rs\n300\t200\tCargo.lock\n-\t-\tfonts/x.ttf\n7\t0\tgen/out.rs\n1\t1\tspec\n";
        assert_eq!(changed_lines(numstat, &|p| p == "gen/out.rs"), 17);
        assert_eq!(changed_lines(numstat, &|_| false), 24);
    }
}
