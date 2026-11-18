//! What readlink takes, and how -f, -e and -m make paths absolute: the
//! part of a path that exists is resolved by the C library's
//! `realpath`, through `std::fs::canonicalize`, and what follows it
//! lexically (MIKSPEC-0098.R41).

use mikros_cli::{Kind, Operand, Opt, Run, Spec};
use std::io;
use std::path::{Component, Path, PathBuf};

pub static COMMAND: Spec = Spec {
    operands: &[Operand::some("path", Kind::Path, "the links to read")],
    options: &[
        Opt::both(
            "canonicalize",
            'f',
            "write the path resolved; all but the last component must exist",
        ),
        Opt::both(
            "canonicalize-existing",
            'e',
            "write the path resolved; every component must exist",
        ),
        Opt::both(
            "canonicalize-missing",
            'm',
            "write the path resolved; any component may be missing",
        ),
        Opt::both("no-newline", 'n', "write no newline after a single target"),
        Opt::both("zero", 'z', "end each target with NUL, not a newline"),
    ],
    ..Spec::new(
        "readlink",
        "Writes the target of each link, or with -f, -e or -m each path made\n\
         absolute with its links, . and .. resolved.",
    )
};

/// How a path is written.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    /// The target of the link, without -f, -e or -m.
    Target,
    /// Resolved, every component existing, with -e.
    Existing,
    /// Resolved, all but the last component existing, with -f.
    Last,
    /// Resolved, any component missing, with -m.
    Missing,
}

/// The mode of `run`, in which the last of `-f`, `-e` and `-m` wins.
pub fn mode(run: &Run) -> Mode {
    match run.last_of(&["-f", "-e", "-m"]) {
        Some("-f") => Mode::Last,
        Some("-e") => Mode::Existing,
        Some("-m") => Mode::Missing,
        _ => Mode::Target,
    }
}

/// `rest` after `base`, with `.` left out and `..` going up, never
/// above `/`.
fn append(mut base: PathBuf, rest: &[Component]) -> PathBuf {
    for part in rest {
        match part {
            Component::Normal(name) => base.push(name),
            Component::ParentDir => {
                base.pop();
            }
            Component::CurDir | Component::RootDir | Component::Prefix(_) => {}
        }
    }
    base
}

/// `path` made absolute against `current` with its links resolved: its
/// longest leading part that exists, resolved, and the rest lexically,
/// when `mode` lets the rest be missing.
pub fn resolve(path: &Path, current: &Path, mode: Mode) -> io::Result<PathBuf> {
    let absolute = current.join(path);
    let first = match std::fs::canonicalize(&absolute) {
        Ok(resolved) => return Ok(resolved),
        Err(e) => e,
    };
    if matches!(mode, Mode::Existing | Mode::Target) {
        return Err(first);
    }
    let parts: Vec<Component> = absolute.components().collect();
    for k in (1..parts.len()).rev() {
        let lead: PathBuf = parts[..k].iter().collect();
        let Ok(base) = std::fs::canonicalize(&lead) else {
            continue;
        };
        let rest = &parts[k..];
        let missing_last = rest.len() == 1 && first.kind() == io::ErrorKind::NotFound;
        return match mode == Mode::Missing || missing_last {
            true => Ok(append(base, rest)),
            false => Err(first),
        };
    }
    match mode {
        Mode::Missing => Ok(append(PathBuf::from("/"), &parts)),
        _ => Err(first),
    }
}

/// What ends each path written: NUL with `-z`, nothing with `-n` and a
/// single path, and otherwise a newline; and whether `-n` is ignored
/// for several paths.
pub fn ending(zero: bool, no_newline: bool, paths: usize) -> (&'static str, bool) {
    match (zero, no_newline && paths == 1) {
        (true, _) => ("\0", false),
        (false, true) => ("", false),
        (false, false) => ("\n", no_newline),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn strings(arguments: &[&str]) -> Vec<String> {
        arguments.iter().map(|a| a.to_string()).collect()
    }

    fn run(arguments: &[&str]) -> mikros_cli::Run {
        match COMMAND.parse(arguments.iter().map(|a| a.to_string())) {
            mikros_cli::Parsed::Run(run) => run,
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn the_last_of_f_e_and_m_wins() {
        assert_eq!(mode(&run(&["x"])), Mode::Target);
        assert_eq!(mode(&run(&["-f", "x"])), Mode::Last);
        assert_eq!(mode(&run(&["-nfe", "x"])), Mode::Existing);
        assert_eq!(
            mode(&run(&["-e", "x", "--canonicalize-missing"])),
            Mode::Missing
        );
    }

    #[test]
    fn ends_targets_as_asked() {
        assert_eq!(ending(false, false, 2), ("\n", false));
        assert_eq!(ending(false, true, 1), ("", false));
        assert_eq!(ending(false, true, 2), ("\n", true));
        assert_eq!(ending(true, true, 2), ("\0", false));
    }

    #[test]
    fn resolves_links_and_what_may_be_missing() {
        let dir = std::env::temp_dir().join(format!("readlink-test-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join("sub")).unwrap();
        fs::write(dir.join("sub/file"), "").unwrap();
        std::os::unix::fs::symlink("sub/file", dir.join("link")).unwrap();
        let dir = fs::canonicalize(&dir).unwrap();
        let at = |p: &str| dir.join(p);
        let resolve = |p: &str, mode| resolve(Path::new(p), &dir, mode);
        assert_eq!(resolve("link", Mode::Existing).unwrap(), at("sub/file"));
        assert_eq!(resolve("sub/new", Mode::Last).unwrap(), at("sub/new"));
        assert!(resolve("sub/new", Mode::Existing).is_err());
        assert!(resolve("no/new", Mode::Last).is_err());
        assert!(resolve("link/x", Mode::Last).is_err());
        assert_eq!(
            resolve("no/../sub/./x", Mode::Missing).unwrap(),
            at("sub/x")
        );
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn declares_what_it_takes() {
        let file = concat!(env!("CARGO_MANIFEST_DIR"), "/readlink.msh");
        mikros_cli::declared(&COMMAND, std::path::Path::new(file)).unwrap();
    }
}
