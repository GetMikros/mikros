//! What sort takes, its keys, and how it compares lines: by bytes, which
//! orders UTF-8 by code point, or as numbers, sizes or versions, by
//! fields and characters (MIKSPEC-0098.R48).

use mikros_cli::{Kind, Operand, Opt, Run, Spec};
use std::cmp::Ordering;

pub static COMMAND: Spec = Spec {
    operands: &[Operand::any(
        "file",
        Kind::File,
        "the files to sort, or - for standard input",
    )],
    options: &[
        Opt::both("numeric-sort", 'n', "compare as decimal numbers"),
        Opt::both(
            "human-numeric-sort",
            'h',
            "compare as sizes, such as 2K and 1G",
        ),
        Opt::both("version-sort", 'V', "compare as version numbers"),
        Opt::both("reverse", 'r', "reverse the order"),
        Opt::both("unique", 'u', "write only the first of equal lines"),
        Opt::both("ignore-case", 'f', "compare without regard to case"),
        Opt::both(
            "ignore-leading-blanks",
            'b',
            "ignore blanks at the start of keys",
        ),
        Opt::both("stable", 's', "keep equal lines in their order"),
        Opt::both("merge", 'm', "merge files that are already sorted"),
        Opt::both("check", 'c', "check that the input is sorted"),
        Opt::both(
            "key",
            'k',
            "compare by a key: field[.char][options][,field[.char][options]]",
        )
        .takes("key", Kind::Text),
        Opt::both(
            "field-separator",
            't',
            "the character between fields, blanks when absent",
        )
        .takes("separator", Kind::Text),
        Opt::both("output", 'o', "write to this file").takes("file", Kind::File),
        Opt::both("zero-terminated", 'z', "lines end with a NUL byte"),
    ],
    ..Spec::new(
        "sort",
        "Sorts the lines of the files, or of standard input, by their bytes,\n\
         which orders UTF-8 by code point. Without a separator, a field is\n\
         blanks and the characters that follow them up to a blank. A key's\n\
         options are b, f, h, n, r and V.",
    )
};

/// How a key compares.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Order {
    pub numeric: bool,
    pub human: bool,
    pub version: bool,
    pub fold: bool,
    pub reverse: bool,
    /// Skip blanks before the start, and before the end's character.
    pub blanks_start: bool,
    pub blanks_end: bool,
}

impl Order {
    fn plain(&self) -> bool {
        *self == Order::default()
    }

    /// Sets the option letter `c`, for the start of a key when `start`.
    fn set(&mut self, c: char, start: bool) -> bool {
        match c {
            'b' if start => self.blanks_start = true,
            'b' => self.blanks_end = true,
            'f' => self.fold = true,
            'h' => self.human = true,
            'n' => self.numeric = true,
            'r' => self.reverse = true,
            'V' => self.version = true,
            _ => return false,
        }
        true
    }
}

/// A key: from character `start.1` of field `start.0`, both from 1, to
/// character `end.1` of field `end.0`, or the end of that field when it
/// is 0, or the end of the line without an end.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Key {
    pub start: (usize, usize),
    pub end: Option<(usize, usize)>,
    pub order: Order,
}

impl Key {
    /// The key `text`, with `global` for one without options of its own.
    pub fn parse(text: &str, global: Order) -> Result<Key, String> {
        let invalid = |why: &str| Err(format!("invalid key {text}: {why}"));
        let mut order = Order::default();
        let (start, end) = match text.split_once(',') {
            Some((start, end)) => (start, Some(end)),
            None => (text, None),
        };
        let mut position = |part: &str, start: bool| -> Result<(usize, usize), String> {
            let digits = |s: &str| s.find(|c: char| !c.is_ascii_digit()).unwrap_or(s.len());
            let n = digits(part);
            let field: usize = part[..n].parse().map_err(|_| "a field is a number")?;
            let mut rest = &part[n..];
            let mut char = if start { 1 } else { 0 };
            if let Some(after) = rest.strip_prefix('.') {
                let n = digits(after);
                char = after[..n].parse().map_err(|_| "a character is a number")?;
                rest = &after[n..];
            }
            if field == 0 || (start && char == 0) {
                return Err("fields and characters count from 1".into());
            }
            for c in rest.chars() {
                if !order.set(c, start) {
                    return Err(format!("unknown option {c}"));
                }
            }
            Ok((field, char))
        };
        let start = match position(start, true) {
            Ok(p) => p,
            Err(why) => return invalid(&why),
        };
        let end = match end.map(|e| position(e, false)).transpose() {
            Ok(e) => e,
            Err(why) => return invalid(&why),
        };
        if order.plain() {
            order = global;
        }
        Ok(Key { start, end, order })
    }
}

