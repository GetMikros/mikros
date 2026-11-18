//! What ls takes, and how it orders and writes entries: names in columns
//! or one per line, long lines, or JSON (MIKSPEC-0098.R34).

use mikros_cli::{Kind, Operand, Opt, Paint, Role, Run, Spec, json_string, size, time};

pub static COMMAND: Spec = Spec {
    operands: &[Operand::any(
        "path",
        Kind::Path,
        "the folders to list and files to name",
    )],
    options: &[
        Opt::both(
            "all",
            'a',
            "show entries whose names start with a dot, and . and ..",
        ),
        Opt::both(
            "almost-all",
            'A',
            "show entries whose names start with a dot",
        ),
        Opt::both(
            "directory",
            'd',
            "show folders themselves, not their entries",
        ),
        Opt::both("recursive", 'R', "list the folders inside, too"),
        Opt::short('1', "one entry per line"),
        Opt::both("reverse", 'r', "reverse the order"),
        Opt::short('t', "order by modified time, newest first"),
        Opt::short('S', "order by size, largest first"),
        Opt::both(
            "classify",
            'F',
            "mark folders with /, links with @ and sockets with =",
        ),
        Opt::both("inode", 'i', "show inode numbers"),
        Opt::both("human-readable", 'h', "show sizes with K, M and G"),
        Opt::short(
            'l',
            "long lines: type, access, size, modified time and name",
        ),
        Opt::long("json", "write the entries as a JSON array of objects"),
    ],
    ..Spec::new(
        "ls",
        "Lists the folders named, or the current one, and names the files named.\n\
         Long lines show what you may do with each entry, not owners or modes.",
    )
};

/// What an entry is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Type {
    File,
    Folder,
    Link,
    Socket,
}

impl Type {
    /// The letter of a long line.
    fn letter(self) -> char {
        match self {
            Type::File => '-',
            Type::Folder => 'd',
            Type::Link => 'l',
            Type::Socket => 's',
        }
    }

    fn word(self) -> &'static str {
        match self {
            Type::File => "file",
            Type::Folder => "folder",
            Type::Link => "link",
            Type::Socket => "socket",
        }
    }

    /// The mark of `-F`.
    fn mark(self) -> &'static str {
        match self {
            Type::File => "",
            Type::Folder => "/",
            Type::Link => "@",
            Type::Socket => "=",
        }
    }
}

/// An entry to show.
#[derive(Clone, Debug, PartialEq)]
pub struct Entry {
    pub name: String,
    pub kind: Type,
    pub size: u64,
    /// Seconds since 1970.
    pub modified: i64,
    pub inode: u64,
    /// What the caller may do, as `rwx`, when it is known.
    pub access: Option<String>,
    /// Whether it has an extended access control list.
    pub acl: bool,
    /// A link's target.
    pub target: Option<String>,
}

/// How entries are shown, from the options.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Show {
    pub long: bool,
    pub one: bool,
    pub classify: bool,
    pub inode: bool,
    pub human: bool,
    pub json: bool,
}

impl Show {
    pub fn of(run: &Run) -> Show {
        Show {
            long: run.has("-l"),
            one: run.has("-1"),
            classify: run.has("-F"),
            inode: run.has("-i"),
            human: run.has("-h"),
            json: run.has("--json"),
        }
    }
}

/// Which entries of a folder to show: all, all but `.` and `..`, or
/// those not starting with a dot.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Hidden {
    All,
    AlmostAll,
    None,
}

impl Hidden {
    pub fn of(run: &Run) -> Hidden {
        match (run.has("-a"), run.has("-A")) {
            (true, _) => Hidden::All,
            (false, true) => Hidden::AlmostAll,
            _ => Hidden::None,
        }
    }

    pub fn shows(self, name: &str) -> bool {
        self != Hidden::None || !name.starts_with('.')
    }
}

/// Orders `entries` by name, or by modified time or size with ties by
/// name, reversed with `-r`.
pub fn order(entries: &mut [Entry], run: &Run) {
    if run.has("-t") {
        entries.sort_by(|a, b| {
            b.modified
                .cmp(&a.modified)
                .then_with(|| a.name.cmp(&b.name))
        });
    } else if run.has("-S") {
        entries.sort_by(|a, b| b.size.cmp(&a.size).then_with(|| a.name.cmp(&b.name)));
    } else {
        entries.sort_by(|a, b| a.name.cmp(&b.name));
    }
    if run.has("-r") {
        entries.reverse();
    }
}

