//! What mv takes, how it pairs each source with its destination, and
//! when it replaces what is there (MIKSPEC-0098.R36).

use mikros_cli::{Kind, Operand, Opt, Run, Spec};
use std::io;
use std::path::{Path, PathBuf};

pub static COMMAND: Spec = Spec {
    operands: &[Operand::some(
        "path",
        Kind::Path,
        "the sources, then the target unless -t names it",
    )],
    options: &[
        Opt::both("force", 'f', "replace destinations without asking"),
        Opt::both("interactive", 'i', "ask before replacing a destination"),
        Opt::both("no-clobber", 'n', "never replace a destination"),
        Opt::both(
            "update",
            'u',
            "move only over destinations older than their source",
        ),
        Opt::both("verbose", 'v', "name each file as it is moved"),
        Opt::both(
            "target-directory",
            't',
            "move every source into this folder",
        )
        .takes("folder", Kind::Folder),
        Opt::both(
            "no-target-directory",
            'T',
            "take the target as the destination, even a folder",
        ),
    ],
    ..Spec::new(
        "mv",
        "Renames files and folders to the target, or moves them into it when\n\
         it is a folder; across file systems, copies them and removes them.",
    )
};

/// Whether a destination that is there is replaced.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Clobber {
    Never,
    Ask,
    Always,
}

/// What the options ask.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Options {
    pub clobber: Clobber,
    pub update: bool,
    pub verbose: bool,
}

impl Options {
    /// The options of `run`. Of `-n`, `-i` and `-f`, the last one given
    /// decides.
    pub fn of(run: &Run) -> Options {
        Options {
            clobber: match run.last_of(&["-n", "-i", "-f"]) {
                Some("-n") => Clobber::Never,
                Some("-i") => Clobber::Ask,
                _ => Clobber::Always,
            },
            update: run.has("-u"),
            verbose: run.has("-v"),
        }
    }
}

/// Why the operands cannot be paired: invalid arguments, or a target
/// that is not a folder.
#[derive(Debug, PartialEq, Eq)]
pub enum Refused {
    Invalid(String),
    Failed(String),
}

/// A source and its destination, or why the source has none.
pub type Pair = Result<(PathBuf, PathBuf), String>;

/// Options that do not go together.
pub fn conflict(run: &Run) -> Option<&'static str> {
    match run.has("-t") && run.has("-T") {
        true => Some("-t and -T cannot be given together"),
        false => None,
    }
}

/// Pairs each source of `operands` with its destination: in the folder
/// `target` of `-t`; the last operand itself with `-T` or when it is
/// not a folder, for one source; or in the last operand when
/// `is_folder` says it is one.
pub fn plan(
    operands: &[String],
    target: Option<&str>,
    no_target: bool,
    is_folder: &dyn Fn(&Path) -> bool,
) -> Result<Vec<Pair>, Refused> {
    let into = |folder: &str, sources: &[String]| -> Result<Vec<Pair>, Refused> {
        if !is_folder(Path::new(folder)) {
            return Err(Refused::Failed(format!("target {folder} is not a folder")));
        }
        Ok(sources.iter().map(|s| inside(folder, s)).collect())
    };
    if let Some(folder) = target {
        return into(folder, operands);
    }
    if operands.len() < 2 {
        let first = operands.first().map_or("", String::as_str);
        return Err(Refused::Invalid(format!(
            "missing destination after {first}"
        )));
    }
    let (last, sources) = operands.split_last().expect("two operands");
    if no_target {
        if operands.len() > 2 {
            return Err(Refused::Invalid(format!("extra operand {last}")));
        }
        return Ok(vec![Ok((sources[0].clone().into(), last.into()))]);
    }
    if is_folder(Path::new(last)) || sources.len() > 1 {
        return into(last, sources);
    }
    Ok(vec![Ok((sources[0].clone().into(), last.into()))])
}

/// `source` with its destination in `folder`, under its own name.
fn inside(folder: &str, source: &str) -> Pair {
    match Path::new(source).file_name() {
        Some(name) => Ok((source.into(), Path::new(folder).join(name))),
        None => Err(format!(
            "cannot move {source}: it has no name to move under"
        )),
    }
}

/// Whether `destination` lies in the folder `source`, both absolute
/// with links resolved.
pub fn within(source: &Path, destination: &Path) -> bool {
    destination.starts_with(source)
}

