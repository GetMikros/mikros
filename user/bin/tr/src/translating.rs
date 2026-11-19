//! What tr takes, how it reads its sets of characters, and how it
//! translates, deletes and squeezes the characters of text read in
//! pieces (MIKSPEC-0098.R49).

use mikros_cli::{Kind, Operand, Opt, Run, Spec};
use std::collections::HashMap;

pub static COMMAND: Spec = Spec {
    operands: &[
        Operand::one("set1", Kind::Text, "the characters to find"),
        Operand::optional("set2", Kind::Text, "the characters to put in their place"),
    ],
    options: &[
        Opt::both("complement", 'c', "use the characters not in set1"),
        Opt::both("delete", 'd', "delete the characters of set1"),
        Opt::both(
            "squeeze-repeats",
            's',
            "write a run of one character of the last set once",
        ),
    ],
    ..Spec::new(
        "tr",
        "Copies standard input to standard output, translating the characters\n\
         of set1 to those of set2, deleting or squeezing them. Sets take\n\
         ranges such as a-z, escapes such as \\n and \\012, classes such as\n\
         [:upper:], [=c=], and in set2 [c*n] and [c*] for repeats.",
    )
};

/// The classes that sets take, each with its characters, which are those
/// of ASCII.
const CLASSES: [&str; 12] = [
    "alnum", "alpha", "blank", "cntrl", "digit", "graph", "lower", "print", "punct", "space",
    "upper", "xdigit",
];

fn class_has(class: &str, c: u8) -> bool {
    match class {
        "alnum" => c.is_ascii_alphanumeric(),
        "alpha" => c.is_ascii_alphabetic(),
        "blank" => c == b' ' || c == b'\t',
        "cntrl" => c.is_ascii_control(),
        "digit" => c.is_ascii_digit(),
        "graph" => c.is_ascii_graphic(),
        "lower" => c.is_ascii_lowercase(),
        "print" => c.is_ascii_graphic() || c == b' ',
        "punct" => c.is_ascii_punctuation(),
        "space" => matches!(c, b' ' | b'\t'..=b'\r'),
        "upper" => c.is_ascii_uppercase(),
        _ => c.is_ascii_hexdigit(),
    }
}

/// A piece of a set.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Item {
    Char(char),
    Range(char, char),
    Class(&'static str),
    /// `[c*n]`, or `[c*]` to fill set2 to the length of set1.
    Repeat(char, Option<usize>),
}

/// The character that starts at `chars[at]`, with the escapes, and where
/// the next starts.
fn escaped(chars: &[char], at: usize) -> (char, usize) {
    if chars[at] != '\\' || at + 1 == chars.len() {
        return (chars[at], at + 1);
    }
    let c = chars[at + 1];
    let simple = match c {
        'a' => Some('\x07'),
        'b' => Some('\x08'),
        'f' => Some('\x0c'),
        'n' => Some('\n'),
        'r' => Some('\r'),
        't' => Some('\t'),
        'v' => Some('\x0b'),
        _ => None,
    };
    if let Some(s) = simple {
        return (s, at + 2);
    }
    if c.is_digit(8) {
        let mut value = 0u32;
        let mut end = at + 1;
        while end < chars.len() && end < at + 4 {
            let Some(d) = chars[end].to_digit(8) else {
                break;
            };
            if value * 8 + d > 0o377 {
                break;
            }
            value = value * 8 + d;
            end += 1;
        }
        return (char::from_u32(value).unwrap_or('\0'), end);
    }
    (c, at + 2)
}