/// How lines are sorted, from the options.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Sort {
    pub keys: Vec<Key>,
    /// The bytes between fields, or none for blanks.
    pub separator: Option<Vec<u8>>,
    /// Whether equal keys keep their lines' order, without comparing the
    /// whole lines last.
    pub stable: bool,
    pub unique: bool,
    pub reverse: bool,
    /// The byte that ends lines.
    pub end: u8,
}

impl Sort {
    /// How `run` asks lines to be sorted, or why its arguments are
    /// invalid.
    pub fn of(run: &Run) -> Result<Sort, String> {
        let mut global = Order::default();
        for c in ['b', 'f', 'h', 'n', 'r', 'V'] {
            if run.has(&format!("-{c}")) {
                global.set(c, true);
                if c == 'b' {
                    global.set(c, false);
                }
            }
        }
        let mut keys = Vec::new();
        for text in run.values("-k") {
            keys.push(Key::parse(text, global)?);
        }
        if keys.is_empty() {
            keys.push(Key {
                start: (1, 1),
                end: None,
                order: global,
            });
        }
        let separator = match run.value("-t") {
            None => None,
            Some("\\0") => Some(vec![0]),
            Some(t) if t.chars().count() == 1 => Some(t.as_bytes().to_vec()),
            Some(t) => return Err(format!("the separator must be one character, not {t}")),
        };
        Ok(Sort {
            keys,
            separator,
            stable: run.has("-s"),
            unique: run.has("-u"),
            reverse: global.reverse,
            end: if run.has("-z") { 0 } else { b'\n' },
        })
    }

    /// Compares `a` and `b` by the keys alone.
    pub fn keys(&self, a: &[u8], b: &[u8]) -> Ordering {
        for key in &self.keys {
            let ordering = compare(&key.order, self.field(a, key), self.field(b, key));
            if ordering != Ordering::Equal {
                return ordering;
            }
        }
        Ordering::Equal
    }

    /// Compares `a` and `b` by the keys, and then by their bytes unless
    /// stable or unique.
    pub fn lines(&self, a: &[u8], b: &[u8]) -> Ordering {
        let ordering = self.keys(a, b);
        if ordering != Ordering::Equal || self.stable || self.unique {
            return ordering;
        }
        match self.reverse {
            true => b.cmp(a),
            false => a.cmp(b),
        }
    }

    /// Sorts `lines`, keeping only the first of equal ones when unique.
    pub fn sort(&self, lines: &mut Vec<Vec<u8>>) {
        lines.sort_by(|a, b| self.lines(a, b));
        if self.unique {
            lines.dedup_by(|later, earlier| self.keys(earlier, later) == Ordering::Equal);
        }
    }

    /// The number, from 1, of the first line of `lines` out of order, if
    /// any: after one that it should precede, or one equal to it when
    /// unique.
    pub fn disorder(&self, lines: &[Vec<u8>]) -> Option<usize> {
        let ordering = |a: &[u8], b: &[u8]| match self.unique {
            true => self.keys(a, b),
            false => self.lines(a, b),
        };
        lines
            .windows(2)
            .position(|pair| {
                let o = ordering(&pair[0], &pair[1]);
                o == Ordering::Greater || (self.unique && o == Ordering::Equal)
            })
            .map(|i| i + 2)
    }

