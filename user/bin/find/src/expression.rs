//! find's expression (MIKSPEC-0098.R42): how its words are read into a
//! tree of primaries and operators, and how the tree is decided for an
//! entry, with what it does to the entry left to a `World`.

use crate::pattern::Pattern;
use crate::primaries::named;
use mikros_cli::{json_string, size, time};
use std::cmp::Ordering;

/// What an entry is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Type {
    File,
    Folder,
    Link,
    Socket,
    /// Anything else, such as a device, which no `-type` names.
    Other,
}

impl Type {
    fn word(self) -> &'static str {
        match self {
            Type::File => "file",
            Type::Folder => "folder",
            Type::Link => "link",
            Type::Socket => "socket",
            Type::Other => "other",
        }
    }
}

/// An entry that the walk reached.
#[derive(Clone, Debug, PartialEq)]
pub struct Entry {
    /// The path as written: a path given, or a folder's and a name.
    pub path: String,
    /// The last component of the path.
    pub name: String,
    pub kind: Type,
    pub size: u64,
    /// When it was modified, as seconds since 1970 and nanoseconds.
    pub modified: (i64, u32),
}

impl Entry {
    /// The entry as an object of `--json`.
    pub fn json(&self) -> String {
        format!(
            "{{\"path\":{},\"type\":\"{}\",\"size\":{},\"modified\":{}}}",
            json_string(&self.path),
            self.kind.word(),
            self.size,
            json_string(&time::utc(self.modified.0))
        )
    }
}

/// The last component of `path`, a path given: without the slashes
/// after it, and `/` for the root.
pub fn last_component(path: &str) -> String {
    let trimmed = path.trim_end_matches('/');
    if trimmed.is_empty() {
        return match path.is_empty() {
            true => String::new(),
            false => "/".into(),
        };
    }
    trimmed.rsplit('/').next().unwrap_or(trimmed).into()
}

/// The path of the entry `name` in the folder at `folder`.
pub fn child(folder: &str, name: &str) -> String {
    match folder.ends_with('/') {
        true => format!("{folder}{name}"),
        false => format!("{folder}/{name}"),
    }
}

/// `[+|-]n`: more than, less than, or exactly `n`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Compare {
    wanted: Ordering,
    n: i64,
}

impl Compare {
    /// The comparison of `text` and the rest after its number.
    fn read(text: &str) -> Option<(Compare, &str)> {
        let (wanted, rest) = match text.as_bytes().first()? {
            b'+' => (Ordering::Greater, &text[1..]),
            b'-' => (Ordering::Less, &text[1..]),
            _ => (Ordering::Equal, text),
        };
        let digits = rest
            .find(|c: char| !c.is_ascii_digit())
            .unwrap_or(rest.len());
        let n = rest[..digits].parse().ok()?;
        Some((Compare { wanted, n }, &rest[digits..]))
    }

    fn holds(self, value: i64) -> bool {
        value.cmp(&self.n) == self.wanted
    }
}

/// A primary or an operator.
#[derive(Clone, Debug, PartialEq)]
pub enum Expr {
    /// `-name` and `-iname`, on the last component.
    Name(Pattern),
    /// `-path`, on the whole path.
    Path(Pattern),
    Type(Type),
    /// `-size`: the size in units of the given bytes, rounded up.
    Size(Compare, u64),
    /// `-mtime` and `-mmin`: the whole units of the given seconds since
    /// it was modified.
    Age(Compare, i64),
    /// `-newer`, with the index of its file in `Expression::newer`.
    Newer(usize),
    Empty,
    /// What `-maxdepth` and `-mindepth` are, where they stand.
    True,
    /// `-print`, and `-print0` with NUL.
    Print(bool),
    Delete,
    /// `-exec ... ;`.
    Exec(Vec<String>),
    /// `-exec ... {} +`, with the index of its batch.
    Batch(usize),
    Not(Box<Expr>),
    And(Box<Expr>, Box<Expr>),
    Or(Box<Expr>, Box<Expr>),
}