/// The items of the set `text`.
fn items(text: &str) -> Result<Vec<Item>, String> {
    let chars: Vec<char> = text.chars().collect();
    let find = |from: usize, end: &[char]| {
        (from..chars.len().saturating_sub(1)).find(|&i| chars[i..].starts_with(end))
    };
    let mut items = Vec::new();
    let mut at = 0;
    while at < chars.len() {
        if chars[at] == '[' && at + 1 < chars.len() {
            match chars[at + 1] {
                ':' => {
                    if let Some(end) = find(at + 2, &[':', ']']) {
                        let name: String = chars[at + 2..end].iter().collect();
                        let Some(class) = CLASSES.iter().find(|c| **c == name) else {
                            return Err(format!("invalid character class {name}"));
                        };
                        items.push(Item::Class(class));
                        at = end + 2;
                        continue;
                    }
                }
                '=' => {
                    if chars.get(at + 3) == Some(&'=') && chars.get(at + 4) == Some(&']') {
                        items.push(Item::Char(chars[at + 2]));
                        at += 5;
                        continue;
                    }
                }
                _ => {
                    let (c, next) = escaped(&chars, at + 1);
                    if chars.get(next) == Some(&'*')
                        && let Some(end) = (next + 1..chars.len()).find(|&i| chars[i] == ']')
                    {
                        let digits: String = chars[next + 1..end].iter().collect();
                        let count = match digits.as_str() {
                            "" => None,
                            d if d.starts_with('0') => usize::from_str_radix(d, 8).ok(),
                            d => d.parse().ok(),
                        };
                        if digits.is_empty() || count.is_some() {
                            items.push(Item::Repeat(c, count.filter(|n| *n > 0)));
                            at = end + 1;
                            continue;
                        }
                        return Err(format!("invalid repeat count {digits}"));
                    }
                }
            }
        }
        let (c, next) = escaped(&chars, at);
        if chars.get(next) == Some(&'-') && next + 1 < chars.len() {
            let (end, after) = escaped(&chars, next + 1);
            if end < c {
                return Err(format!("the range {c}-{end} runs backwards"));
            }
            items.push(Item::Range(c, end));
            at = after;
        } else {
            items.push(Item::Char(c));
            at = next;
        }
    }
    Ok(items)
}

/// The characters of `items`, with `fill` characters for a `[c*]`.
fn expand(items: &[Item], fill: usize) -> Vec<char> {
    let mut chars = Vec::new();
    for item in items {
        match item {
            Item::Char(c) => chars.push(*c),
            Item::Range(start, end) => chars.extend(*start..=*end),
            Item::Class(class) => {
                chars.extend((0..128u8).filter(|b| class_has(class, *b)).map(char::from))
            }
            Item::Repeat(c, Some(n)) => chars.extend(std::iter::repeat_n(*c, *n)),
            Item::Repeat(c, None) => chars.extend(std::iter::repeat_n(*c, fill)),
        }
    }
    chars
}

/// The characters of set1, which takes no repeats.
fn first_set(text: &str) -> Result<Vec<char>, String> {
    let items = items(text)?;
    if items.iter().any(|i| matches!(i, Item::Repeat(..))) {
        return Err("set1 takes no repeats [c*n]".into());
    }
    Ok(expand(&items, 0))
}

/// The characters of set2, filled to `length` by its one `[c*]`; only
/// `upper` and `lower` among the classes when `translating`.
fn second_set(text: &str, length: usize, translating: bool) -> Result<Vec<char>, String> {
    let items = items(text)?;
    let fills = items
        .iter()
        .filter(|i| matches!(i, Item::Repeat(_, None)))
        .count();
    if fills > 1 {
        return Err("set2 takes at most one [c*]".into());
    }
    if translating
        && items
            .iter()
            .any(|i| matches!(i, Item::Class(c) if *c != "upper" && *c != "lower"))
    {
        return Err("set2 of a translation takes only the classes upper and lower".into());
    }
    let fixed = expand(&items, 0).len();
    Ok(expand(&items, length.saturating_sub(fixed)))
}

/// Characters to find: those listed, or with `complement` all others.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct Set {
    /// Sorted, each once.
    chars: Vec<char>,
    complement: bool,
}

impl Set {
    fn new(mut chars: Vec<char>, complement: bool) -> Set {
        chars.sort_unstable();
        chars.dedup();
        Set { chars, complement }
    }

    fn has(&self, c: char) -> bool {
        self.chars.binary_search(&c).is_ok() != self.complement
    }
}

/// How characters are translated.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Map {
    /// Each character of set1 to the one of set2 at its place.
    Table(HashMap<char, char>),
    /// Each character not in `set1`, sorted, to the one of `set2` at its
    /// place among them, or the last.
    Complement { set1: Vec<char>, set2: Vec<char> },
}

impl Map {
    fn get(&self, c: char) -> char {
        match self {
            Map::Table(table) => table.get(&c).copied().unwrap_or(c),
            Map::Complement { set1, set2 } => match set1.binary_search(&c) {
                Ok(_) => c,
                Err(below) => {
                    let place = (c as usize).saturating_sub(below);
                    set2[place.min(set2.len() - 1)]
                }
            },
        }
    }
}

