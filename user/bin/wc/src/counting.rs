//! What wc takes, how it counts the bytes of a file as UTF-8, and how it
//! writes the counts as columns or JSON (MIKSPEC-0098.R47).

use mikros_cli::{Kind, Operand, Opt, Paint, Role, Run, Spec, json_string};

pub static COMMAND: Spec = Spec {
    operands: &[Operand::any(
        "file",
        Kind::File,
        "the files to count, or - for standard input",
    )],
    options: &[
        Opt::both("lines", 'l', "count lines"),
        Opt::both("words", 'w', "count words"),
        Opt::both("chars", 'm', "count characters"),
        Opt::both("bytes", 'c', "count bytes"),
        Opt::both("max-line-length", 'L', "show the width of the longest line"),
        Opt::long("json", "write the counts as a JSON array of objects"),
    ],
    ..Spec::new(
        "wc",
        "Counts the lines, words and bytes of each file, or of standard input,\n\
         with a total when there are several.",
    )
};

/// The counts of one file, or the total.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Counts {
    pub lines: u64,
    pub words: u64,
    pub chars: u64,
    pub bytes: u64,
    /// The width of the longest line.
    pub longest: u64,
}

impl Counts {
    /// The total of `self` and `other`, with the longer longest line.
    pub fn add(&mut self, other: &Counts) {
        self.lines += other.lines;
        self.words += other.words;
        self.chars += other.chars;
        self.bytes += other.bytes;
        self.longest = self.longest.max(other.longest);
    }
}

/// Counts bytes given in pieces, decoding them as UTF-8 across the
/// pieces' ends.
#[derive(Debug, Default)]
pub struct Counter {
    counts: Counts,
    in_word: bool,
    /// The width of the line so far.
    width: u64,
    /// The start of a character that the last piece ended in.
    carry: Vec<u8>,
}

impl Counter {
    /// Counts `bytes`, the next piece.
    pub fn feed(&mut self, bytes: &[u8]) {
        if self.carry.is_empty() {
            let cut = bytes.len() - incomplete(bytes);
            self.carry = bytes[cut..].to_vec();
            self.count(&bytes[..cut]);
            return;
        }
        let mut joined = std::mem::take(&mut self.carry);
        joined.extend_from_slice(bytes);
        let cut = joined.len() - incomplete(&joined);
        self.carry = joined.split_off(cut);
        self.count(&joined);
    }

    /// The counts, after the last piece.
    pub fn finish(mut self) -> Counts {
        let carry = std::mem::take(&mut self.carry);
        self.count(&carry);
        self.counts
    }

    fn count(&mut self, bytes: &[u8]) {
        self.counts.bytes += bytes.len() as u64;
        for chunk in bytes.utf8_chunks() {
            for c in chunk.valid().chars() {
                self.char(c);
            }
            // Bytes that are not UTF-8 are part of words, and are not
            // characters.
            if !chunk.invalid().is_empty() {
                if !self.in_word {
                    self.counts.words += 1;
                    self.in_word = true;
                }
                self.width += chunk.invalid().len() as u64;
            }
        }
        self.counts.longest = self.counts.longest.max(self.width);
    }

    fn char(&mut self, c: char) {
        self.counts.chars += 1;
        match c {
            '\n' => {
                self.counts.lines += 1;
                self.counts.longest = self.counts.longest.max(self.width);
                self.width = 0;
            }
            '\t' => self.width += 8 - self.width % 8,
            '\r' | '\x0c' => {
                self.counts.longest = self.counts.longest.max(self.width);
                self.width = 0;
            }
            c if c.is_control() => {}
            _ => self.width += 1,
        }
        if separates(c) {
            self.in_word = false;
        } else if !self.in_word {
            self.counts.words += 1;
            self.in_word = true;
        }
    }
}

/// Whether `c` separates words: white space, except the spaces that do
/// not break.
fn separates(c: char) -> bool {
    c.is_whitespace() && !matches!(c, '\u{a0}' | '\u{2007}' | '\u{202f}')
}

/// How many bytes at the end of `bytes` start a character that is not
/// complete yet.
fn incomplete(bytes: &[u8]) -> usize {
    for back in 1..=bytes.len().min(3) {
        let b = bytes[bytes.len() - back];
        if b & 0xc0 == 0x80 {
            continue;
        }
        let needs = match b {
            0xc0..=0xdf => 2,
            0xe0..=0xef => 3,
            0xf0..=0xf7 => 4,
            _ => 1,
        };
        return if needs > back { back } else { 0 };
    }
    0
}

/// Which counts to show, in the order wc writes them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Show {
    pub lines: bool,
    pub words: bool,
    pub chars: bool,
    pub bytes: bool,
    pub longest: bool,
}

