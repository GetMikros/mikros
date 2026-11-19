//! What cut takes, its lists of positions, and how it cuts a line into
//! bytes, characters or fields and writes those selected
//! (MIKSPEC-0098.R49).

use mikros_cli::{Kind, Operand, Opt, Run, Spec};

pub static COMMAND: Spec = Spec {
    operands: &[Operand::any(
        "file",
        Kind::File,
        "the files to cut, or - for standard input",
    )],
    options: &[
        Opt::both("bytes", 'b', "select the bytes of the list").takes("list", Kind::Text),
        Opt::both("characters", 'c', "select the characters of the list").takes("list", Kind::Text),
        Opt::both("fields", 'f', "select the fields of the list").takes("list", Kind::Text),
        Opt::both(
            "delimiter",
            'd',
            "the character between fields, a tab when absent",
        )
        .takes("delimiter", Kind::Text),
        Opt::both(
            "only-delimited",
            's',
            "leave out lines without the delimiter",
        ),
        Opt::long("complement", "select what the list does not"),
        Opt::long(
            "output-delimiter",
            "write this between the fields or ranges written",
        )
        .takes("text", Kind::Text),
    ],
    ..Spec::new(
        "cut",
        "Writes the selected bytes, characters or fields of each line of the\n\
         files, or of standard input. A list is positions from 1 and ranges,\n\
         separated by commas, such as 1,3-5, -3 or 4-.",
    )
};

/// The positions of a list, from 1: ranges, ordered and joined, with
/// `usize::MAX` for an open end.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct List(Vec<(usize, usize)>);

impl List {
    /// The list `text`, or why it is not one.
    pub fn parse(text: &str) -> Result<List, String> {
        let invalid = |why: &str| Err(format!("invalid list {text}: {why}"));
        let mut ranges = Vec::new();
        for item in text.split(',') {
            let number = |n: &str| n.parse::<usize>().ok().filter(|n| *n > 0);
            let range = match item.split_once('-') {
                None => number(item).map(|n| (n, n)),
                Some(("", "")) => None,
                Some(("", end)) => number(end).map(|e| (1, e)),
                Some((start, "")) => number(start).map(|s| (s, usize::MAX)),
                Some((start, end)) => number(start).zip(number(end)),
            };
            match range {
                Some((start, end)) if start <= end => ranges.push((start, end)),
                Some(_) => return invalid("a range ends before it starts"),
                None => return invalid("positions are numbers from 1"),
            }
        }
        ranges.sort();
        let mut joined: Vec<(usize, usize)> = Vec::new();
        for (start, end) in ranges {
            match joined.last_mut() {
                Some(last) if start <= last.1.saturating_add(1) => last.1 = last.1.max(end),
                _ => joined.push((start, end)),
            }
        }
        Ok(List(joined))
    }

    /// Whether position `n`, from 1, is in the list.
    pub fn has(&self, n: usize) -> bool {
        self.0
            .iter()
            .any(|(start, end)| (*start..=*end).contains(&n))
    }
}

/// What a line is cut into.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Unit {
    Bytes,
    Characters,
    Fields,
}

/// How lines are cut, from the options.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Cut {
    pub unit: Unit,
    pub list: List,
    pub complement: bool,
    pub delimiter: Vec<u8>,
    pub only_delimited: bool,
    /// What is written between the fields or ranges written; for fields,
    /// the delimiter when absent.
    pub output: Option<Vec<u8>>,
}

impl Cut {
    /// How `run` asks lines to be cut, or why its arguments are invalid.
    pub fn of(run: &Run) -> Result<Cut, String> {
        let lists: Vec<(Unit, &str)> = [
            (Unit::Bytes, "-b"),
            (Unit::Characters, "-c"),
            (Unit::Fields, "-f"),
        ]
        .into_iter()
        .filter_map(|(unit, option)| run.value(option).map(|list| (unit, list)))
        .collect();
        let (unit, list) = match lists.as_slice() {
            [] => return Err("give a list of bytes, characters or fields".into()),
            [one] => *one,
            _ => return Err("give only one list".into()),
        };
        if unit != Unit::Fields && (run.has("-d") || run.has("-s")) {
            return Err("-d and -s apply only to fields".into());
        }
        let delimiter = match run.value("-d") {
            None => vec![b'\t'],
            Some(d) if d.chars().count() == 1 => d.as_bytes().to_vec(),
            Some(d) => return Err(format!("the delimiter must be one character, not {d}")),
        };
        Ok(Cut {
            unit,
            list: List::parse(list)?,
            complement: run.has("--complement"),
            delimiter,
            only_delimited: run.has("-s"),
            output: run
                .value("--output-delimiter")
                .map(|o| o.as_bytes().to_vec()),
        })
    }

    fn selects(&self, n: usize) -> bool {
        self.list.has(n) != self.complement
    }

    /// Writes the selected parts of `line`, without its line feed, and a
    /// line feed to `out`; nothing for a line without the delimiter under
    /// `-s`.
    pub fn line(&self, line: &[u8], out: &mut Vec<u8>) {
        match self.unit {
            Unit::Bytes => {
                let items: Vec<&[u8]> = line.chunks(1).collect();
                self.items(&items, out);
            }
            Unit::Characters => self.items(&characters(line), out),
            Unit::Fields => {
                let fields = split(line, &self.delimiter);
                if fields.len() == 1 {
                    if !self.only_delimited {
                        out.extend_from_slice(line);
                        out.push(b'\n');
                    }
                    return;
                }
                let between = self.output.as_deref().unwrap_or(&self.delimiter);
                let mut first = true;
                for (i, field) in fields.iter().enumerate() {
                    if self.selects(i + 1) {
                        if !first {
                            out.extend_from_slice(between);
                        }
                        out.extend_from_slice(field);
                        first = false;
                    }
                }
                out.push(b'\n');
            }
        }
    }