/// What tr does, from its options and sets.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Tr {
    delete: Option<Set>,
    map: Option<Map>,
    squeeze: Option<Set>,
}

impl Tr {
    /// What `run` asks, or why its arguments are invalid.
    pub fn of(run: &Run) -> Result<Tr, String> {
        let (complement, delete, squeeze) = (run.has("-c"), run.has("-d"), run.has("-s"));
        let set1 = first_set(&run.operands[0])?;
        let set2 = run.operands.get(1);
        match (delete, squeeze, set2) {
            (true, false, Some(extra)) => Err(format!(
                "unexpected argument {extra}: only one set is deleted"
            )),
            (true, false, None) => Ok(Tr {
                delete: Some(Set::new(set1, complement)),
                ..Tr::default()
            }),
            (true, true, None) => Err("missing set2, the characters to squeeze".into()),
            (true, true, Some(set2)) => Ok(Tr {
                delete: Some(Set::new(set1, complement)),
                squeeze: Some(Set::new(second_set(set2, 0, false)?, false)),
                ..Tr::default()
            }),
            (false, true, None) => Ok(Tr {
                squeeze: Some(Set::new(set1, complement)),
                ..Tr::default()
            }),
            (false, false, None) => Err("missing set2, the characters to put in place".into()),
            (false, _, Some(set2)) => {
                let set2 = second_set(set2, set1.len(), true)?;
                if set2.is_empty() && (complement || !set1.is_empty()) {
                    return Err("set2 must not be empty".into());
                }
                let map = match complement {
                    true => Map::Complement {
                        set1: Set::new(set1, false).chars,
                        set2: set2.clone(),
                    },
                    false => {
                        let last = set2.last().copied().unwrap_or('\0');
                        let table = set1
                            .iter()
                            .enumerate()
                            .map(|(i, c)| (*c, set2.get(i).copied().unwrap_or(last)))
                            .collect();
                        Map::Table(table)
                    }
                };
                Ok(Tr {
                    map: Some(map),
                    squeeze: squeeze.then(|| Set::new(set2, false)),
                    ..Tr::default()
                })
            }
        }
    }
}

/// Changes text given in pieces, decoding it as UTF-8 across the pieces'
/// ends and copying bytes that are not UTF-8 as they are.
pub struct Translator<'a> {
    tr: &'a Tr,
    /// The character written last, for squeezing.
    last: Option<char>,
    /// The start of a character that the last piece ended in.
    carry: Vec<u8>,
}