/// An expression as read, with what applies to the whole walk.
#[derive(Debug, PartialEq)]
pub struct Expression {
    pub root: Expr,
    pub maxdepth: Option<usize>,
    pub mindepth: usize,
    /// Whether it has `-delete`, which walks a folder's entries before
    /// the folder.
    pub deletes: bool,
    /// The files of `-newer`.
    pub newer: Vec<String>,
    /// The commands of `-exec ... {} +`.
    pub batches: Vec<Vec<String>>,
}

/// What deciding an expression for an entry needs from the walk and
/// does to it.
pub trait World {
    /// Whether the folder of `entry` has no entries.
    fn empty_folder(&mut self, entry: &Entry) -> bool;
    /// The modified time of the `index`th file of `-newer`.
    fn reference(&self, index: usize) -> (i64, u32);
    /// The seconds since 1970 when the walk started.
    fn now(&self) -> i64;
    fn print(&mut self, entry: &Entry, nul: bool);
    /// Removes the entry; whether it was removed.
    fn delete(&mut self, entry: &Entry) -> bool;
    /// Runs `command`, its `{}` replaced by the path; whether it exited
    /// with 0.
    fn exec(&mut self, command: &[String], entry: &Entry) -> bool;
    /// Adds the entry to the `index`th batch.
    fn batch(&mut self, index: usize, entry: &Entry);
}

impl Expr {
    /// Whether the expression is true for `entry`, doing what its
    /// actions say on the way, from left to right, as far as needed.
    pub fn decide(&self, entry: &Entry, world: &mut dyn World) -> bool {
        match self {
            Expr::Name(pattern) => pattern.matches(&entry.name),
            Expr::Path(pattern) => pattern.matches(&entry.path),
            Expr::Type(kind) => entry.kind == *kind,
            Expr::Size(compare, unit) => {
                let units = entry.size.div_ceil(*unit);
                compare.holds(i64::try_from(units).unwrap_or(i64::MAX))
            }
            Expr::Age(compare, unit) => {
                let age = world.now().saturating_sub(entry.modified.0);
                compare.holds(age.div_euclid(*unit))
            }
            Expr::Newer(index) => entry.modified > world.reference(*index),
            Expr::Empty => match entry.kind {
                Type::File => entry.size == 0,
                Type::Folder => world.empty_folder(entry),
                _ => false,
            },
            Expr::True => true,
            Expr::Print(nul) => {
                world.print(entry, *nul);
                true
            }
            Expr::Delete => world.delete(entry),
            Expr::Exec(command) => world.exec(command, entry),
            Expr::Batch(index) => {
                world.batch(*index, entry);
                true
            }
            Expr::Not(inner) => !inner.decide(entry, world),
            Expr::And(left, right) => left.decide(entry, world) && right.decide(entry, world),
            Expr::Or(left, right) => left.decide(entry, world) || right.decide(entry, world),
        }
    }
}

/// Reads the words of an expression.
struct Reader<'a> {
    words: &'a [String],
    at: usize,
    acts: bool,
    expression: Expression,
}

/// Reads `words`, an expression, or why they are not one. Without
/// `-print`, `-print0`, `-delete` or `-exec`, the expression is followed
/// by `-print`; without words it is `-print`.
pub fn parse(words: &[String]) -> Result<Expression, String> {
    let mut reader = Reader {
        words,
        at: 0,
        acts: false,
        expression: Expression {
            root: Expr::True,
            maxdepth: None,
            mindepth: 0,
            deletes: false,
            newer: Vec::new(),
            batches: Vec::new(),
        },
    };
    let root = match words.is_empty() {
        true => Expr::True,
        false => reader.or()?,
    };
    if let Some(word) = reader.peek() {
        return Err(format!("unexpected {word}"));
    }
    reader.expression.root = match reader.acts {
        true => root,
        false => Expr::And(Box::new(root), Box::new(Expr::Print(false))),
    };
    Ok(reader.expression)
}

