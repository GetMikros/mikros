//! What `tar -t` writes (MIKSPEC-0098.R57): the names of the entries,
//! with `-v` long lines of each entry's type, size, modified time and
//! name, and with `--json` an array of objects.

use crate::header::{Entry, Kind};
use mikros_cli::{Paint, Role, json_string, time};

/// The names of `entries`, one per line.
pub fn names(entries: &[Entry], paint: Paint) -> String {
    entries
        .iter()
        .map(|e| format!("{}\n", paint.paint(Role::Path, &e.path)))
        .collect()
}

/// The long lines of `entries`: the type's letter, the size, the
/// modified time and the name, with `-> target` for links and hard
/// links.
pub fn long(entries: &[Entry], paint: Paint) -> String {
    let width = entries
        .iter()
        .map(|e| e.size.to_string().len())
        .max()
        .unwrap_or(0);
    let mut text = String::new();
    for entry in entries {
        text.push(entry.kind.letter());
        text.push(' ');
        let size = format!("{:>width$}", entry.size);
        text.push_str(&paint.paint(Role::Number, &size));
        text.push(' ');
        text.push_str(&paint.paint(Role::Secondary, &time::local(entry.modified)));
        text.push(' ');
        text.push_str(&paint.paint(Role::Path, &entry.path));
        if matches!(entry.kind, Kind::Link | Kind::Hard) {
            text.push_str(" -> ");
            text.push_str(&paint.paint(Role::Path, &entry.target));
        }
        text.push('\n');
    }
    text
}

/// `entries` as a JSON array of objects with `path`, `type`, `size`,
/// `modified` and `target`, null but for links and hard links.
pub fn json(entries: &[Entry]) -> String {
    let objects: Vec<String> = entries
        .iter()
        .map(|e| {
            let target = match e.kind {
                Kind::Link | Kind::Hard => json_string(&e.target),
                _ => "null".into(),
            };
            format!(
                "{{\"path\":{},\"type\":\"{}\",\"size\":{},\"modified\":{},\"target\":{target}}}",
                json_string(&e.path),
                e.kind.word(),
                e.size,
                json_string(&time::utc(e.modified)),
            )
        })
        .collect();
    format!("[{}]\n", objects.join(","))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::reader::tests::entry;

    fn sample() -> Vec<Entry> {
        let mut link = entry("l", Kind::Link, 0);
        link.target = "f".into();
        vec![
            entry("d/", Kind::Folder, 0),
            entry("d/f", Kind::File, 1234),
            link,
        ]
    }

    #[test]
    fn writes_names_and_long_lines() {
        assert_eq!(names(&sample(), Paint::never()), "d/\nd/f\nl\n");
        let text = long(&sample(), Paint::never());
        let lines: Vec<&str> = text.lines().collect();
        assert!(
            lines[0].starts_with("d    0 ") && lines[0].ends_with(" d/"),
            "{}",
            lines[0]
        );
        assert!(lines[1].starts_with("- 1234 ") && lines[1].ends_with(" d/f"));
        assert!(lines[2].starts_with("l    0 ") && lines[2].ends_with(" l -> f"));
        assert_eq!(lines[1].len(), "- 1234 YYYY-MM-DD HH:MM d/f".len());
    }

    #[test]
    fn writes_json() {
        assert_eq!(
            json(&sample()[1..]),
            "[{\"path\":\"d/f\",\"type\":\"file\",\"size\":1234,\"modified\":\"2023-11-14T22:13:20Z\",\
             \"target\":null},{\"path\":\"l\",\"type\":\"link\",\"size\":0,\
             \"modified\":\"2023-11-14T22:13:20Z\",\"target\":\"f\"}]\n"
        );
    }
}
