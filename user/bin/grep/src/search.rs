//! What grep takes, and how it selects the lines of one input and writes
//! them, with their context, names and numbers, or as JSON
//! (MIKSPEC-0098.R52).

use crate::pattern::{Matcher, Settings, Syntax};
use mikros_cli::{Kind, Operand, Opt, Paint, Role, Run, Spec, json_string};
use std::collections::VecDeque;
use std::io::{self, BufRead, Write};

pub static COMMAND: Spec = Spec {
    operands: &[
        Operand::optional(
            "pattern",
            Kind::Text,
            "the pattern, unless -e or -f gives it",
        ),
        Operand::any(
            "file",
            Kind::Path,
            "the files to search, or - for standard input",
        ),
    ],
    options: &[
        Opt::both("regexp", 'e', "a pattern to match").takes("pattern", Kind::Text),
        Opt::both("file", 'f', "read patterns from a file, one per line").takes("file", Kind::File),
        Opt::both(
            "extended-regexp",
            'E',
            "patterns are extended regular expressions",
        ),
        Opt::both("fixed-strings", 'F', "patterns are fixed strings"),
        Opt::both("ignore-case", 'i', "ignore the case of letters"),
        Opt::both("invert-match", 'v', "select the lines that do not match"),
        Opt::both("word-regexp", 'w', "match whole words only"),
        Opt::both("line-regexp", 'x', "match whole lines only"),
        Opt::both(
            "count",
            'c',
            "write the number of selected lines of each file",
        ),
        Opt::both(
            "files-with-matches",
            'l',
            "write the names of the files with selected lines",
        ),
        Opt::both(
            "files-without-match",
            'L',
            "write the names of the files without selected lines",
        ),
        Opt::both("line-number", 'n', "write the number of each line"),
        Opt::both("no-filename", 'h', "never write the names of files"),
        Opt::both(
            "with-filename",
            'H',
            "write the name of the file of each line",
        ),
        Opt::both(
            "only-matching",
            'o',
            "write only the matching parts of lines",
        ),
        Opt::both("quiet", 'q', "write nothing, and stop at the first match"),
        Opt::both(
            "no-messages",
            's',
            "say nothing of files that are missing or unreadable",
        ),
        Opt::both(
            "recursive",
            'r',
            "search folders, without following links in them",
        ),
        Opt::both(
            "dereference-recursive",
            'R',
            "search folders, following links in them",
        ),
        Opt::both("after-context", 'A', "write lines after each selected line")
            .takes("lines", Kind::Int),
        Opt::both(
            "before-context",
            'B',
            "write lines before each selected line",
        )
        .takes("lines", Kind::Int),
        Opt::both("context", 'C', "write lines around each selected line")
            .takes("lines", Kind::Int),
        Opt::long("include", "search only files whose names match").takes("glob", Kind::Text),
        Opt::long("exclude", "skip files whose names match").takes("glob", Kind::Text),
        Opt::both("max-count", 'm', "stop after this many selected lines")
            .takes("count", Kind::Int),
        Opt::both("null-data", 'z', "lines end with NUL, not line feed"),
        Opt::long(
            "json",
            "write the selected lines as a JSON array of objects",
        ),
    ],
    ..Spec::new(
        "grep",
        "Writes the lines of the files, or of standard input, that match a pattern:\n\
         a basic regular expression, an extended one with -E, or fixed strings\n\
         with -F. Exits with 0 when a line was selected, 1 when none was, and 2\n\
         on an error.",
    )
};

/// How lines are matched, from the options.
pub fn settings(run: &Run) -> Settings {
    let syntax = match run.last_of(&["-F", "-E"]) {
        Some("-F") => Syntax::Fixed,
        Some(_) => Syntax::Extended,
        None => Syntax::Basic,
    };
    Settings {
        syntax,
        ignore_case: run.has("-i"),
        word: run.has("-w"),
        line: run.has("-x"),
        null_data: run.has("-z"),
    }
}