/// The name as shown, with its mark under `-F`, and its inode under `-i`,
/// and the width it takes.
fn shown_name(entry: &Entry, show: Show, paint: Paint) -> (String, usize) {
    let mark = if show.classify { entry.kind.mark() } else { "" };
    let mut text = String::new();
    let mut width = 0;
    if show.inode {
        let inode = entry.inode.to_string();
        width += inode.len() + 1;
        text.push_str(&paint.paint(Role::Number, &inode));
        text.push(' ');
    }
    width += entry.name.chars().count() + mark.len();
    text.push_str(&paint.paint(Role::Path, &entry.name));
    text.push_str(mark);
    (text, width)
}

/// The size as shown.
fn shown_size(size: u64, human: bool) -> String {
    match human {
        true => size::human(size),
        false => size.to_string(),
    }
}

/// The long lines of `entries`: type, access, `+` or a space, size,
/// modified time and name, with `-> target` for links.
pub fn long(entries: &[Entry], show: Show, paint: Paint) -> String {
    let sizes: Vec<String> = entries
        .iter()
        .map(|e| shown_size(e.size, show.human))
        .collect();
    let width = sizes.iter().map(String::len).max().unwrap_or(0);
    let mut text = String::new();
    for (entry, size) in entries.iter().zip(&sizes) {
        let access = entry.access.as_deref().unwrap_or("???");
        let acl = if entry.acl { '+' } else { ' ' };
        let (name, _) = shown_name(entry, show, paint);
        text.push(entry.kind.letter());
        text.push_str(access);
        text.push(acl);
        text.push(' ');
        text.push_str(&paint.paint(Role::Number, &format!("{size:>width$}")));
        text.push(' ');
        text.push_str(&paint.paint(Role::Secondary, &time::local(entry.modified)));
        text.push(' ');
        text.push_str(&name);
        if let Some(target) = &entry.target {
            text.push_str(" -> ");
            text.push_str(&paint.paint(Role::Path, target));
        }
        text.push('\n');
    }
    text
}

/// The names of `entries` in columns that fit `columns`, ordered down
/// each column, or one per line without `columns`.
pub fn names(entries: &[Entry], show: Show, paint: Paint, columns: Option<usize>) -> String {
    let shown: Vec<(String, usize)> = entries.iter().map(|e| shown_name(e, show, paint)).collect();
    let Some(columns) = columns.filter(|_| !show.one) else {
        return shown.iter().map(|(n, _)| format!("{n}\n")).collect();
    };
    if shown.is_empty() {
        return String::new();
    }
    // The fewest rows whose columns, two spaces apart, fit.
    let mut rows = 1;
    let widths = loop {
        let count = shown.len().div_ceil(rows);
        let widths: Vec<usize> = (0..count)
            .map(|c| {
                shown[c * rows..((c + 1) * rows).min(shown.len())]
                    .iter()
                    .map(|(_, w)| *w)
                    .max()
                    .unwrap_or(0)
            })
            .collect();
        let total = widths.iter().sum::<usize>() + 2 * widths.len().saturating_sub(1);
        if total <= columns || rows >= shown.len() {
            break widths;
        }
        rows += 1;
    };
    let mut text = String::new();
    for row in 0..rows {
        let mut line = String::new();
        let mut pad = 0;
        for (c, width) in widths.iter().enumerate() {
            let Some((name, w)) = shown.get(c * rows + row) else {
                break;
            };
            line.push_str(&" ".repeat(pad));
            line.push_str(name);
            pad = width - w + 2;
        }
        text.push_str(&line);
        text.push('\n');
    }
    text
}