impl<'a> Translator<'a> {
    pub fn new(tr: &'a Tr) -> Translator<'a> {
        Translator {
            tr,
            last: None,
            carry: Vec::new(),
        }
    }

    /// Changes `bytes`, the next piece, into `out`.
    pub fn feed(&mut self, bytes: &[u8], out: &mut Vec<u8>) {
        let mut joined = std::mem::take(&mut self.carry);
        joined.extend_from_slice(bytes);
        let cut = joined.len() - incomplete(&joined);
        self.carry = joined.split_off(cut);
        self.change(&joined, out);
    }

    /// Writes what is left, after the last piece, into `out`.
    pub fn finish(mut self, out: &mut Vec<u8>) {
        let carry = std::mem::take(&mut self.carry);
        self.change(&carry, out);
    }

    fn change(&mut self, bytes: &[u8], out: &mut Vec<u8>) {
        let mut buffer = [0; 4];
        for chunk in bytes.utf8_chunks() {
            for c in chunk.valid().chars() {
                if self.tr.delete.as_ref().is_some_and(|d| d.has(c)) {
                    continue;
                }
                let c = self.tr.map.as_ref().map_or(c, |m| m.get(c));
                if self.last == Some(c) && self.tr.squeeze.as_ref().is_some_and(|s| s.has(c)) {
                    continue;
                }
                out.extend_from_slice(c.encode_utf8(&mut buffer).as_bytes());
                self.last = Some(c);
            }
            if !chunk.invalid().is_empty() {
                out.extend_from_slice(chunk.invalid());
                self.last = None;
            }
        }
    }
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

#[cfg(test)]
mod tests {
    use super::*;
    use mikros_cli::Parsed;

    fn tr_of(arguments: &[&str]) -> Result<Tr, String> {
        match COMMAND.parse(arguments.iter().map(|a| a.to_string())) {
            Parsed::Run(run) => Tr::of(&run),
            other => panic!("{other:?}"),
        }
    }

    fn tr(arguments: &[&str], text: &[u8]) -> Vec<u8> {
        let tr = tr_of(arguments).unwrap();
        let mut out = Vec::new();
        let mut translator = Translator::new(&tr);
        for byte in text {
            translator.feed(&[*byte], &mut out);
        }
        translator.finish(&mut out);
        out
    }

    fn tr_text(arguments: &[&str], text: &str) -> String {
        String::from_utf8(tr(arguments, text.as_bytes())).unwrap()
    }

    #[test]
    fn reads_sets() {
        assert_eq!(
            first_set("a-c\\n\\101").unwrap(),
            ['a', 'b', 'c', '\n', 'A']
        );
        assert_eq!(first_set("[:digit:]").unwrap().len(), 10);
        assert_eq!(first_set("[:punct:]").unwrap().len(), 32);
        assert_eq!(first_set("[=e=]x").unwrap(), ['e', 'x']);
        assert_eq!(first_set("a-").unwrap(), ['a', '-']);
        assert_eq!(first_set("[a").unwrap(), ['[', 'a']);
        assert_eq!(first_set("\\\\-").unwrap(), ['\\', '-']);
        assert!(first_set("z-a").is_err());
        assert!(first_set("[:nope:]").is_err());
        assert!(first_set("[a*3]").is_err());
        assert_eq!(second_set("[x*3]y", 0, true).unwrap(), ['x', 'x', 'x', 'y']);
        assert_eq!(
            second_set("a[x*]b", 5, true).unwrap(),
            ['a', 'x', 'x', 'x', 'b']
        );
        assert_eq!(second_set("[x*010]", 0, true).unwrap().len(), 8);
        assert!(second_set("[:digit:]", 10, true).is_err());
    }

    #[test]
    fn translates_characters() {
        assert_eq!(tr_text(&["a-z", "A-Z"], "hello, wörld"), "HELLO, WöRLD");
        assert_eq!(tr_text(&["[:lower:]", "[:upper:]"], "abc xyz"), "ABC XYZ");
        assert_eq!(tr_text(&["abc", "x"], "abcd"), "xxxd");
        assert_eq!(tr_text(&["äö", "ao"], "Bäöx"), "Baox");
        assert_eq!(tr_text(&["€", "E"], "5€"), "5E");
        assert_eq!(tr_text(&["aa", "xy"], "a"), "y");
    }

    #[test]
    fn deletes_squeezes_and_complements() {
        assert_eq!(tr_text(&["-d", "[:digit:]"], "a1b22c"), "abc");
        assert_eq!(tr_text(&["-dc", "a-z\\n"], "Hi, there!\n"), "ithere\n");
        assert_eq!(tr_text(&["-s", " "], "a   b  c"), "a b c");
        assert_eq!(
            tr_text(&["-cs", "a-z", "\\n"], "one, two  three"),
            "one\ntwo\nthree"
        );
        assert_eq!(tr_text(&["-ds", "x", "a"], "axaab"), "ab");
        assert_eq!(tr_text(&["-s", "a-z", "A-Z"], "aabbAc"), "ABAC");
        assert_eq!(tr_text(&["-c", "a", "xyz"], "a\x01\x02b"), "ayzz");
    }

    #[test]
    fn copies_bytes_that_are_not_utf8() {
        assert_eq!(tr(&["a", "b"], b"a\xffa\xe2\x82"), b"b\xffb\xe2\x82");
        assert_eq!(tr(&["-s", "a"], b"a\xffaa"), b"a\xffa");
    }

    #[test]
    fn refuses_bad_operands() {
        assert!(tr_of(&["a"]).is_err());
        assert!(tr_of(&["-d", "a", "b"]).is_err());
        assert!(tr_of(&["-ds", "a"]).is_err());
        assert!(tr_of(&["a", ""]).is_err());
        assert!(tr_of(&["-s", "a"]).is_ok());
    }

    #[test]
    fn declares_what_it_takes() {
        let file = concat!(env!("CARGO_MANIFEST_DIR"), "/tr.msh");
        mikros_cli::declared(&COMMAND, std::path::Path::new(file)).unwrap();
    }
}