/// What to do with the selected lines, from the options.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Options {
    pub invert: bool,
    pub count: bool,
    pub files_with: bool,
    pub files_without: bool,
    pub line_number: bool,
    /// Whether lines start with their file's name; the program decides.
    pub with_name: bool,
    pub only: bool,
    pub quiet: bool,
    pub json: bool,
    pub before: usize,
    pub after: usize,
    pub max: Option<u64>,
    /// The byte that ends lines.
    pub terminator: u8,
}

impl Options {
    /// The options of `run`, or why they are invalid.
    pub fn of(run: &Run) -> Result<Options, String> {
        let number = |name: &str| -> Result<Option<u64>, String> {
            match run.value(name) {
                None => Ok(None),
                Some(text) => text
                    .parse()
                    .map(Some)
                    .map_err(|_| format!("invalid number for {name}: {text}")),
            }
        };
        let context = number("-C")?;
        let lines = |n: Option<u64>| n.map(|n| usize::try_from(n).unwrap_or(usize::MAX));
        let options = Options {
            invert: run.has("-v"),
            count: run.has("-c"),
            files_with: run.has("-l"),
            files_without: run.has("-L"),
            line_number: run.has("-n"),
            with_name: false,
            only: run.has("-o"),
            quiet: run.has("-q"),
            json: run.has("--json"),
            before: lines(number("-B")?.or(context)).unwrap_or(0),
            after: lines(number("-A")?.or(context)).unwrap_or(0),
            max: number("-m")?,
            terminator: if run.has("-z") { 0 } else { b'\n' },
        };
        let lists = options.count || options.files_with || options.files_without;
        let context = run.has("-A") || run.has("-B") || run.has("-C");
        if options.json && (lists || context) {
            return Err("--json does not go with -c, -l, -L, -A, -B or -C".into());
        }
        Ok(options)
    }
}

/// How an input failed: reading it, or writing what it gave.
#[derive(Debug)]
pub enum Failed {
    Read(io::Error),
    Write(io::Error),
}

/// The colour codes that start and end `role` with `paint`, so that
/// bytes that are not UTF-8 can be painted.
fn codes(paint: Paint, role: Role) -> (String, String) {
    let painted = paint.paint(role, "\0");
    let (start, end) = painted.split_once('\0').unwrap_or(("", ""));
    (start.to_string(), end.to_string())
}

/// Writes `bytes` in the colour of `role`.
fn painted(out: &mut impl Write, paint: Paint, role: Role, bytes: &[u8]) -> io::Result<()> {
    let (start, end) = codes(paint, role);
    out.write_all(start.as_bytes())?;
    out.write_all(bytes)?;
    out.write_all(end.as_bytes())
}

/// One input as it is searched.
pub struct Input<'a> {
    /// Its name in text, such as `(standard input)`.
    pub name: &'a str,
    /// Its path in JSON, such as `-`.
    pub path: &'a str,
}

/// Writes the lines of one input that the search selects, and the
/// context around them.
struct Writer<'a, W: Write> {
    out: &'a mut W,
    options: &'a Options,
    matcher: &'a Matcher,
    paint: Paint,
    name: &'a str,
}

impl<W: Write> Writer<'_, W> {
    fn prefix(&mut self, number: u64, separator: &[u8]) -> io::Result<()> {
        if self.options.with_name {
            painted(self.out, self.paint, Role::Path, self.name.as_bytes())?;
            self.out.write_all(separator)?;
        }
        if self.options.line_number {
            let number = number.to_string();
            painted(self.out, self.paint, Role::Number, number.as_bytes())?;
            self.out.write_all(separator)?;
        }
        Ok(())
    }

    fn context(&mut self, number: u64, line: &[u8]) -> io::Result<()> {
        self.prefix(number, b"-")?;
        self.out.write_all(line)?;
        self.out.write_all(&[self.options.terminator])
    }

    fn selected(&mut self, number: u64, line: &[u8]) -> io::Result<()> {
        let terminator = self.options.terminator;
        if self.options.only {
            if self.options.invert {
                return Ok(());
            }
            for (start, end) in self.matcher.matches(line) {
                self.prefix(number, b":")?;
                painted(self.out, self.paint, Role::String, &line[start..end])?;
                self.out.write_all(&[terminator])?;
            }
            return Ok(());
        }
        self.prefix(number, b":")?;
        if !self.paint.on() || self.options.invert {
            self.out.write_all(line)?;
        } else {
            let mut at = 0;
            for (start, end) in self.matcher.matches(line) {
                self.out.write_all(&line[at..start])?;
                painted(self.out, self.paint, Role::String, &line[start..end])?;
                at = end;
            }
            self.out.write_all(&line[at..])?;
        }
        self.out.write_all(&[terminator])
    }
}

