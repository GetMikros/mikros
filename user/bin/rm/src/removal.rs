//! What rm takes, which paths it refuses, and when it asks
//! (MIKSPEC-0098.R37).

use mikros_cli::{Kind, Operand, Opt, Run, Spec};
use std::path::{Component, Path, PathBuf};

pub static COMMAND: Spec = Spec {
    operands: &[Operand::any(
        "path",
        Kind::Path,
        "the files and folders to remove",
    )],
    options: &[
        Opt::both("force", 'f', "ignore paths that do not exist and never ask"),
        Opt::short('i', "ask before each removal"),
        Opt::short(
            'I',
            "ask once before removing more than three paths or recursively",
        ),
        Opt::both("recursive", 'r', "remove folders and what they hold"),
        Opt::short('R', "the same as -r"),
        Opt::both("dir", 'd', "remove empty folders"),
        Opt::both("verbose", 'v', "name each entry removed"),
    ],
    ..Spec::new(
        "rm",
        "Removes files, and folders with -r or -d.\n\
         It never removes / or what belongs to the read-only system version.",
    )
};

/// When rm asks before removing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Asking {
    Never,
    /// Once, before removing more than three paths or recursively.
    Once,
    Each,
}

/// What a run asks for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Settings {
    pub force: bool,
    pub asking: Asking,
    pub recursive: bool,
    pub dir: bool,
    pub verbose: bool,
}

impl Settings {
    /// The settings of `run`. Of `-f`, `-i` and `-I`, the last one given
    /// decides, as POSIX says for `-f` and `-i`.
    pub fn of(run: &Run) -> Settings {
        let last = run.last_of(&["-f", "-i", "-I"]);
        let force = last == Some("-f");
        let asking = match last {
            Some("-i") => Asking::Each,
            Some("-I") => Asking::Once,
            _ => Asking::Never,
        };
        Settings {
            force,
            asking,
            recursive: run.has("-r") || run.has("-R"),
            dir: run.has("-d"),
            verbose: run.has("-v"),
        }
    }

    /// The question that `-I` asks once before removing `count` paths,
    /// if it asks one.
    pub fn once(&self, count: usize) -> Option<String> {
        if self.asking != Asking::Once || (count <= 3 && !self.recursive) {
            return None;
        }
        let noun = if count == 1 { "path" } else { "paths" };
        Some(match self.recursive {
            true => format!("remove {count} {noun} recursively"),
            false => format!("remove {count} {noun}"),
        })
    }
}

/// Why rm refuses to remove a path.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Refusal {
    /// Its last name is `.` or `..`.
    Dots,
    Root,
    /// It belongs to the read-only system version (MIKSPEC-0100.R5).
    System,
}

impl Refusal {
    pub fn why(self) -> &'static str {
        match self {
            Refusal::Dots => "the folders . and .. are not removed",
            Refusal::Root => "it is the root folder",
            Refusal::System => "it is part of the read-only system version",
        }
    }
}

/// Whether the last name of `operand`, as given, is `.` or `..`.
pub fn dots(operand: &str) -> bool {
    let trimmed = operand.trim_end_matches('/');
    let last = trimmed.rsplit('/').next().unwrap_or("");
    !trimmed.is_empty() && (last == "." || last == "..")
}

/// The entries below `/system` at which writable file systems appear
/// (MIKSPEC-0100.R19); what lies below them is not the system version's,
/// but they themselves are.
const WRITABLE: [&str; 5] = ["config", "state", "logs", "tmp", "boot/efi"];

/// Why `path`, absolute and without `.`, `..` or links in its folders,
/// is refused: the root, `/system`, and everything below `/system` that
/// is not below one of its writable entries. The version's drivers
/// share `/system/drivers` with installed ones, and are refused with it.
pub fn refused(path: &Path) -> Option<Refusal> {
    if path == Path::new("/") {
        return Some(Refusal::Root);
    }
    let rest = path.strip_prefix("/system").ok()?;
    let below_writable = WRITABLE.iter().any(|w| {
        rest.strip_prefix(w)
            .is_ok_and(|inside| inside.components().next().is_some())
    });
    match below_writable {
        true => None,
        false => Some(Refusal::System),
    }
}

/// `path` made absolute against `cwd` and without `.` and `..`, by its
/// names alone.
pub fn normalized(cwd: &Path, path: &Path) -> PathBuf {
    let mut out = PathBuf::from("/");
    for component in cwd.join(path).components() {
        match component {
            Component::Normal(name) => out.push(name),
            Component::ParentDir => {
                out.pop();
            }
            Component::RootDir | Component::CurDir | Component::Prefix(_) => {}
        }
    }
    out
}