/// `entries` as a JSON array of objects with `name`, `type`, `size`,
/// `modified`, `access` and `acl`, on one line.
pub fn json(entries: &[Entry]) -> String {
    let objects: Vec<String> = entries
        .iter()
        .map(|e| {
            let access = match &e.access {
                Some(a) => json_string(a),
                None => "null".into(),
            };
            format!(
                "{{\"name\":{},\"type\":\"{}\",\"size\":{},\"modified\":{},\"access\":{access},\"acl\":{}}}",
                json_string(&e.name),
                e.kind.word(),
                e.size,
                json_string(&time::utc(e.modified)),
                e.acl
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

    fn entry(name: &str, kind: Type, size: u64, modified: i64) -> Entry {
        Entry {
            name: name.into(),
            kind,
            size,
            modified,
            inode: 7,
            access: Some("rw-".into()),
            acl: false,
            target: None,
        }
    }

    fn sample() -> Vec<Entry> {
        vec![
            entry("b", Type::File, 10, 300),
            entry("a", Type::Folder, 4096, 100),
            entry("c", Type::File, 2000, 200),
        ]
    }

    fn order_of(arguments: &[&str]) -> Vec<String> {
        let mut entries = sample();
        order(&mut entries, &run(arguments));
        entries.into_iter().map(|e| e.name).collect()
    }

    #[test]
    fn orders_by_name_time_and_size() {
        assert_eq!(order_of(&[]), ["a", "b", "c"]);
        assert_eq!(order_of(&["-r"]), ["c", "b", "a"]);
        assert_eq!(order_of(&["-t"]), ["b", "c", "a"]);
        assert_eq!(order_of(&["-S"]), ["a", "c", "b"]);
        assert_eq!(order_of(&["-Sr"]), ["b", "c", "a"]);
    }

    #[test]
    fn hides_dot_entries_unless_asked() {
        assert!(!Hidden::of(&run(&[])).shows(".x"));
        assert!(Hidden::of(&run(&["-A"])).shows(".x"));
        assert_eq!(Hidden::of(&run(&["-a"])), Hidden::All);
    }

    #[test]
    fn writes_long_lines() {
        let mut entries = sample();
        entries[0].acl = true;
        entries[1].access = None;
        let mut link = entry("l", Type::Link, 3, 100);
        link.target = Some("b".into());
        entries.push(link);
        let text = long(&entries, Show::default(), Paint::never());
        let lines: Vec<&str> = text.lines().collect();
        assert!(lines[0].starts_with("-rw-+   10 "), "{}", lines[0]);
        assert!(lines[0].ends_with(" b"));
        assert!(lines[1].starts_with("d???  4096 "));
        assert!(lines[2].starts_with("-rw-  2000 "));
        assert!(lines[3].starts_with("lrw-     3 ") && lines[3].ends_with(" l -> b"));
        let human = Show {
            human: true,
            classify: true,
            ..Show::default()
        };
        let text = long(&entries[1..2], human, Paint::never());
        assert!(
            text.starts_with("d???  4.0K ") && text.ends_with(" a/\n"),
            "{text}"
        );
    }

    #[test]
    fn writes_names_in_columns_down_each_one() {
        let entries: Vec<Entry> = ["aa", "b", "cccc", "d", "e"]
            .iter()
            .map(|n| entry(n, Type::File, 0, 0))
            .collect();
        let show = Show::default();
        assert_eq!(
            names(&entries, show, Paint::never(), Some(80)),
            "aa  b  cccc  d  e\n"
        );
        assert_eq!(
            names(&entries, show, Paint::never(), Some(12)),
            "aa  cccc  e\nb   d\n"
        );
        assert_eq!(
            names(&entries, show, Paint::never(), Some(1)),
            "aa\nb\ncccc\nd\ne\n"
        );
        assert_eq!(
            names(&entries, show, Paint::never(), None),
            "aa\nb\ncccc\nd\ne\n"
        );
        let one = Show { one: true, ..show };
        assert_eq!(
            names(&entries[..2], one, Paint::never(), Some(80)),
            "aa\nb\n"
        );
        let marked = Show {
            classify: true,
            inode: true,
            ..show
        };
        let mut folder = entry("f", Type::Folder, 0, 0);
        folder.inode = 12;
        assert_eq!(names(&[folder], marked, Paint::never(), None), "12 f/\n");
    }

    #[test]
    fn writes_json() {
        let mut entries = sample();
        entries[1].access = None;
        assert_eq!(
            json(&entries[..2]),
            "[{\"name\":\"b\",\"type\":\"file\",\"size\":10,\"modified\":\"1970-01-01T00:05:00Z\",\
             \"access\":\"rw-\",\"acl\":false},{\"name\":\"a\",\"type\":\"folder\",\"size\":4096,\
             \"modified\":\"1970-01-01T00:01:40Z\",\"access\":null,\"acl\":false}]\n"
        );
    }

    #[test]
    fn declares_what_it_takes() {
        let file = concat!(env!("CARGO_MANIFEST_DIR"), "/ls.msh");
        mikros_cli::declared(&COMMAND, std::path::Path::new(file)).unwrap();
    }
}