/// The JSON objects of a selected line: one for the line, or with `-o`
/// one for each match.
fn objects(input: &Input, number: u64, line: &[u8], matcher: &Matcher, o: &Options) -> Vec<String> {
    let object = |text: &[u8]| {
        format!(
            "{{\"path\":{},\"line\":{number},\"text\":{}}}",
            json_string(input.path),
            json_string(&String::from_utf8_lossy(text))
        )
    };
    match (o.only, o.invert) {
        (false, _) => vec![object(line)],
        (true, true) => Vec::new(),
        (true, false) => matcher
            .matches(line)
            .into_iter()
            .map(|(s, e)| object(&line[s..e]))
            .collect(),
    }
}

/// Searches `reader`, writing the selected lines and their context to
/// `out`, or adding their JSON objects to `json`, and returns how many
/// lines it selected. With `-q`, `-l` and `-L` it stops at the first.
pub fn search(
    reader: &mut impl BufRead,
    input: &Input,
    matcher: &Matcher,
    o: &Options,
    paint: Paint,
    out: &mut impl Write,
    json: &mut Vec<String>,
) -> Result<u64, Failed> {
    if o.max == Some(0) {
        return Ok(0);
    }
    let listing = o.count || o.files_with || o.files_without || o.quiet;
    let shown = !listing && !o.json;
    let (before, after) = match shown && !o.only {
        true => (o.before, o.after),
        false => (0, 0),
    };
    let mut writer = Writer {
        out,
        options: o,
        matcher,
        paint,
        name: input.name,
    };
    let mut kept: VecDeque<(u64, Vec<u8>)> = VecDeque::new();
    let mut last: Option<u64> = None;
    let mut left = 0;
    let mut done = false;
    let mut selected = 0;
    let mut number = 0;
    let mut buffer = Vec::new();
    loop {
        buffer.clear();
        if reader
            .read_until(o.terminator, &mut buffer)
            .map_err(Failed::Read)?
            == 0
        {
            break;
        }
        number += 1;
        let line = buffer.strip_suffix(&[o.terminator]).unwrap_or(&buffer);
        let selects = !done && matcher.is_match(line) != o.invert;
        if selects {
            selected += 1;
            if o.quiet || o.files_with || o.files_without {
                return Ok(selected);
            }
            if o.json {
                json.extend(objects(input, number, line, matcher, o));
            } else if shown {
                let first = kept.front().map_or(number, |(n, _)| *n);
                if (before > 0 || after > 0) && last.is_some_and(|l| first > l + 1) {
                    writer.out.write_all(b"--\n").map_err(Failed::Write)?;
                }
                for (n, l) in kept.drain(..) {
                    writer.context(n, &l).map_err(Failed::Write)?;
                }
                writer.selected(number, line).map_err(Failed::Write)?;
                last = Some(number);
                left = after;
            }
            if o.max.is_some_and(|m| selected >= m) {
                done = true;
                if left == 0 {
                    break;
                }
            }
        } else if left > 0 {
            writer.context(number, line).map_err(Failed::Write)?;
            last = Some(number);
            left -= 1;
            if done && left == 0 {
                break;
            }
        } else if before > 0 {
            kept.push_back((number, line.to_vec()));
            if kept.len() > before {
                kept.pop_front();
            }
        }
    }
    Ok(selected)
}