impl Reader<'_> {
    fn peek(&self) -> Option<&str> {
        self.words.get(self.at).map(String::as_str)
    }

    fn next(&mut self) -> Option<&str> {
        let word = self.words.get(self.at)?;
        self.at += 1;
        Some(word)
    }

    fn or(&mut self) -> Result<Expr, String> {
        let mut left = self.and()?;
        while self.peek() == Some("-o") {
            self.at += 1;
            let right = self.and_after("-o")?;
            left = Expr::Or(Box::new(left), Box::new(right));
        }
        Ok(left)
    }

    fn and_after(&mut self, after: &str) -> Result<Expr, String> {
        match self.peek() {
            None | Some("-o" | "-a" | ")") => Err(format!("{after} needs an expression after it")),
            Some(_) => self.and(),
        }
    }

    fn and(&mut self) -> Result<Expr, String> {
        let mut left = self.not()?;
        loop {
            let right = match self.peek() {
                None | Some("-o" | ")") => return Ok(left),
                Some("-a") => {
                    self.at += 1;
                    match self.peek() {
                        None | Some("-o" | "-a" | ")") => {
                            return Err("-a needs an expression after it".into());
                        }
                        Some(_) => self.not()?,
                    }
                }
                Some(_) => self.not()?,
            };
            left = Expr::And(Box::new(left), Box::new(right));
        }
    }

    fn not(&mut self) -> Result<Expr, String> {
        let Some(word) = self.next().map(str::to_string) else {
            return Err("an expression is missing".into());
        };
        match word.as_str() {
            "!" | "-not" => match self.peek() {
                None | Some("-o" | "-a" | ")") => {
                    Err(format!("{word} needs an expression after it"))
                }
                Some(_) => Ok(Expr::Not(Box::new(self.not()?))),
            },
            "(" => {
                if self.peek() == Some(")") {
                    return Err("( ) holds no expression".into());
                }
                let inner = self.or()?;
                match self.next() {
                    Some(")") => Ok(inner),
                    _ => Err("( has no )".into()),
                }
            }
            ")" => Err("unexpected )".into()),
            "-a" | "-o" => Err(format!("{word} needs an expression before it")),
            _ => self.primary(&word),
        }
    }

    /// The value of the primary `word`.
    fn value(&mut self, word: &str) -> Result<String, String> {
        self.next()
            .map(str::to_string)
            .ok_or_else(|| format!("{word} needs a value"))
    }

    fn primary(&mut self, word: &str) -> Result<Expr, String> {
        if !word.starts_with('-') || word == "-" {
            return Err(format!("paths must come before the expression: {word}"));
        }
        let Some(primary) = named(word) else {
            return Err(format!("unknown primary {word}"));
        };
        let levels = |value: &str| {
            value
                .parse::<usize>()
                .map_err(|_| format!("invalid value {value} for {word}"))
        };
        let compare = |value: &str| match Compare::read(value) {
            Some((compare, "")) => Ok(compare),
            _ => Err(format!("invalid value {value} for {word}")),
        };
        Ok(match primary.name {
            "name" | "iname" => {
                let fold = primary.name == "iname";
                Expr::Name(Pattern::new(&self.value(word)?, fold))
            }
            "path" => Expr::Path(Pattern::new(&self.value(word)?, false)),
            "type" => match self.value(word)?.as_str() {
                "f" => Expr::Type(Type::File),
                "d" => Expr::Type(Type::Folder),
                "l" => Expr::Type(Type::Link),
                "s" => Expr::Type(Type::Socket),
                other => return Err(format!("invalid value {other} for -type: f, d, l or s")),
            },
            "size" => {
                let value = self.value(word)?;
                let read = Compare::read(&value)
                    .and_then(|(c, unit)| Some((c, size::parse(&format!("1{unit}"))?)));
                match read {
                    Some((compare, unit)) if compare.n >= 0 => Expr::Size(compare, unit),
                    _ => return Err(format!("invalid value {value} for -size")),
                }
            }
            "mtime" => Expr::Age(compare(&self.value(word)?)?, 86_400),
            "mmin" => Expr::Age(compare(&self.value(word)?)?, 60),
            "newer" => {
                let file = self.value(word)?;
                self.expression.newer.push(file);
                Expr::Newer(self.expression.newer.len() - 1)
            }
            "empty" => Expr::Empty,
            "maxdepth" => {
                self.expression.maxdepth = Some(levels(&self.value(word)?)?);
                Expr::True
            }
            "mindepth" => {
                self.expression.mindepth = levels(&self.value(word)?)?;
                Expr::True
            }
            "print" | "print0" => {
                self.acts = true;
                Expr::Print(primary.name == "print0")
            }
            "delete" => {
                self.acts = true;
                self.expression.deletes = true;
                Expr::Delete
            }
            "exec" => {
                self.acts = true;
                self.exec()?
            }
            // `-not`, `-a` and `-o` are read as operators.
            _ => return Err(format!("unexpected {word}")),
        })
    }

    /// The command of `-exec`, up to `;`, or up to `{} +` for a batch.
    fn exec(&mut self) -> Result<Expr, String> {
        let mut command: Vec<String> = Vec::new();
        loop {
            let Some(word) = self.next().map(str::to_string) else {
                return Err("-exec needs a ; or {} + at its end".into());
            };
            let batch = word == "+" && command.last().is_some_and(|w| w == "{}");
            if word == ";" || batch {
                if batch {
                    command.pop();
                }
                if command.is_empty() {
                    return Err("-exec needs a command".into());
                }
                if !batch {
                    return Ok(Expr::Exec(command));
                }
                self.expression.batches.push(command);
                return Ok(Expr::Batch(self.expression.batches.len() - 1));
            }
            command.push(word);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn words(text: &str) -> Vec<String> {
        text.split_whitespace().map(str::to_string).collect()
    }

    fn parsed(text: &str) -> Expression {
        parse(&words(text)).unwrap()
    }

    fn invalid(text: &str) -> String {
        parse(&words(text)).unwrap_err()
    }

    /// A world that records what was done, at the time 1000000.
    #[derive(Default)]
    struct Record {
        done: Vec<String>,
        empty: bool,
        exits: bool,
    }

    impl World for Record {
        fn empty_folder(&mut self, _: &Entry) -> bool {
            self.empty
        }
        fn reference(&self, _: usize) -> (i64, u32) {
            (500, 5)
        }
        fn now(&self) -> i64 {
            1_000_000
        }
        fn print(&mut self, entry: &Entry, nul: bool) {
            let end = if nul { "\\0" } else { "" };
            self.done.push(format!("print {}{end}", entry.path));
        }
        fn delete(&mut self, entry: &Entry) -> bool {
            self.done.push(format!("delete {}", entry.path));
            true
        }
        fn exec(&mut self, command: &[String], entry: &Entry) -> bool {
            self.done
                .push(format!("exec {} {}", command.join(" "), entry.path));
            self.exits
        }
        fn batch(&mut self, index: usize, entry: &Entry) {
            self.done.push(format!("batch {index} {}", entry.path));
        }
    }

    fn file(path: &str, size: u64, modified: i64) -> Entry {
        Entry {
            path: path.into(),
            name: last_component(path),
            kind: Type::File,
            size,
            modified: (modified, 0),
        }
    }

    /// What deciding `text` for `entry` did, and whether it was true.
    fn decide(text: &str, entry: &Entry) -> (bool, Vec<String>) {
        let mut world = Record::default();
        let true_ = parsed(text).root.decide(entry, &mut world);
        (true_, world.done)
    }

    fn holds(text: &str, entry: &Entry) -> bool {
        decide(&format!("{text} -print"), entry).0
    }

    #[test]
    fn decides_tests_for_an_entry() {
        let entry = file("./d/a.txt", 1500, 1_000_000 - 3 * 86_400 - 5);
        assert!(holds("-name a.txt", &entry) && holds("-iname A.*", &entry));
        assert!(!holds("-name d", &entry) && holds("-path ./d/*", &entry));
        assert!(holds("-type f", &entry) && !holds("-type d", &entry));
        assert!(holds("-size 1500", &entry) && holds("-size +1K", &entry));
        assert!(
            holds("-size 2K", &entry)
                && holds("-size -3KB", &entry)
                && !holds("-size -2KB", &entry)
        );
        assert!(!holds("-size -1M", &entry) && holds("-size 1M", &entry));
        assert!(holds("-mtime 3", &entry) && holds("-mtime +2", &entry));
        assert!(holds("-mtime -4", &entry) && !holds("-mtime -3", &entry));
        assert!(holds("-mmin +4319", &entry) && holds("-mmin 4320", &entry));
        assert!(holds("-newer x", &entry) && !holds("-newer x", &file("a", 0, 500)));
        assert!(!holds("-empty", &entry) && holds("-empty", &file("e", 0, 0)));
        let folder = Entry {
            kind: Type::Folder,
            ..entry.clone()
        };
        let mut world = Record {
            empty: true,
            ..Record::default()
        };
        assert!(parsed("-empty").root.decide(&folder, &mut world));
    }

    #[test]
    fn reads_operators_by_precedence() {
        let entry = file("a.txt", 0, 0);
        assert!(holds("-name x -o -name a.txt", &entry));
        assert!(!holds("-name x -o -name a.txt -type d", &entry));
        assert!(holds("( -name x -o -name a.txt ) -type f", &entry));
        assert!(holds("! -name x -a -not -type d", &entry));
        assert!(holds("! ! -name a.txt", &entry));
        assert!(!holds("-not ( -type f )", &entry));
    }

    #[test]
    fn acts_left_to_right_and_prints_without_actions() {
        let entry = file("./a", 0, 0);
        assert_eq!(decide("", &entry), (true, vec!["print ./a".into()]));
        assert_eq!(decide("-name b", &entry), (false, vec![]));
        assert_eq!(
            decide("-print0 -name b -o -delete", &entry),
            (true, vec!["print ./a\\0".into(), "delete ./a".into()])
        );
        assert_eq!(
            decide("-exec rm {} ; -print", &entry),
            (false, vec!["exec rm {} ./a".into()])
        );
        assert_eq!(
            decide("-exec ls -l {} + -exec x {} y ; -o -print", &entry),
            (
                true,
                vec![
                    "batch 0 ./a".into(),
                    "exec x {} y ./a".into(),
                    "print ./a".into()
                ]
            )
        );
        let expression = parsed("-exec ls -l {} + -delete -maxdepth 2 -mindepth 1");
        assert_eq!(expression.batches, [words("ls -l")]);
        assert!(expression.deletes);
        assert_eq!((expression.maxdepth, expression.mindepth), (Some(2), 1));
        assert_eq!(parsed("-newer a -newer b").newer, ["a", "b"]);
    }

    #[test]
    fn names_what_is_wrong() {
        assert_eq!(invalid("-nope"), "unknown primary -nope");
        assert_eq!(invalid("-name"), "-name needs a value");
        assert_eq!(
            invalid("-type x"),
            "invalid value x for -type: f, d, l or s"
        );
        assert_eq!(invalid("-size 1Q"), "invalid value 1Q for -size");
        assert_eq!(invalid("-mtime x"), "invalid value x for -mtime");
        assert_eq!(invalid("-maxdepth -1"), "invalid value -1 for -maxdepth");
        assert_eq!(invalid("-exec ls"), "-exec needs a ; or {} + at its end");
        assert_eq!(invalid("-exec ;"), "-exec needs a command");
        assert_eq!(invalid("-exec {} +"), "-exec needs a command");
        assert_eq!(invalid("( -print"), "( has no )");
        assert_eq!(invalid("( )"), "( ) holds no expression");
        assert_eq!(invalid("-print )"), "unexpected )");
        assert_eq!(invalid("-o -print"), "-o needs an expression before it");
        assert_eq!(invalid("-print -o"), "-o needs an expression after it");
        assert_eq!(invalid("-print -a"), "-a needs an expression after it");
        assert_eq!(invalid("!"), "! needs an expression after it");
        assert_eq!(
            invalid("-print x"),
            "paths must come before the expression: x"
        );
    }

    #[test]
    fn names_and_paths_of_entries() {
        assert_eq!(last_component("a/b/"), "b");
        assert_eq!(last_component("/"), "/");
        assert_eq!(last_component("."), ".");
        assert_eq!(child(".", "a"), "./a");
        assert_eq!(child("/", "a"), "/a");
        assert_eq!(
            file("./a \"b\"", 5, 300).json(),
            "{\"path\":\"./a \\\"b\\\"\",\"type\":\"file\",\"size\":5,\"modified\":\"1970-01-01T00:05:00Z\"}"
        );
    }
}
