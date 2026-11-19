//! What unzip takes (MIKSPEC-0098.R59), which of its modes a run asks
//! for, and which entries it takes: those that the names given match, as
//! wildcard patterns, or every one, less those that `-x` matches.

use crate::central::Record;
use crate::pattern::Pattern;
use mikros_cli::{Kind, Operand, Opt, Run, Spec};

pub static COMMAND: Spec = Spec {
    operands: &[
        Operand::one("archive", Kind::File, "the zip archive"),
        Operand::any(
            "name",
            Kind::Text,
            "the entries to take, as wildcard patterns; every one when none is named",
        ),
    ],
    options: &[
        Opt::short('d', "extract into this folder instead of the current one")
            .takes("folder", Kind::Folder),
        Opt::short('l', "list each entry's size, modified time and name"),
        Opt::short('t', "check every entry's CRC-32 without writing anything"),
        Opt::short('p', "write the contents to standard output"),
        Opt::short('q', "print nothing but errors"),
        Opt::short('j', "extract without the entries' folders"),
        Opt::short('o', "overwrite existing files without asking"),
        Opt::short('n', "never overwrite existing files"),
        Opt::short('x', "leave out the entries that this pattern matches")
            .takes("name", Kind::Text),
        Opt::long(
            "json",
            "with -l, write the entries as a JSON array of objects",
        ),
    ],
    ..Spec::new(
        "unzip",
        "Extracts the entries of a zip archive, or those named, or lists, checks or\n\
         writes them. It takes stored and deflated entries and ZIP64 archives, and\n\
         asks before overwriting a file when standard input is a terminal.",
    )
};

/// What a run does.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    Extract,
    List,
    Test,
    /// `-p`.
    Pipe,
}

/// How existing files are treated.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Existing {
    Overwrite,
    Never,
    Ask,
}

/// The mode that `run` asks for, or why its arguments are invalid.
pub fn mode(run: &Run) -> Result<Mode, String> {
    let modes = [("-l", Mode::List), ("-t", Mode::Test), ("-p", Mode::Pipe)];
    let given: Vec<Mode> = modes
        .iter()
        .filter(|(name, _)| run.has(name))
        .map(|(_, mode)| *mode)
        .collect();
    let mode = match given[..] {
        [] => Mode::Extract,
        [mode] => mode,
        _ => return Err("give at most one of -l, -t and -p".into()),
    };
    if run.has("--json") && mode != Mode::List {
        return Err("--json goes with -l only".into());
    }
    if run.has("-o") && run.has("-n") {
        return Err("give at most one of -o and -n".into());
    }
    Ok(mode)
}

pub fn existing(run: &Run) -> Existing {
    match (run.has("-o"), run.has("-n")) {
        (true, _) => Existing::Overwrite,
        (_, true) => Existing::Never,
        _ => Existing::Ask,
    }
}

/// The indexes of the records that the run takes, in their order, and
/// the names given that matched none.
pub fn chosen(records: &[Record], names: &[String], exclude: &[&str]) -> (Vec<usize>, Vec<String>) {
    let patterns: Vec<Pattern> = names.iter().map(|n| Pattern::new(n)).collect();
    let exclude: Vec<Pattern> = exclude.iter().map(|p| Pattern::new(p)).collect();
    let mut found = vec![false; names.len()];
    let mut taken = Vec::new();
    for (i, record) in records.iter().enumerate() {
        let mut wanted = names.is_empty();
        for (pattern, found) in patterns.iter().zip(&mut found) {
            if pattern.matches(&record.name) {
                *found = true;
                wanted = true;
            }
        }
        if wanted && !exclude.iter().any(|p| p.matches(&record.name)) {
            taken.push(i);
        }
    }
    let unmatched = names
        .iter()
        .zip(found)
        .filter(|(_, found)| !found)
        .map(|(name, _)| name.clone())
        .collect();
    (taken, unmatched)
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
        assert_eq!(mode(&run(&["a.zip"])), Ok(Mode::Extract));
        assert_eq!(mode(&run(&["-l", "a.zip", "--json"])), Ok(Mode::List));
        assert_eq!(mode(&run(&["-tq", "a.zip"])), Ok(Mode::Test));
        assert_eq!(mode(&run(&["-p", "a.zip", "x"])), Ok(Mode::Pipe));
        assert!(mode(&run(&["-lt", "a.zip"])).is_err());
        assert!(mode(&run(&["--json", "a.zip"])).is_err());
        assert!(mode(&run(&["-on", "a.zip"])).is_err());
        assert_eq!(existing(&run(&["-o", "a.zip"])), Existing::Overwrite);
        assert_eq!(existing(&run(&["-n", "a.zip"])), Existing::Never);
        assert_eq!(existing(&run(&["a.zip"])), Existing::Ask);
        let r = run(&["a.zip", "-d", "out", "-x", "*.o", "-x", "b"]);
        assert_eq!(r.value("-d"), Some("out"));
        assert_eq!(r.values("-x"), ["*.o", "b"]);
    }

    #[test]
    fn chooses_entries_by_patterns() {
        let records: Vec<Record> = ["a/1.c", "a/2.o", "b", "c"]
            .iter()
            .map(|n| Record {
                name: n.to_string(),
                ..Record::default()
            })
            .collect();
        let names = |n: &[&str]| n.iter().map(|n| n.to_string()).collect::<Vec<_>>();
        assert_eq!(chosen(&records, &[], &[]).0, [0, 1, 2, 3]);
        assert_eq!(chosen(&records, &[], &["*.o", "c"]).0, [0, 2]);
        let (taken, unmatched) = chosen(&records, &names(&["a/*", "none", "b"]), &["*.o"]);
        assert_eq!(taken, [0, 2]);
        assert_eq!(unmatched, ["none"]);
    }

    #[test]
    fn declares_what_it_takes() {
        let file = concat!(env!("CARGO_MANIFEST_DIR"), "/unzip.msh");
        mikros_cli::declared(&COMMAND, std::path::Path::new(file)).unwrap();
    }
}
