//! What tar takes (MIKSPEC-0098.R57), which of its three modes a run
//! asks for, and which entries it takes: those named, or every one, less
//! those that `--exclude` leaves out.

use crate::pattern::Pattern;
use mikros_cli::{Kind, Operand, Opt, Run, Spec};

pub static COMMAND: Spec = Spec {
    operands: &[Operand::any(
        "path",
        Kind::Path,
        "with -c, the files and folders to store; with -x and -t, the entries to take",
    )],
    options: &[
        Opt::both("create", 'c', "create an archive of the paths"),
        Opt::both("extract", 'x', "extract every entry, or those named"),
        Opt::both("list", 't', "list every entry, or those named"),
        Opt::both(
            "file",
            'f',
            "the archive, - or none for standard input or output",
        )
        .takes("archive", Kind::File),
        Opt::both(
            "verbose",
            'v',
            "print each name; with -t, each entry's type, size, modified time and name",
        ),
        Opt::both("directory", 'C', "create from or extract into this folder")
            .takes("folder", Kind::Folder),
        Opt::both("gzip", 'z', "compress the archive with gzip"),
        Opt::both(
            "keep-old-files",
            'k',
            "keep existing files, failing the entries that would replace them",
        ),
        Opt::long("exclude", "leave out the entries that match the pattern")
            .takes("pattern", Kind::Text),
        Opt::long(
            "json",
            "with -t, write the entries as a JSON array of objects",
        ),
    ],
    ..Spec::new(
        "tar",
        "Creates, extracts and lists tar archives. It writes pax archives, reads\n\
         ustar, pax and GNU archives, and finds gzip by itself when it reads.\n\
         Entries keep their stored mode and modified time; owners are not kept.",
    )
};

/// What a run does.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    Create,
    Extract,
    List,
}

/// The mode that `run` asks for, or why its arguments are invalid.
pub fn mode(run: &Run) -> Result<Mode, String> {
    let modes = [
        ("-c", Mode::Create),
        ("-x", Mode::Extract),
        ("-t", Mode::List),
    ];
    let given: Vec<Mode> = modes
        .iter()
        .filter(|(name, _)| run.has(name))
        .map(|(_, mode)| *mode)
        .collect();
    let mode = match given[..] {
        [mode] => mode,
        _ => return Err("give one of -c, -x and -t".into()),
    };
    if run.has("--json") && mode != Mode::List {
        return Err("--json goes with -t only".into());
    }
    if mode == Mode::Create && run.operands.is_empty() {
        return Err("-c needs the paths to store".into());
    }
    Ok(mode)
}

/// `name` without `./` at its start and `/` at its end, as names are
/// compared.
pub fn plain(name: &str) -> &str {
    let mut name = name.trim_end_matches('/');
    while let Some(rest) = name.strip_prefix("./") {
        name = rest.trim_start_matches('/');
    }
    name
}

/// Which entries a run takes.
pub struct Select {
    /// The names given, and whether an entry had each.
    names: Vec<(String, bool)>,
    exclude: Vec<Pattern>,
}

impl Select {
    /// Every entry, or those of `names` and those in them, less those
    /// that one of `exclude` matches.
    pub fn new(names: &[String], exclude: &[&str]) -> Select {
        Select {
            names: names
                .iter()
                .map(|n| (plain(n).to_string(), false))
                .collect(),
            exclude: exclude.iter().map(|p| Pattern::new(p)).collect(),
        }
    }

    /// Whether the run takes the entry `path`.
    pub fn takes(&mut self, path: &str) -> bool {
        if self.excluded(path) {
            return false;
        }
        if self.names.is_empty() {
            return true;
        }
        let path = plain(path);
        let mut taken = false;
        for (name, found) in &mut self.names {
            let inside = path
                .strip_prefix(name.as_str())
                .is_some_and(|rest| rest.is_empty() || rest.starts_with('/'));
            if inside {
                *found = true;
                taken = true;
            }
        }
        taken
    }

    /// Whether one of the patterns of `--exclude` matches `path`: all of
    /// it, or the parts from any of its parts on, or the first of those.
    pub fn excluded(&self, path: &str) -> bool {
        if self.exclude.is_empty() {
            return false;
        }
        let parts: Vec<&str> = plain(path).split('/').collect();
        (0..parts.len()).any(|from| {
            (from + 1..=parts.len()).any(|to| {
                let text = parts[from..to].join("/");
                self.exclude.iter().any(|p| p.matches(&text))
            })
        })
    }

    /// The names given that no entry had.
    pub fn missing(&self) -> Vec<&str> {
        self.names
            .iter()
            .filter(|(_, found)| !found)
            .map(|(name, _)| name.as_str())
            .collect()
    }
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
    fn takes_one_mode() {
        assert_eq!(mode(&run(&["-czf", "a.tgz", "d"])), Ok(Mode::Create));
        assert_eq!(mode(&run(&["-xvf", "a.tar"])), Ok(Mode::Extract));
        assert_eq!(mode(&run(&["--list", "--json"])), Ok(Mode::List));
        assert!(mode(&run(&["-f", "a.tar"])).is_err());
        assert!(mode(&run(&["-cx", "d"])).is_err());
        assert!(mode(&run(&["-x", "--json"])).is_err());
        assert!(mode(&run(&["-c"])).is_err());
        let r = run(&[
            "-cf",
            "-",
            "--exclude",
            "*.o",
            "--exclude=b",
            "-C",
            "dir",
            "x",
        ]);
        assert_eq!(r.value("-f"), Some("-"));
        assert_eq!(r.values("--exclude"), ["*.o", "b"]);
        assert_eq!(r.value("-C"), Some("dir"));
    }

    #[test]
    fn takes_named_entries_and_what_is_in_them() {
        let mut select = Select::new(&["dir/".into(), "./f".into(), "none".into()], &[]);
        assert!(select.takes("dir"));
        assert!(select.takes("./dir/a/b"));
        assert!(!select.takes("dirt"));
        assert!(select.takes("f"));
        assert!(!select.takes("g"));
        assert_eq!(select.missing(), ["none"]);
        let mut all = Select::new(&[], &[]);
        assert!(all.takes("anything"));
        assert!(all.missing().is_empty());
    }

    #[test]
    fn excludes_by_any_trailing_parts() {
        let select = Select::new(&[], &["*.o", "cache", "a/b"]);
        assert!(select.excluded("x.o"));
        assert!(select.excluded("src/deep/x.o"));
        assert!(select.excluded("cache"));
        assert!(select.excluded("./top/cache/file"));
        assert!(select.excluded("z/a/b/c"));
        assert!(!select.excluded("src/x.c"));
        assert!(!select.excluded("caches"));
    }

    #[test]
    fn declares_what_it_takes() {
        let file = concat!(env!("CARGO_MANIFEST_DIR"), "/tar.msh");
        mikros_cli::declared(&COMMAND, std::path::Path::new(file)).unwrap();
    }
}