/// The path that `operand` names, absolute, with the links of its
/// folders resolved by `canonical` where it can, and by names otherwise.
/// Its last name is kept, so that a link is judged as itself.
pub fn resolved(
    cwd: &Path,
    operand: &Path,
    canonical: impl Fn(&Path) -> Option<PathBuf>,
) -> PathBuf {
    let path = normalized(cwd, operand);
    let (Some(parent), Some(name)) = (path.parent(), path.file_name()) else {
        return path;
    };
    let parent = canonical(parent).unwrap_or_else(|| parent.to_path_buf());
    normalized(Path::new("/"), &parent).join(name)
}

/// Whether `answer`, a line read after a question, says yes.
pub fn yes(answer: &str) -> bool {
    matches!(answer.trim_start().chars().next(), Some('y' | 'Y'))
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

    #[test]
    fn the_last_of_force_and_asking_decides() {
        assert_eq!(Settings::of(&run(&["-fi"])).asking, Asking::Each);
        assert!(!Settings::of(&run(&["-fi"])).force);
        assert_eq!(Settings::of(&run(&["-if"])).asking, Asking::Never);
        assert!(Settings::of(&run(&["-if"])).force);
        assert_eq!(Settings::of(&run(&["-iI"])).asking, Asking::Once);
        assert_eq!(Settings::of(&run(&["-Ii"])).asking, Asking::Each);
        assert_eq!(Settings::of(&run(&["-I"])).asking, Asking::Once);
        assert!(Settings::of(&run(&["-R"])).recursive);
    }

    #[test]
    fn asks_once_for_many_paths_or_recursively() {
        let once = Settings::of(&run(&["-I"]));
        assert_eq!(once.once(3), None);
        assert_eq!(once.once(4).as_deref(), Some("remove 4 paths"));
        let recursive = Settings::of(&run(&["-Ir"]));
        assert_eq!(
            recursive.once(1).as_deref(),
            Some("remove 1 path recursively")
        );
        assert_eq!(Settings::of(&run(&["-r"])).once(9), None);
    }

    #[test]
    fn refuses_the_root_and_the_system_version() {
        let refused = |p: &str| refused(Path::new(p));
        assert_eq!(refused("/"), Some(Refusal::Root));
        assert_eq!(refused("/system"), Some(Refusal::System));
        assert_eq!(refused("/system/bin/ls"), Some(Refusal::System));
        assert_eq!(refused("/system/state"), Some(Refusal::System));
        assert_eq!(refused("/system/boot"), Some(Refusal::System));
        assert_eq!(refused("/system/boot/efi"), Some(Refusal::System));
        assert_eq!(refused("/system/drivers/x"), Some(Refusal::System));
        assert_eq!(refused("/system/state/logsvc"), None);
        assert_eq!(refused("/system/boot/efi/EFI"), None);
        assert_eq!(refused("/system/tmp/a/b"), None);
        assert_eq!(refused("/systems"), None);
        assert_eq!(refused("/users/ada/system"), None);
    }

    #[test]
    fn resolves_the_folders_but_not_the_last_name() {
        let cwd = Path::new("/users/ada");
        let none = |_: &Path| None;
        assert_eq!(
            resolved(cwd, Path::new("a/./b/../c"), none),
            Path::new("/users/ada/a/c")
        );
        assert_eq!(resolved(cwd, Path::new("../../.."), none), Path::new("/"));
        assert_eq!(resolved(cwd, Path::new("/"), none), Path::new("/"));
        let link = |p: &Path| (p == Path::new("/users/ada/s")).then(|| PathBuf::from("/system"));
        assert_eq!(
            resolved(cwd, Path::new("s/bin"), link),
            Path::new("/system/bin")
        );
        assert_eq!(
            resolved(cwd, Path::new("s"), link),
            Path::new("/users/ada/s")
        );
    }

    #[test]
    fn finds_dots_and_answers() {
        assert!(dots(".") && dots("..") && dots("a/..") && dots("a/./"));
        assert!(!dots("a") && !dots(".a") && !dots("/") && !dots("a/..b"));
        assert!(yes("y\n") && yes("  Yes") && !yes("n") && !yes(""));
    }

    #[test]
    fn declares_what_it_takes() {
        let file = concat!(env!("CARGO_MANIFEST_DIR"), "/rm.msh");
        mikros_cli::declared(&COMMAND, std::path::Path::new(file)).unwrap();
    }
}
