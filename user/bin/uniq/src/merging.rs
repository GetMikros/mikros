//! What uniq takes, which part of a line it compares, and how it merges
//! runs of adjacent equal lines (MIKSPEC-0098.R48).

use mikros_cli::{Kind, Operand, Opt, Run, Spec};
use std::io::{self, Write};

pub static COMMAND: Spec = Spec {
    operands: &[
        Operand::optional(
            "input",
            Kind::File,
            "the file to read, or - for standard input",
        ),
        Operand::optional(
            "output",
            Kind::File,
            "the file to write, standard output when absent",
        ),
    ],
    options: &[
        Opt::both("count", 'c', "put how often each line occurs before it"),
        Opt::both(
            "repeated",
            'd',
            "write only lines that occur more than once",
        ),
        Opt::both("unique", 'u', "write only lines that occur once"),
        Opt::both("ignore-case", 'i', "compare without regard to case"),
        Opt::both("skip-fields", 'f', "do not compare the first fields").takes("fields", Kind::Int),
        Opt::both("skip-chars", 's', "do not compare the first characters")
            .takes("characters", Kind::Int),
        Opt::both("check-chars", 'w', "compare at most this many characters")
            .takes("characters", Kind::Int),
        Opt::both("zero-terminated", 'z', "lines end with a NUL byte"),
    ],
    ..Spec::new(
        "uniq",
        "Writes each run of adjacent equal lines of the input once.\n\
         A field is blanks and the characters that follow them up to a blank.",
    )
};

/// How lines are compared and written, from the options.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Merge {
    pub count: bool,
    pub repeated: bool,
    pub unique: bool,
    pub ignore_case: bool,
    pub skip_fields: usize,
    pub skip_chars: usize,
    pub check_chars: Option<usize>,
    /// The byte that ends lines.
    pub end: u8,
}

impl Merge {
    /// How `run` asks lines to be merged, or why its arguments are
    /// invalid.
    pub fn of(run: &Run) -> Result<Merge, String> {
        let number = |option: &str| -> Result<Option<usize>, String> {
            match run.value(option) {
                None => Ok(None),
                Some(text) => text
                    .parse()
                    .map(Some)
                    .map_err(|_| format!("invalid number {text} for {option}")),
            }
        };
        Ok(Merge {
            count: run.has("-c"),
            repeated: run.has("-d"),
            unique: run.has("-u"),
            ignore_case: run.has("-i"),
            skip_fields: number("-f")?.unwrap_or(0),
            skip_chars: number("-s")?.unwrap_or(0),
            check_chars: number("-w")?,
            end: if run.has("-z") { 0 } else { b'\n' },
        })
    }

    /// The part of `line`, without its end, that is compared.
    pub fn key(&self, line: &[u8]) -> Vec<u8> {
        let mut at = 0;
        let blank = |b: u8| b == b' ' || b == b'\t';
        for _ in 0..self.skip_fields {
            while at < line.len() && blank(line[at]) {
                at += 1;
            }
            while at < line.len() && !blank(line[at]) {
                at += 1;
            }
        }
        let rest = &line[at..];
        let rest = &rest[characters(rest, self.skip_chars)..];
        let rest = match self.check_chars {
            Some(n) => &rest[..characters(rest, n)],
            None => rest,
        };
        if !self.ignore_case {
            return rest.to_vec();
        }
        let mut folded = Vec::with_capacity(rest.len());
        for chunk in rest.utf8_chunks() {
            folded.extend_from_slice(chunk.valid().to_lowercase().as_bytes());
            folded.extend_from_slice(chunk.invalid());
        }
        folded
    }
}

/// The bytes that the first `n` characters of `text` take, counting a
/// byte that is not UTF-8 as one.
fn characters(text: &[u8], n: usize) -> usize {
    let mut left = n;
    let mut at = 0;
    for chunk in text.utf8_chunks() {
        for c in chunk.valid().chars() {
            if left == 0 {
                return at;
            }
            left -= 1;
            at += c.len_utf8();
        }
        for _ in chunk.invalid() {
            if left == 0 {
                return at;
            }
            left -= 1;
            at += 1;
        }
    }
    at
}

/// Merges lines given one at a time and writes each run once, as the
/// options say.
pub struct Merger<'a> {
    merge: &'a Merge,
    /// The first line of the current run, its key, and the run's length.
    run: Option<(Vec<u8>, Vec<u8>, u64)>,
}

