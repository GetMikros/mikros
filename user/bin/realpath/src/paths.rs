//! What realpath takes, and how it makes paths absolute: the part of a
//! path that exists is resolved by the C library's `realpath`, through
//! `std::fs::canonicalize`, and what follows it lexically
//! (MIKSPEC-0098.R41).

use mikros_cli::{Kind, Operand, Opt, Run, Spec};
use std::io;
use std::path::{Component, Path, PathBuf};

pub static COMMAND: Spec = Spec {
    operands: &[Operand::some("path", Kind::Path, "the paths to resolve")],
    options: &[
        Opt::both(
            "canonicalize-existing",
            'e',
            "fail unless every component exists",
        ),
        Opt::both("canonicalize-missing", 'm', "let any component be missing"),
        Opt::both("strip", 's', "do not resolve links, only . and .."),
        Opt::both("zero", 'z', "end each path with NUL, not a newline"),
        Opt::long("relative-to", "write the paths relative to this folder")
            .takes("folder", Kind::Folder),
    ],
    ..Spec::new(
        "realpath",
        "Writes each path made absolute, with its links, . and .. resolved.\n\
         All but the last component must exist, unless -e or -m says otherwise.",
    )
};

/// Which components of a path must exist.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    /// Every one, with `-e`.
    Existing,
    /// All but the last, without `-e` or `-m`.
    Last,
    /// None, with `-m`.
    Missing,
}

/// The mode of `run`, in which the last of `-e` and `-m` wins.
pub fn mode(run: &Run) -> Mode {
    match run.last_of(&["-e", "-m"]) {
        Some("-e") => Mode::Existing,
        Some("-m") => Mode::Missing,
        _ => Mode::Last,
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

/// `path` made absolute against `current`, with `.`, `..` and repeated
/// slashes resolved without looking at links.
pub fn lexical(path: &Path, current: &Path) -> PathBuf {
    let absolute = current.join(path);
    let parts: Vec<Component> = absolute.components().collect();
    append(PathBuf::from("/"), &parts)
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
    if mode == Mode::Existing {
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

/// `path` without links resolved, as `-s` asks, failing as `mode` says
/// when what must exist does not.
pub fn stripped(path: &Path, current: &Path, mode: Mode) -> io::Result<PathBuf> {
    let path = lexical(path, current);
    let must = match mode {
        Mode::Existing => Some(path.as_path()),
        Mode::Last => path.parent(),
        Mode::Missing => None,
    };
    if let Some(must) = must {
        std::fs::symlink_metadata(must)?;
    }
    Ok(path)
}

/// `path` relative to `base`, both absolute and resolved.
pub fn relative(path: &Path, base: &Path) -> PathBuf {
    let path: Vec<Component> = path.components().collect();
    let base: Vec<Component> = base.components().collect();
    let common = path.iter().zip(&base).take_while(|(a, b)| a == b).count();
    let mut relative = PathBuf::new();
    for _ in common..base.len() {
        relative.push("..");
    }
    for part in &path[common..] {
        relative.push(part);
    }
    match relative.as_os_str().is_empty() {
        true => PathBuf::from("."),
        false => relative,
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
    fn the_last_of_e_and_m_wins() {
        assert_eq!(mode(&run(&["x"])), Mode::Last);
        assert_eq!(mode(&run(&["-e", "x"])), Mode::Existing);
        assert_eq!(mode(&run(&["-em", "x"])), Mode::Missing);
        assert_eq!(
            mode(&run(&["-m", "x", "--canonicalize-existing"])),
            Mode::Existing
        );
        assert_eq!(mode(&run(&["--relative-to", "-e", "-z", "x"])), Mode::Last);
        assert_eq!(mode(&run(&["-e", "--", "-m"])), Mode::Existing);
    }

    #[test]
    fn resolves_dots_lexically() {
        let current = Path::new("/a/b");
        assert_eq!(
            lexical(Path::new("c/./d//"), current),
            Path::new("/a/b/c/d")
        );
        assert_eq!(lexical(Path::new("../../../x"), current), Path::new("/x"));
        assert_eq!(lexical(Path::new("/y/.."), current), Path::new("/"));
    }

    #[test]
    fn writes_paths_relative_to_a_folder() {
        let rel = |p: &str, b: &str| relative(Path::new(p), Path::new(b));
        assert_eq!(rel("/a/b/c", "/a"), Path::new("b/c"));
        assert_eq!(rel("/a/x", "/a/b/c"), Path::new("../../x"));
        assert_eq!(rel("/a", "/a"), Path::new("."));
        assert_eq!(rel("/", "/a"), Path::new(".."));
    }

    #[test]
    fn resolves_links_and_what_may_be_missing() {
        let dir = std::env::temp_dir().join(format!("realpath-test-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join("sub")).unwrap();
        fs::write(dir.join("sub/file"), "").unwrap();
        std::os::unix::fs::symlink("sub", dir.join("link")).unwrap();
        let dir = fs::canonicalize(&dir).unwrap();
        let at = |p: &str| dir.join(p);
        let resolve = |p: &str, mode| resolve(Path::new(p), &dir, mode);
        assert_eq!(
            resolve("link/file", Mode::Existing).unwrap(),
            at("sub/file")
        );
        assert_eq!(resolve("link/../link", Mode::Last).unwrap(), at("sub"));
        assert_eq!(resolve("link/new", Mode::Last).unwrap(), at("sub/new"));
        assert!(resolve("link/new", Mode::Existing).is_err());
        assert!(resolve("no/new", Mode::Last).is_err());
        assert!(resolve("link/file/x", Mode::Last).is_err());
        assert_eq!(resolve("link/no/../x", Mode::Missing).unwrap(), at("sub/x"));
        assert_eq!(
            stripped(Path::new("link/./file"), &dir, Mode::Existing).unwrap(),
            at("link/file")
        );
        assert!(stripped(Path::new("link/no/x"), &dir, Mode::Last).is_err());
        assert_eq!(
            stripped(Path::new("no/x"), &dir, Mode::Missing).unwrap(),
            at("no/x")
        );
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn declares_what_it_takes() {
        let file = concat!(env!("CARGO_MANIFEST_DIR"), "/realpath.msh");
        mikros_cli::declared(&COMMAND, std::path::Path::new(file)).unwrap();
    }
}
