//! What zip takes (MIKSPEC-0098.R58): the archive and the paths to add,
//! or with `-d` the names of the entries to remove, and the level of
//! compression.

use mikros_cli::{Kind, Operand, Opt, Run, Spec};

pub static COMMAND: Spec = Spec {
    operands: &[
        Operand::one("archive", Kind::File, "the zip archive to create or change"),
        Operand::some(
            "path",
            Kind::Path,
            "the files and folders to add, or with -d the entries to remove",
        ),
    ],
    options: &[
        Opt::short('r', "add folders with what they hold"),
        Opt::short('q', "print nothing but errors"),
        Opt::short('0', "store without compressing"),
        Opt::short('1', "deflate, fastest"),
        Opt::short('2', "deflate at level 2"),
        Opt::short('3', "deflate at level 3"),
        Opt::short('4', "deflate at level 4"),
        Opt::short('5', "deflate at level 5"),
        Opt::short('6', "deflate at level 6, as without a level"),
        Opt::short('7', "deflate at level 7"),
        Opt::short('8', "deflate at level 8"),
        Opt::short('9', "deflate, smallest"),
        Opt::short(
            'd',
            "remove the entries that the names match, as wildcard patterns",
        ),
        Opt::short('j', "store the names without their folders"),
        Opt::short('y', "store links as links instead of what they lead to"),
        Opt::short('x', "leave out the paths that this pattern matches")
            .takes("pattern", Kind::Text),
    ],
    ..Spec::new(
        "zip",
        "Creates a zip archive of the paths, or adds them to it, replacing entries of\n\
         the same name. Names are stored in UTF-8, with each entry's modified time\n\
         and stored mode, and ZIP64 records where they are needed; nothing is\n\
         encrypted.",
    )
};

const LEVELS: [&str; 10] = ["-0", "-1", "-2", "-3", "-4", "-5", "-6", "-7", "-8", "-9"];

/// The level of compression: 0 to store, the last given, or 6.
pub fn level(run: &Run) -> u32 {
    run.last_of(&LEVELS)
        .and_then(|l| l[1..].parse().ok())
        .unwrap_or(6)
}

/// `name`, a path given, as an entry's name: without `/` at its start,
/// without `.` parts, and without what comes before its last `..`
/// part; only its last part with `-j`.
pub fn stored(name: &str, junk: bool) -> String {
    let parts: Vec<&str> = name
        .split('/')
        .filter(|p| !p.is_empty() && *p != ".")
        .collect();
    let from = parts.iter().rposition(|p| *p == "..").map_or(0, |i| i + 1);
    let parts = &parts[from..];
    match junk {
        true => parts.last().copied().unwrap_or("").to_string(),
        false => parts.join("/"),
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
    fn takes_the_last_level() {
        assert_eq!(level(&run(&["a.zip", "f"])), 6);
        assert_eq!(level(&run(&["-0", "a.zip", "f"])), 0);
        assert_eq!(level(&run(&["-9", "a.zip", "-1", "f"])), 1);
        assert_eq!(level(&run(&["-r3q", "a.zip", "f"])), 3);
        let r = run(&["-d", "a.zip", "x*", "-x", "*.o"]);
        assert!(r.has("-d"));
        assert_eq!(r.operands, ["a.zip", "x*"]);
        assert_eq!(r.values("-x"), ["*.o"]);
    }

    #[test]
    fn stores_names_inside_the_archive() {
        assert_eq!(stored("dir/file", false), "dir/file");
        assert_eq!(stored("/abs/./file", false), "abs/file");
        assert_eq!(stored("../up/file", false), "up/file");
        assert_eq!(stored("a/../b", false), "b");
        assert_eq!(stored("./dir/file", true), "file");
    }

    #[test]
    fn declares_what_it_takes() {
        let file = concat!(env!("CARGO_MANIFEST_DIR"), "/zip.msh");
        mikros_cli::declared(&COMMAND, std::path::Path::new(file)).unwrap();
    }
}