impl<'a> Merger<'a> {
    pub fn new(merge: &'a Merge) -> Merger<'a> {
        Merger { merge, run: None }
    }

    /// Takes `line`, without its end, writing the run before it to `out`
    /// when it starts another.
    pub fn line(&mut self, line: &[u8], out: &mut impl Write) -> io::Result<()> {
        let key = self.merge.key(line);
        if let Some((_, run_key, count)) = &mut self.run
            && *run_key == key
        {
            *count += 1;
            return Ok(());
        }
        let previous = self.run.replace((line.to_vec(), key, 1));
        self.write(previous, out)
    }

    /// Writes the last run to `out`.
    pub fn finish(mut self, out: &mut impl Write) -> io::Result<()> {
        let last = self.run.take();
        self.write(last, out)
    }

    fn write(&self, run: Option<(Vec<u8>, Vec<u8>, u64)>, out: &mut impl Write) -> io::Result<()> {
        let Some((line, _, count)) = run else {
            return Ok(());
        };
        let shown = match (self.merge.repeated, self.merge.unique) {
            (true, true) => false,
            (true, false) => count > 1,
            (false, true) => count == 1,
            (false, false) => true,
        };
        if !shown {
            return Ok(());
        }
        if self.merge.count {
            write!(out, "{count:>7} ")?;
        }
        out.write_all(&line)?;
        out.write_all(&[self.merge.end])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mikros_cli::Parsed;

    fn merge(arguments: &[&str]) -> Merge {
        match COMMAND.parse(arguments.iter().map(|a| a.to_string())) {
            Parsed::Run(run) => Merge::of(&run).unwrap(),
            other => panic!("{other:?}"),
        }
    }

    fn uniq(arguments: &[&str], text: &str) -> String {
        let merge = merge(arguments);
        let mut merger = Merger::new(&merge);
        let mut out = Vec::new();
        for line in text.lines() {
            merger.line(line.as_bytes(), &mut out).unwrap();
        }
        merger.finish(&mut out).unwrap();
        String::from_utf8(out).unwrap()
    }

    #[test]
    fn merges_adjacent_equal_lines() {
        assert_eq!(uniq(&[], "a\na\nb\na\n"), "a\nb\na\n");
        assert_eq!(uniq(&[], ""), "");
        assert_eq!(uniq(&["-c"], "a\na\nb\n"), "      2 a\n      1 b\n");
        assert_eq!(uniq(&["-d"], "a\na\nb\nc\nc\n"), "a\nc\n");
        assert_eq!(uniq(&["-u"], "a\na\nb\nc\nc\n"), "b\n");
        assert_eq!(uniq(&["-du"], "a\na\nb\n"), "");
    }

    #[test]
    fn compares_the_part_asked_for() {
        assert_eq!(uniq(&["-i"], "Äb\näB\nc\n"), "Äb\nc\n");
        assert_eq!(uniq(&["-f", "1"], "1 x\n2 x\n3 y\n"), "1 x\n3 y\n");
        assert_eq!(uniq(&["-s", "2"], "aöx\nbux\n"), "aöx\n");
        assert_eq!(uniq(&["-s", "1"], "aöx\nböx\n"), "aöx\n");
        assert_eq!(uniq(&["-w", "2"], "abX\nabY\nacZ\n"), "abX\nacZ\n");
        assert_eq!(uniq(&["-f1", "-w2"], "  a bx\n\tc by\n"), "  a bx\n");
    }

    #[test]
    fn ends_lines_with_nul_under_z() {
        let merge = merge(&["-z"]);
        let mut merger = Merger::new(&merge);
        let mut out = Vec::new();
        for line in [&b"a"[..], b"a", b"b\n"] {
            merger.line(line, &mut out).unwrap();
        }
        merger.finish(&mut out).unwrap();
        assert_eq!(out, b"a\0b\n\0");
    }

    #[test]
    fn refuses_bad_numbers() {
        let Parsed::Run(run) = COMMAND.parse(["-f".to_string(), "x".into()]) else {
            panic!();
        };
        assert!(Merge::of(&run).is_err());
    }

    #[test]
    fn declares_what_it_takes() {
        let file = concat!(env!("CARGO_MANIFEST_DIR"), "/uniq.msh");
        mikros_cli::declared(&COMMAND, std::path::Path::new(file)).unwrap();
    }
}