    /// Writes the selected bytes or characters `items`, with the output
    /// delimiter between runs that are not next to each other.
    fn items(&self, items: &[&[u8]], out: &mut Vec<u8>) {
        let mut written = false;
        let mut previous = false;
        for (i, item) in items.iter().enumerate() {
            let selected = self.selects(i + 1);
            if selected {
                if let Some(between) = &self.output
                    && written
                    && !previous
                {
                    out.extend_from_slice(between);
                }
                out.extend_from_slice(item);
                written = true;
            }
            previous = selected;
        }
        out.push(b'\n');
    }
}

/// The characters of `line` as UTF-8, with each byte that is not UTF-8 as
/// one.
fn characters(line: &[u8]) -> Vec<&[u8]> {
    let mut items = Vec::new();
    let mut at = 0;
    for chunk in line.utf8_chunks() {
        for c in chunk.valid().chars() {
            items.push(&line[at..at + c.len_utf8()]);
            at += c.len_utf8();
        }
        for _ in chunk.invalid() {
            items.push(&line[at..at + 1]);
            at += 1;
        }
    }
    items
}

/// The parts of `line` between occurrences of `delimiter`.
fn split<'a>(line: &'a [u8], delimiter: &[u8]) -> Vec<&'a [u8]> {
    let mut parts = Vec::new();
    let mut start = 0;
    let mut i = 0;
    while i + delimiter.len() <= line.len() {
        if &line[i..i + delimiter.len()] == delimiter {
            parts.push(&line[start..i]);
            i += delimiter.len();
            start = i;
        } else {
            i += 1;
        }
    }
    parts.push(&line[start..]);
    parts
}

#[cfg(test)]
mod tests {
    use super::*;
    use mikros_cli::Parsed;

    fn cut(arguments: &[&str], line: &str) -> Result<String, String> {
        let run = match COMMAND.parse(arguments.iter().map(|a| a.to_string())) {
            Parsed::Run(run) => run,
            other => panic!("{other:?}"),
        };
        let cut = Cut::of(&run)?;
        let mut out = Vec::new();
        cut.line(line.as_bytes(), &mut out);
        Ok(String::from_utf8(out).unwrap())
    }

    #[test]
    fn reads_lists() {
        assert_eq!(List::parse("1,3-5").unwrap(), List(vec![(1, 1), (3, 5)]));
        assert_eq!(
            List::parse("5-,-2,2-3").unwrap(),
            List(vec![(1, 3), (5, usize::MAX)])
        );
        assert_eq!(List::parse("4-,-3").unwrap(), List(vec![(1, usize::MAX)]));
        assert!(List::parse("0").is_err());
        assert!(List::parse("3-1").is_err());
        assert!(List::parse("a").is_err());
        assert!(List::parse("1,,2").is_err());
        assert!(List::parse("-").is_err());
    }

    #[test]
    fn cuts_bytes_and_characters() {
        assert_eq!(cut(&["-b", "1,3-4"], "abcdef").unwrap(), "acd\n");
        assert_eq!(cut(&["-b", "-2"], "äb").unwrap(), "ä\n");
        assert_eq!(cut(&["-c", "2-3"], "äöüx").unwrap(), "öü\n");
        assert_eq!(cut(&["-c", "2", "--complement"], "äöü").unwrap(), "äü\n");
        assert_eq!(
            cut(&["-c", "1-2,4-", "--output-delimiter", ":"], "abcdef").unwrap(),
            "ab:def\n"
        );
        assert_eq!(
            cut(&["-c1-2,3", "--output-delimiter=:"], "abcd").unwrap(),
            "abc\n"
        );
        assert_eq!(cut(&["-b", "9"], "abc").unwrap(), "\n");
    }

    #[test]
    fn cuts_fields() {
        assert_eq!(cut(&["-f", "2"], "a\tb\tc").unwrap(), "b\n");
        assert_eq!(cut(&["-d", ":", "-f", "1,3"], "a:b:c:d").unwrap(), "a:c\n");
        assert_eq!(
            cut(&["-d:", "-f2-", "--output-delimiter", " "], "a:b:c").unwrap(),
            "b c\n"
        );
        assert_eq!(
            cut(&["-d", ":", "-f", "1", "--complement"], "a:b:c").unwrap(),
            "b:c\n"
        );
        assert_eq!(cut(&["-d", "→", "-f", "2"], "a→b→c").unwrap(), "b\n");
        assert_eq!(cut(&["-d", ":", "-f", "2"], "plain").unwrap(), "plain\n");
        assert_eq!(cut(&["-s", "-d", ":", "-f", "2"], "plain").unwrap(), "");
        assert_eq!(cut(&["-d", ":", "-f", "5"], "a:b").unwrap(), "\n");
    }

    #[test]
    fn refuses_bad_combinations() {
        assert!(cut(&[], "x").is_err());
        assert!(cut(&["-b1", "-f1"], "x").is_err());
        assert!(cut(&["-b1", "-d:"], "x").is_err());
        assert!(cut(&["-f1", "-d", "ab"], "x").is_err());
        assert!(cut(&["-f", "x"], "x").is_err());
    }

    #[test]
    fn declares_what_it_takes() {
        let file = concat!(env!("CARGO_MANIFEST_DIR"), "/cut.msh");
        mikros_cli::declared(&COMMAND, std::path::Path::new(file)).unwrap();
    }
}