/// The line of `-c` for an input with `count` selected lines.
pub fn count_line(name: &str, count: u64, o: &Options, paint: Paint) -> String {
    let count = paint.paint(Role::Number, &count.to_string());
    match o.with_name {
        true => format!("{}:{count}\n", paint.paint(Role::Path, name)),
        false => format!("{count}\n"),
    }
}

/// Whether `name` matches the glob `pattern`, with `*`, `?`, bracket
/// expressions with `!` or `^` and ranges, and `\` before a character
/// that stands for itself.
pub fn glob(pattern: &str, name: &str) -> bool {
    let p: Vec<char> = pattern.chars().collect();
    let n: Vec<char> = name.chars().collect();
    let (mut pi, mut ni) = (0, 0);
    // Where to go back to after the last `*`: the pattern after it, and
    // the name it has taken up to.
    let mut star: Option<(usize, usize)> = None;
    while ni < n.len() {
        let step = match p.get(pi) {
            Some('*') => {
                star = Some((pi + 1, ni));
                pi += 1;
                continue;
            }
            Some('?') => Some(pi + 1),
            Some('[') => bracket(&p, pi, n[ni]),
            Some('\\') if pi + 1 < p.len() => (p[pi + 1] == n[ni]).then_some(pi + 2),
            Some(&c) => (c == n[ni]).then_some(pi + 1),
            None => None,
        };
        match (step, star) {
            (Some(next), _) => {
                pi = next;
                ni += 1;
            }
            (None, Some((after, taken))) => {
                pi = after;
                ni = taken + 1;
                star = Some((after, taken + 1));
            }
            (None, None) => return false,
        }
    }
    p[pi..].iter().all(|&c| c == '*')
}