    /// The part of `line` that `key` selects.
    fn field<'a>(&self, line: &'a [u8], key: &Key) -> &'a [u8] {
        let start = self.position(line, key.start.0, key.start.1, key.order.blanks_start);
        let end = match key.end {
            None => line.len(),
            Some((field, 0)) => {
                let at = self.skip_fields(line, field - 1);
                self.field_end(line, at)
            }
            Some((field, char)) => {
                let at = self.skip_fields(line, field - 1);
                let end = self.field_end(line, at);
                let mut at = at;
                if key.order.blanks_end {
                    at += line[at..end].iter().take_while(|b| blank(**b)).count();
                }
                at + characters(&line[at..end], char)
            }
        };
        if end <= start {
            return &[];
        }
        &line[start..end]
    }

    /// Where the `char`th character of field `field` of `line` starts,
    /// after the field's blanks when `blanks`, or the line's end.
    fn position(&self, line: &[u8], field: usize, char: usize, blanks: bool) -> usize {
        let mut at = self.skip_fields(line, field - 1);
        if blanks {
            at += line[at..].iter().take_while(|b| blank(**b)).count();
        }
        at + characters(&line[at..], char.saturating_sub(1))
    }

    /// Where field `n + 1` of `line` starts.
    fn skip_fields(&self, line: &[u8], n: usize) -> usize {
        let mut at = 0;
        for _ in 0..n {
            if at >= line.len() {
                break;
            }
            match &self.separator {
                Some(separator) => match find(&line[at..], separator) {
                    Some(i) => at += i + separator.len(),
                    None => at = line.len(),
                },
                None => {
                    at += line[at..].iter().take_while(|b| blank(**b)).count();
                    at += line[at..].iter().take_while(|b| !blank(**b)).count();
                }
            }
        }
        at
    }

    /// Where the field that starts at `at` in `line` ends.
    fn field_end(&self, line: &[u8], at: usize) -> usize {
        let rest = &line[at.min(line.len())..];
        let length = match &self.separator {
            Some(separator) => find(rest, separator).unwrap_or(rest.len()),
            None => {
                let blanks = rest.iter().take_while(|b| blank(**b)).count();
                blanks + rest[blanks..].iter().take_while(|b| !blank(**b)).count()
            }
        };
        at + length
    }
}

fn blank(b: u8) -> bool {
    b == b' ' || b == b'\t'
}