/// Whether a time, as seconds and nanoseconds, is later than another.
pub fn newer(source: (i64, i64), destination: (i64, i64)) -> bool {
    source > destination
}

/// Whether the answer to a question is yes.
pub fn yes(answer: &str) -> bool {
    answer.trim_start().starts_with(['y', 'Y'])
}

/// The C library's error number for a rename across file systems, which
/// the file system's `CROSS_DEVICE` becomes.
const EXDEV: i32 = 18;

/// Whether renaming failed because the destination is on another file
/// system.
pub fn across(error: &io::Error) -> bool {
    error.raw_os_error() == Some(EXDEV) || error.kind() == io::ErrorKind::CrossesDevices
}

#[cfg(test)]
mod tests {
    use super::*;
    use mikros_cli::Parsed;

    fn run(arguments: &[&str]) -> Run {
        match COMMAND.parse(arguments.iter().map(|a| a.to_string())) {
            Parsed::Run(run) => run,
            other => panic!("{other:?}"),
        }
    }

    fn pairs(
        operands: &[&str],
        target: Option<&str>,
        no_target: bool,
    ) -> Result<Vec<Pair>, Refused> {
        let operands: Vec<String> = operands.iter().map(|o| o.to_string()).collect();
        let folder = |p: &Path| p.to_str().is_some_and(|p| p.starts_with("dir"));
        plan(&operands, target, no_target, &folder)
    }

    fn pair(source: &str, destination: &str) -> Pair {
        Ok((source.into(), destination.into()))
    }

    #[test]
    fn reads_the_options() {
        let o = Options::of(&run(&["a", "b"]));
        assert_eq!(o.clobber, Clobber::Always);
        assert!(!o.update && !o.verbose);
        assert_eq!(Options::of(&run(&["-fi", "a", "b"])).clobber, Clobber::Ask);
        assert_eq!(
            Options::of(&run(&["-in", "a", "b"])).clobber,
            Clobber::Never
        );
        assert!(Options::of(&run(&["-uv", "a", "b"])).update);
        assert_eq!(
            conflict(&run(&["-Tt", "x", "a"])),
            Some("-t and -T cannot be given together")
        );
    }

    #[test]
    fn pairs_sources_with_destinations() {
        assert_eq!(pairs(&["a", "b"], None, false), Ok(vec![pair("a", "b")]));
        assert_eq!(
            pairs(&["a", "x/b/", "dir"], None, false),
            Ok(vec![pair("a", "dir/a"), pair("x/b/", "dir/b")])
        );
        assert_eq!(pairs(&["a", "dir"], None, true), Ok(vec![pair("a", "dir")]));
        assert_eq!(
            pairs(&["a"], Some("dir"), false),
            Ok(vec![pair("a", "dir/a")])
        );
        assert_eq!(
            pairs(&["/", "dir"], None, false),
            Ok(vec![Err(
                "cannot move /: it has no name to move under".into()
            )])
        );
        assert_eq!(
            pairs(&["a"], None, false),
            Err(Refused::Invalid("missing destination after a".into()))
        );
        assert_eq!(
            pairs(&["a", "b", "c"], None, true),
            Err(Refused::Invalid("extra operand c".into()))
        );
        assert_eq!(
            pairs(&["a", "b", "c"], None, false),
            Err(Refused::Failed("target c is not a folder".into()))
        );
    }

    #[test]
    fn compares_paths_times_answers_and_errors() {
        assert!(within(Path::new("/a"), Path::new("/a/b")));
        assert!(!within(Path::new("/a"), Path::new("/ab")));
        assert!(newer((2, 0), (1, 9)) && !newer((1, 9), (1, 9)));
        assert!(yes("yes\n") && !yes("no\n"));
        assert!(across(&io::Error::from_raw_os_error(EXDEV)));
        assert!(across(&io::Error::from(io::ErrorKind::CrossesDevices)));
        assert!(!across(&io::Error::from(io::ErrorKind::NotFound)));
    }

    #[test]
    fn declares_what_it_takes() {
        let file = concat!(env!("CARGO_MANIFEST_DIR"), "/mv.msh");
        mikros_cli::declared(&COMMAND, std::path::Path::new(file)).unwrap();
    }
}