/// Where the glob's bracket expression at `at` ends when it holds `c`;
/// a `[` without its `]` stands for itself.
fn bracket(p: &[char], at: usize, c: char) -> Option<usize> {
    let mut i = at + 1;
    let negated = matches!(p.get(i), Some('!' | '^'));
    if negated {
        i += 1;
    }
    let mut holds = false;
    let mut first = true;
    loop {
        let &start = match p.get(i) {
            Some(s) => s,
            None => return (c == '[').then_some(at + 1),
        };
        if start == ']' && !first {
            return (holds != negated).then_some(i + 1);
        }
        first = false;
        if p.get(i + 1) == Some(&'-') && p.get(i + 2).is_some_and(|&e| e != ']') {
            holds |= (start..=p[i + 2]).contains(&c);
            i += 3;
        } else {
            holds |= start == c;
            i += 1;
        }
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
    fn takes_the_later_of_e_and_f() {
        assert!(matches!(
            settings(&run(&["-E", "-F"])).syntax,
            Syntax::Fixed
        ));
        assert!(matches!(settings(&run(&["-FE"])).syntax, Syntax::Extended));
        assert!(matches!(settings(&run(&[])).syntax, Syntax::Basic));
    }

    /// What grep writes for `text` with `arguments`, whose first operand
    /// is the pattern, and how many lines it selects.
    fn grep(arguments: &[&str], text: &str, name: bool, paint: Paint) -> (String, u64) {
        let run = run(arguments);
        let matcher = Matcher::new(&run.operands[..1], settings(&run)).unwrap();
        let mut options = Options::of(&run).unwrap();
        options.with_name = name;
        let input = Input {
            name: "f",
            path: "f",
        };
        let mut out = Vec::new();
        let mut json = Vec::new();
        let selected = search(
            &mut text.as_bytes(),
            &input,
            &matcher,
            &options,
            paint,
            &mut out,
            &mut json,
        )
        .unwrap();
        if options.json {
            out = format!("[{}]", json.join(",")).into_bytes();
        }
        (String::from_utf8(out).unwrap(), selected)
    }

    fn plain(arguments: &[&str], text: &str) -> String {
        grep(arguments, text, false, Paint::never()).0
    }

    const TEXT: &str = "one\ntwo\nthree\nfour\nfive\nsix\nseven\n";

    #[test]
    fn selects_lines() {
        assert_eq!(plain(&["o"], TEXT), "one\ntwo\nfour\n");
        assert_eq!(plain(&["-v", "e"], TEXT), "two\nfour\nsix\n");
        assert_eq!(plain(&["-n", "^t"], TEXT), "2:two\n3:three\n");
        assert_eq!(plain(&["-m", "1", "o"], TEXT), "one\n");
        assert_eq!(plain(&["-ix", "ONE"], TEXT), "one\n");
        assert_eq!(
            plain(&["-o", "e."], "seven eleven\n"),
            "ev\nen\nel\nev\nen\n"
        );
        assert_eq!(plain(&["x"], "no end"), "");
        assert_eq!(plain(&["end"], "no end"), "no end\n");
        assert_eq!(plain(&["-z", "b"], "a\nb\0c\0"), "a\nb\0".to_string());
        assert_eq!(
            grep(&["-c", "o"], TEXT, false, Paint::never()),
            (String::new(), 3)
        );
        assert_eq!(grep(&["-q", "e"], TEXT, false, Paint::never()).1, 1);
    }

    #[test]
    fn writes_context_between_separators() {
        assert_eq!(
            plain(&["-n", "-C", "1", "^t\\|six"], TEXT),
            "1-one\n2:two\n3:three\n4-four\n5-five\n6:six\n7-seven\n"
        );
        assert_eq!(
            plain(&["-A", "1", "one\\|five"], TEXT),
            "one\ntwo\n--\nfive\nsix\n"
        );
        assert_eq!(plain(&["-B", "2", "four"], TEXT), "two\nthree\nfour\n");
        assert_eq!(
            plain(&["-m", "1", "-A", "2", "o"], TEXT),
            "one\ntwo\nthree\n"
        );
    }

    #[test]
    fn writes_names_and_colours() {
        let (text, _) = grep(&["-n", "wo"], TEXT, true, Paint::never());
        assert_eq!(text, "f:2:two\n");
        let on = Paint::new(Some("always"), false, None);
        let (text, _) = grep(&["wo"], "two\n", true, on);
        let path = on.paint(Role::Path, "f");
        let found = on.paint(Role::String, "wo");
        assert_eq!(text, format!("{path}:t{found}\n"));
        let options = Options {
            with_name: true,
            ..Options::default()
        };
        assert_eq!(count_line("f", 3, &options, Paint::never()), "f:3\n");
        assert_eq!(
            count_line("f", 3, &Options::default(), Paint::never()),
            "3\n"
        );
    }

    #[test]
    fn writes_json() {
        assert_eq!(
            plain(&["--json", "t\"\\?w"], "a\ntwo\nt\"w\n"),
            "[{\"path\":\"f\",\"line\":2,\"text\":\"two\"},\
             {\"path\":\"f\",\"line\":3,\"text\":\"t\\\"w\"}]"
        );
        assert_eq!(
            plain(&["--json", "-o", "o"], "foo\n"),
            "[{\"path\":\"f\",\"line\":1,\"text\":\"o\"},{\"path\":\"f\",\"line\":1,\"text\":\"o\"}]"
        );
        assert!(Options::of(&run(&["--json", "-c", "x"])).is_err());
        assert!(Options::of(&run(&["-A", "x", "y"])).is_err());
    }

    #[test]
    fn matches_globs() {
        assert!(glob("*.rs", "main.rs"));
        assert!(!glob("*.rs", "main.rsx"));
        assert!(glob("a?c", "abc") && !glob("a?c", "ac"));
        assert!(glob("[a-c]x*", "bxyz") && !glob("[!a-c]x", "bx"));
        assert!(glob("*a*b*", "xxaxxbxx") && !glob("*a*b", "ba"));
        assert!(glob("\\*", "*") && !glob("\\*", "x"));
        assert!(glob("[", "[") && glob("", "") && glob("*", ""));
    }

    #[test]
    fn declares_what_it_takes() {
        let file = concat!(env!("CARGO_MANIFEST_DIR"), "/grep.msh");
        mikros_cli::declared(&COMMAND, std::path::Path::new(file)).unwrap();
    }
}
