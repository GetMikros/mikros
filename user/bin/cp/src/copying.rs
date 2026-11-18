//! What cp takes, how it pairs each source with its destination, and
//! when it replaces what is there (MIKSPEC-0098.R36).

use mikros_cli::{Kind, Operand, Opt, Run, Spec};
use std::path::{Path, PathBuf};

pub static COMMAND: Spec = Spec {
    operands: &[Operand::some(
        "path",
        Kind::Path,
        "the sources, then the target unless -t names it",
    )],
    options: &[
        Opt::both("recursive", 'r', "copy folders with what they hold"),
        Opt::short('R', "copy folders with what they hold, as -r"),
        Opt::both(
            "force",
            'f',
            "remove a destination that cannot be opened, and try again",
        ),
        Opt::both("interactive", 'i', "ask before replacing a destination"),
        Opt::both("no-clobber", 'n', "never replace a destination"),
        Opt::both(
            "update",
            'u',
            "copy only over destinations older than their source",
        ),
        Opt::both("verbose", 'v', "name each file as it is copied"),
        Opt::both("link", 'l', "make hard links instead of copying"),
        Opt::both(
            "symbolic-link",
            's',
            "make symbolic links instead of copying",
        ),
        Opt::both(
            "target-directory",
            't',
            "copy every source into this folder",
        )
        .takes("folder", Kind::Folder),
        Opt::both(
            "no-target-directory",
            'T',
            "take the target as the destination, even a folder",
        ),
        Opt::short('p', "keep the times, the mode and the access control lists"),
        Opt::both("archive", 'a', "as -r -p, copying links as links"),
        Opt::long(
            "reflink",
            "share contents where the file system can: auto, always or never",
        )
        .takes("when", Kind::Choice(&["auto", "always", "never"])),
    ],
    ..Spec::new(
        "cp",
        "Copies files, and folders with -r, to the target, or into it when\n\
         it is a folder.",
    )
};

/// Whether a destination that is there is replaced.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Clobber {
    Never,
    Ask,
    Always,
}

/// What cp makes at a destination.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Make {
    Copy,
    Hard,
    Symbolic,
}

/// Whether contents are shared with `CloneRange`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Reflink {
    Auto,
    Always,
    Never,
}

/// What the options ask.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Options {
    pub recursive: bool,
    /// Keeps times, mode and access control lists.
    pub preserve: bool,
    /// Follows links among the sources, which a recursive copy does not.
    pub follow: bool,
    pub make: Make,
    pub clobber: Clobber,
    pub force: bool,
    pub update: bool,
    pub verbose: bool,
    pub reflink: Reflink,
}

impl Options {
    /// The options of `run`.
    pub fn of(run: &Run) -> Options {
        let archive = run.has("-a");
        let recursive = archive || run.has("-r") || run.has("-R");
        Options {
            recursive,
            preserve: archive || run.has("-p"),
            follow: !recursive,
            make: match (run.has("-l"), run.has("-s")) {
                (true, _) => Make::Hard,
                (_, true) => Make::Symbolic,
                _ => Make::Copy,
            },
            // Of -n, -i and -f, the last one given decides.
            clobber: match run.last_of(&["-n", "-i", "-f"]) {
                Some("-n") => Clobber::Never,
                Some("-i") => Clobber::Ask,
                _ => Clobber::Always,
            },
            force: run.has("-f"),
            update: run.has("-u"),
            verbose: run.has("-v"),
            reflink: match run.value("--reflink") {
                Some("always") => Reflink::Always,
                Some("never") => Reflink::Never,
                _ => Reflink::Auto,
            },
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
    if run.has("-t") && run.has("-T") {
        return Some("-t and -T cannot be given together");
    }
    if run.has("-l") && run.has("-s") {
        return Some("-l and -s cannot be given together");
    }
    None
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
            "cannot copy {source}: it has no name to copy under"
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

    fn strings(operands: &[&str]) -> Vec<String> {
        operands.iter().map(|o| o.to_string()).collect()
    }

    fn pairs(
        operands: &[&str],
        target: Option<&str>,
        no_target: bool,
    ) -> Result<Vec<Pair>, Refused> {
        let folder = |p: &Path| p.to_str().is_some_and(|p| p.starts_with("dir"));
        plan(&strings(operands), target, no_target, &folder)
    }

    fn pair(source: &str, destination: &str) -> Pair {
        Ok((source.into(), destination.into()))
    }

    #[test]
    fn reads_the_options() {
        let o = Options::of(&run(&["a", "b"]));
        assert!(!o.recursive && o.follow && !o.preserve);
        assert_eq!(
            (o.make, o.clobber, o.reflink),
            (Make::Copy, Clobber::Always, Reflink::Auto)
        );
        let o = Options::of(&run(&["-a", "a", "b"]));
        assert!(o.recursive && o.preserve && !o.follow);
        let o = Options::of(&run(&["-Rin", "--reflink=never", "a", "b"]));
        assert!(o.recursive && !o.preserve);
        assert_eq!((o.clobber, o.reflink), (Clobber::Never, Reflink::Never));
        assert_eq!(Options::of(&run(&["-s", "a", "b"])).make, Make::Symbolic);
        assert_eq!(Options::of(&run(&["-li", "a", "b"])).clobber, Clobber::Ask);
        // The last of -n, -i and -f decides.
        assert_eq!(Options::of(&run(&["-ni", "a", "b"])).clobber, Clobber::Ask);
        assert_eq!(
            Options::of(&run(&["-if", "a", "b"])).clobber,
            Clobber::Always
        );
        assert_eq!(
            conflict(&run(&["-ls", "a", "b"])),
            Some("-l and -s cannot be given together")
        );
        assert_eq!(
            conflict(&run(&["-T", "-t", "x", "a"])),
            Some("-t and -T cannot be given together")
        );
        assert_eq!(conflict(&run(&["a", "b"])), None);
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
            pairs(&["a", "b"], Some("dir2"), false),
            Ok(vec![pair("a", "dir2/a"), pair("b", "dir2/b")])
        );
        assert_eq!(
            pairs(&["..", "dir"], None, false),
            Ok(vec![Err(
                "cannot copy ..: it has no name to copy under".into()
            )])
        );
    }

    #[test]
    fn refuses_what_cannot_be_paired() {
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
        assert_eq!(
            pairs(&["a"], Some("x"), false),
            Err(Refused::Failed("target x is not a folder".into()))
        );
    }

    #[test]
    fn compares_paths_times_and_answers() {
        assert!(within(Path::new("/a/b"), Path::new("/a/b/c")));
        assert!(within(Path::new("/a/b"), Path::new("/a/b")));
        assert!(!within(Path::new("/a/b"), Path::new("/a/bc")));
        assert!(newer((5, 1), (5, 0)) && !newer((5, 0), (5, 0)) && !newer((4, 9), (5, 0)));
        assert!(yes("y\n") && yes("  Yes") && !yes("n") && !yes("") && !yes("\n"));
    }

    #[test]
    fn declares_what_it_takes() {
        let file = concat!(env!("CARGO_MANIFEST_DIR"), "/cp.msh");
        mikros_cli::declared(&COMMAND, std::path::Path::new(file)).unwrap();
    }
}