impl Show {
    /// The counts asked for, or lines, words and bytes when none is.
    pub fn of(run: &Run) -> Show {
        let show = Show {
            lines: run.has("-l"),
            words: run.has("-w"),
            chars: run.has("-m"),
            bytes: run.has("-c"),
            longest: run.has("-L"),
        };
        match show.lines || show.words || show.chars || show.bytes || show.longest {
            true => show,
            false => Show {
                lines: true,
                words: true,
                bytes: true,
                ..show
            },
        }
    }

    fn values(self, counts: &Counts) -> Vec<u64> {
        [
            (self.lines, counts.lines),
            (self.words, counts.words),
            (self.chars, counts.chars),
            (self.bytes, counts.bytes),
            (self.longest, counts.longest),
        ]
        .into_iter()
        .filter_map(|(shown, value)| shown.then_some(value))
        .collect()
    }
}

/// The lines of `rows`, each counts and a name, which standard input
/// read without a name lacks: the counts shown, right-aligned to the
/// widest, and the name.
pub fn text(rows: &[(Option<String>, Counts)], show: Show, paint: Paint) -> String {
    let values: Vec<Vec<u64>> = rows.iter().map(|(_, c)| show.values(c)).collect();
    let width = values
        .iter()
        .flatten()
        .map(|v| v.to_string().len())
        .max()
        .unwrap_or(1);
    let mut text = String::new();
    for ((name, _), values) in rows.iter().zip(&values) {
        let shown: Vec<String> = values
            .iter()
            .map(|v| paint.paint(Role::Number, &format!("{v:>width$}")))
            .collect();
        text.push_str(&shown.join(" "));
        if let Some(name) = name {
            text.push(' ');
            text.push_str(&paint.paint(Role::Path, name));
        }
        text.push('\n');
    }
    text
}

/// The counts of `rows` as a JSON array of objects with `path`, `lines`,
/// `words`, `bytes` and `characters`, on one line.
pub fn json(rows: &[(String, Counts)]) -> String {
    let objects: Vec<String> = rows
        .iter()
        .map(|(path, c)| {
            format!(
                "{{\"path\":{},\"lines\":{},\"words\":{},\"bytes\":{},\"characters\":{}}}",
                json_string(path),
                c.lines,
                c.words,
                c.bytes,
                c.chars
            )
        })
        .collect();
    format!("[{}]\n", objects.join(","))
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

    fn count(pieces: &[&[u8]]) -> Counts {
        let mut counter = Counter::default();
        for piece in pieces {
            counter.feed(piece);
        }
        counter.finish()
    }

    #[test]
    fn counts_lines_words_characters_and_bytes() {
        let c = count(&["one two\n  three\nfour".as_bytes()]);
        assert_eq!(
            c,
            Counts {
                lines: 2,
                words: 4,
                chars: 20,
                bytes: 20,
                longest: 7
            }
        );
        let c = count(&["grüße\tx\n".as_bytes()]);
        assert_eq!((c.chars, c.bytes, c.words, c.longest), (8, 10, 2, 9));
        assert_eq!(count(&[]), Counts::default());
    }

    #[test]
    fn decodes_characters_split_between_pieces() {
        let text = "aé€😀 b".as_bytes();
        for cut in 0..text.len() {
            let c = count(&[&text[..cut], &text[cut..]]);
            assert_eq!((c.chars, c.bytes, c.words), (6, 12, 2), "cut at {cut}");
        }
    }

    #[test]
    fn counts_bytes_that_are_not_utf8_in_words_only() {
        let c = count(&[b"a\xff b \xe2\x82"]);
        assert_eq!((c.chars, c.bytes, c.words), (4, 7, 3));
        let c = count(&["x\u{a0}y".as_bytes()]);
        assert_eq!(c.words, 1);
    }

    #[test]
    fn shows_what_was_asked_in_order() {
        let c = Counts {
            lines: 3,
            words: 10,
            chars: 50,
            bytes: 120,
            longest: 9,
        };
        let rows = vec![(Some("a".to_string()), c), (Some("total".into()), c)];
        assert_eq!(
            text(&rows, Show::of(&run(&[])), Paint::never()),
            "  3  10 120 a\n  3  10 120 total\n"
        );
        assert_eq!(
            text(&[(None, c)], Show::of(&run(&["-L", "-ml"])), Paint::never()),
            " 3 50  9\n"
        );
        assert_eq!(
            text(&[(None, c)], Show::of(&run(&["-c"])), Paint::never()),
            "120\n"
        );
    }

    #[test]
    fn writes_json() {
        let c = Counts {
            lines: 1,
            words: 2,
            chars: 3,
            bytes: 4,
            longest: 5,
        };
        assert_eq!(
            json(&[("a b".into(), c)]),
            "[{\"path\":\"a b\",\"lines\":1,\"words\":2,\"bytes\":4,\"characters\":3}]\n"
        );
        assert_eq!(json(&[]), "[]\n");
    }

    #[test]
    fn declares_what_it_takes() {
        let file = concat!(env!("CARGO_MANIFEST_DIR"), "/wc.msh");
        mikros_cli::declared(&COMMAND, std::path::Path::new(file)).unwrap();
    }
}