fn find(text: &[u8], what: &[u8]) -> Option<usize> {
    text.windows(what.len()).position(|w| w == what)
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

/// Compares the keys `a` and `b` as `order` says.
fn compare(order: &Order, a: &[u8], b: &[u8]) -> Ordering {
    let ordering = if order.numeric {
        numeric(a, b)
    } else if order.human {
        human(a, b)
    } else if order.version {
        version(a, b)
    } else if order.fold {
        folded(a).cmp(&folded(b))
    } else {
        a.cmp(b)
    };
    match order.reverse {
        true => ordering.reverse(),
        false => ordering,
    }
}

/// `text` with its characters in upper case, and bytes that are not
/// UTF-8 as they are.
fn folded(text: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(text.len());
    for chunk in text.utf8_chunks() {
        out.extend_from_slice(chunk.valid().to_uppercase().as_bytes());
        out.extend_from_slice(chunk.invalid());
    }
    out
}

/// A decimal number at the start of a key: whether it is negative, its
/// whole digits without leading zeros, its fraction's digits without
/// trailing zeros, and the rest of the key.
fn number(text: &[u8]) -> (bool, &[u8], &[u8], &[u8]) {
    let mut at = text.iter().take_while(|b| blank(**b)).count();
    let negative = text.get(at) == Some(&b'-');
    if negative {
        at += 1;
    }
    let digits = |from: usize| {
        text[from..]
            .iter()
            .take_while(|b| b.is_ascii_digit())
            .count()
    };
    let whole_length = digits(at);
    let mut whole = &text[at..at + whole_length];
    at += whole_length;
    let mut fraction: &[u8] = &[];
    if text.get(at) == Some(&b'.') {
        let length = digits(at + 1);
        fraction = &text[at + 1..at + 1 + length];
        at += 1 + length;
    }
    while let [b'0', rest @ ..] = whole {
        whole = rest;
    }
    while let [rest @ .., b'0'] = fraction {
        fraction = rest;
    }
    let zero = whole.is_empty() && fraction.is_empty();
    (negative && !zero, whole, fraction, &text[at..])
}

/// Compares the magnitudes of two numbers of `number`.
fn magnitude(a: (&[u8], &[u8]), b: (&[u8], &[u8])) -> Ordering {
    a.0.len()
        .cmp(&b.0.len())
        .then_with(|| a.0.cmp(b.0))
        .then_with(|| a.1.cmp(b.1))
}

fn numeric(a: &[u8], b: &[u8]) -> Ordering {
    let (a_negative, a_whole, a_fraction, _) = number(a);
    let (b_negative, b_whole, b_fraction, _) = number(b);
    match (a_negative, b_negative) {
        (true, false) => Ordering::Less,
        (false, true) => Ordering::Greater,
        (negative, _) => {
            let ordering = magnitude((a_whole, a_fraction), (b_whole, b_fraction));
            if negative {
                ordering.reverse()
            } else {
                ordering
            }
        }
    }
}

/// Compares sizes: by sign, then by the suffix's power, then as numbers.
fn human(a: &[u8], b: &[u8]) -> Ordering {
    let power = |rest: &[u8]| {
        rest.first()
            .and_then(|s| {
                b"KMGTPEZY"
                    .iter()
                    .position(|p| p == s || (*s == b'k' && *p == b'K'))
            })
            .map_or(0, |p| p + 1)
    };
    let (a_negative, a_whole, a_fraction, a_rest) = number(a);
    let (b_negative, b_whole, b_fraction, b_rest) = number(b);
    let sign = |negative: bool, whole: &[u8], fraction: &[u8]| match (
        negative,
        whole.is_empty() && fraction.is_empty(),
    ) {
        (true, _) => -1,
        (false, true) => 0,
        (false, false) => 1,
    };
    let a_sign = sign(a_negative, a_whole, a_fraction);
    let b_sign = sign(b_negative, b_whole, b_fraction);
    a_sign.cmp(&b_sign).then_with(|| {
        let ordering = power(a_rest)
            .cmp(&power(b_rest))
            .then_with(|| magnitude((a_whole, a_fraction), (b_whole, b_fraction)));
        if a_sign < 0 {
            ordering.reverse()
        } else {
            ordering
        }
    })
}

/// Compares versions as GNU's `filevercmp` does: `.` and `..` and other
/// names starting with a dot first, then by parts of digits and of other
/// characters, without suffixes such as `.tar.gz` first.
pub fn version(a: &[u8], b: &[u8]) -> Ordering {
    match (a.is_empty(), b.is_empty()) {
        (true, true) => return Ordering::Equal,
        (true, false) => return Ordering::Less,
        (false, true) => return Ordering::Greater,
        _ => {}
    }
    for special in [&b"."[..], b".."] {
        match (a == special, b == special) {
            (true, true) => return Ordering::Equal,
            (true, false) => return Ordering::Less,
            (false, true) => return Ordering::Greater,
            _ => {}
        }
    }
    match (a[0] == b'.', b[0] == b'.') {
        (true, false) => return Ordering::Less,
        (false, true) => return Ordering::Greater,
        _ => {}
    }
    let a_prefix = prefix(a);
    let b_prefix = prefix(b);
    let ordering = parts(&a[..a_prefix], &b[..b_prefix]);
    if ordering != Ordering::Equal || (a_prefix == a.len() && b_prefix == b.len()) {
        return ordering;
    }
    parts(a, b)
}

/// The length of `name` without its suffix: the dots, each followed by a
/// letter or `~` and then letters, digits or `~`, that end it.
fn prefix(name: &[u8]) -> usize {
    let n = name.len();
    let mut length = 0;
    let mut i = 0;
    while i < n {
        i += 1;
        length = i;
        while i + 1 < n
            && name[i] == b'.'
            && (name[i + 1].is_ascii_alphabetic() || name[i + 1] == b'~')
        {
            i += 2;
            while i < n && (name[i].is_ascii_alphanumeric() || name[i] == b'~') {
                i += 1;
            }
        }
    }
    length
}

/// The rank of the character at `at` among those that are not digits:
/// the end, after `~`, before letters, before the rest.
fn rank(text: &[u8], at: usize) -> i32 {
    match text.get(at) {
        None => -1,
        Some(c) if c.is_ascii_digit() => 0,
        Some(c) if c.is_ascii_alphabetic() => i32::from(*c),
        Some(b'~') => -2,
        Some(c) => i32::from(*c) + 256,
    }
}

/// Compares `a` and `b` by alternate parts of other characters and of
/// digits, the latter as numbers.
fn parts(a: &[u8], b: &[u8]) -> Ordering {
    let (mut i, mut j) = (0, 0);
    let digit = |text: &[u8], at: usize| text.get(at).is_some_and(u8::is_ascii_digit);
    while i < a.len() || j < b.len() {
        while (i < a.len() && !digit(a, i)) || (j < b.len() && !digit(b, j)) {
            let (x, y) = (rank(a, i), rank(b, j));
            if x != y {
                return x.cmp(&y);
            }
            i += 1;
            j += 1;
        }
        while a.get(i) == Some(&b'0') {
            i += 1;
        }
        while b.get(j) == Some(&b'0') {
            j += 1;
        }
        let mut first = Ordering::Equal;
        while digit(a, i) && digit(b, j) {
            if first == Ordering::Equal {
                first = a[i].cmp(&b[j]);
            }
            i += 1;
            j += 1;
        }
        if digit(a, i) {
            return Ordering::Greater;
        }
        if digit(b, j) {
            return Ordering::Less;
        }
        if first != Ordering::Equal {
            return first;
        }
    }
    Ordering::Equal
}

/// The lines of `text`, each without the byte `end` that ends it; the
/// last needs none.
pub fn split(text: &[u8], end: u8) -> Vec<Vec<u8>> {
    let mut lines: Vec<Vec<u8>> = text.split(|b| *b == end).map(<[u8]>::to_vec).collect();
    if lines.last().is_some_and(Vec::is_empty) {
        lines.pop();
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;
    use mikros_cli::Parsed;

    fn sort_of(arguments: &[&str]) -> Result<Sort, String> {
        match COMMAND.parse(arguments.iter().map(|a| a.to_string())) {
            Parsed::Run(run) => Sort::of(&run),
            other => panic!("{other:?}"),
        }
    }

    fn sort(arguments: &[&str], lines: &[&str]) -> Vec<String> {
        let sort = sort_of(arguments).unwrap();
        let mut lines: Vec<Vec<u8>> = lines.iter().map(|l| l.as_bytes().to_vec()).collect();
        sort.sort(&mut lines);
        lines
            .into_iter()
            .map(|l| String::from_utf8(l).unwrap())
            .collect()
    }

    #[test]
    fn sorts_by_code_point() {
        assert_eq!(
            sort(&[], &["b", "ä", "a", "B", "z"]),
            ["B", "a", "b", "z", "ä"]
        );
        assert_eq!(sort(&["-r"], &["a", "c", "b"]), ["c", "b", "a"]);
        assert_eq!(sort(&["-f"], &["b", "A", "a", "B"]), ["A", "a", "B", "b"]);
        assert_eq!(sort(&["-u"], &["b", "a", "b"]), ["a", "b"]);
        assert_eq!(sort(&["-fu"], &["b", "B", "a"]), ["a", "b"]);
        assert_eq!(sort(&["-b"], &["  b", "a"]), ["a", "  b"]);
        assert_eq!(sort(&[], &["  b", "a"]), ["  b", "a"]);
    }

    #[test]
    fn sorts_numbers_and_sizes() {
        assert_eq!(
            sort(&["-n"], &["10", "9", "-3", "x", "2.50", "2.5", "-10", "0"]),
            ["-10", "-3", "0", "x", "2.5", "2.50", "9", "10"]
        );
        assert_eq!(
            sort(
                &["-n"],
                &["123456789012345678901234567890", "99999999999999999999999"]
            ),
            ["99999999999999999999999", "123456789012345678901234567890"]
        );
        assert_eq!(
            sort(&["-h"], &["2G", "1K", "900", "3M", "-1K", "1.5K"]),
            ["-1K", "900", "1K", "1.5K", "3M", "2G"]
        );
        assert_eq!(sort(&["-nr"], &["1", "3", "2"]), ["3", "2", "1"]);
    }

    #[test]
    fn sorts_versions() {
        assert_eq!(
            sort(
                &["-V"],
                &["a-1.10", "a-1.9", "a-1.9~rc1", "a-1.2.tar.gz", "a-1.2"]
            ),
            ["a-1.2", "a-1.2.tar.gz", "a-1.9~rc1", "a-1.9", "a-1.10"]
        );
        assert_eq!(
            sort(&["-V"], &["b", ".a", "..", "."]),
            [".", "..", ".a", "b"]
        );
        assert_eq!(version(b"file7", b"file10"), Ordering::Less);
        assert_eq!(version(b"file007", b"file7"), Ordering::Equal);
    }

    #[test]
    fn sorts_by_keys() {
        let lines = ["x 3 b", "y 1 c", "z 2 a"];
        assert_eq!(sort(&["-k", "2n"], &lines), ["y 1 c", "z 2 a", "x 3 b"]);
        assert_eq!(sort(&["-k3"], &lines), ["z 2 a", "x 3 b", "y 1 c"]);
        assert_eq!(sort(&["-k2,2nr"], &lines), ["x 3 b", "z 2 a", "y 1 c"]);
        assert_eq!(
            sort(&["-t", ":", "-k", "2"], &["a:b", "b:a", "c"]),
            ["c", "b:a", "a:b"]
        );
        assert_eq!(
            sort(&["-t:", "-k1.2,1.2"], &["ab:", "ba:", "ac:"]),
            ["ba:", "ab:", "ac:"]
        );
        assert_eq!(
            sort(&["-k", "1.2b,1.2b"], &["  zb", " ya", "xc"]),
            [" ya", "  zb", "xc"]
        );
        assert_eq!(sort(&["-k", "1.2,1.2"], &["äb", "öa"]), ["öa", "äb"]);
        // Equal keys fall back to the whole line, unless stable.
        assert_eq!(sort(&["-k2,2"], &["b 1", "a 1"]), ["a 1", "b 1"]);
        assert_eq!(sort(&["-s", "-k2,2"], &["b 1", "a 1"]), ["b 1", "a 1"]);
        assert_eq!(sort(&["-u", "-k2,2"], &["b 1", "a 1"]), ["b 1"]);
        // Global options apply to keys without their own.
        assert_eq!(sort(&["-n", "-k2"], &["a 10", "b 9"]), ["b 9", "a 10"]);
        assert_eq!(sort(&["-n", "-k2r"], &["a 10", "b 9"]), ["b 9", "a 10"]);
    }

    #[test]
    fn checks_the_order() {
        let sort = sort_of(&[]).unwrap();
        let lines = split(b"a\nb\nb\na\n", b'\n');
        assert_eq!(sort.disorder(&lines), Some(4));
        assert_eq!(sort.disorder(&lines[..3]), None);
        let unique = sort_of(&["-u"]).unwrap();
        assert_eq!(unique.disorder(&lines[..3]), Some(3));
    }

    #[test]
    fn splits_lines() {
        assert_eq!(split(b"a\nb", b'\n'), [b"a".to_vec(), b"b".to_vec()]);
        assert_eq!(split(b"a\n\n", b'\n'), [b"a".to_vec(), b"".to_vec()]);
        assert_eq!(split(b"", b'\n'), Vec::<Vec<u8>>::new());
        assert_eq!(split(b"a\0b\n\0", 0), [b"a".to_vec(), b"b\n".to_vec()]);
    }

    #[test]
    fn refuses_bad_keys_and_separators() {
        assert!(sort_of(&["-k", "0"]).is_err());
        assert!(sort_of(&["-k", "1.0"]).is_err());
        assert!(sort_of(&["-k", "x"]).is_err());
        assert!(sort_of(&["-k", "1q"]).is_err());
        assert!(sort_of(&["-t", "ab"]).is_err());
        assert!(sort_of(&["-k", "2,3.0"]).is_ok());
    }

    #[test]
    fn declares_what_it_takes() {
        let file = concat!(env!("CARGO_MANIFEST_DIR"), "/sort.msh");
        mikros_cli::declared(&COMMAND, std::path::Path::new(file)).unwrap();
    }
}
