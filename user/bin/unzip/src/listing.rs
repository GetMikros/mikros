//! What `unzip -l` writes (MIKSPEC-0098.R66): each entry's size,
//! modified time and name in columns, or with `--json` an array of
//! objects.

use crate::central::Record;
use mikros_cli::table::{Cell, columns};
use mikros_cli::{EntryType, Paint, Role, json_string, time};

/// The type of `record`'s entry, from its stored mode.
pub fn entry_type(record: &Record) -> EntryType {
    match (record.is_folder(), record.mode()) {
        (true, _) => EntryType::Folder,
        (false, Some(mode)) => EntryType::from_mode(mode),
        (false, None) => EntryType::Other,
    }
}

/// The entries `records` in columns under a header.
pub fn list(records: &[&Record], paint: Paint) -> String {
    let rows: Vec<Vec<Cell>> = records
        .iter()
        .map(|r| {
            vec![
                (r.size.to_string(), Some(Role::Number)),
                (time::local(r.modified()), Some(Role::Secondary)),
                (r.name.clone(), Some(Role::Path(entry_type(r)))),
            ]
        })
        .collect();
    columns(
        &["SIZE", "MODIFIED", "NAME"],
        &rows,
        &[true, false, false],
        paint,
    )
}

/// `records` as a JSON array of objects with `path`, `size`,
/// `compressed` and `modified`.
pub fn json(records: &[&Record]) -> String {
    let objects: Vec<String> = records
        .iter()
        .map(|r| {
            format!(
                "{{\"path\":{},\"size\":{},\"compressed\":{},\"modified\":{}}}",
                json_string(&r.name),
                r.size,
                r.compressed,
                json_string(&time::utc(r.modified())),
            )
        })
        .collect();
    format!("[{}]\n", objects.join(","))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::central::TIMESTAMP;

    fn record(name: &str, size: u64) -> Record {
        let mut extra = Vec::new();
        extra.extend(TIMESTAMP.to_le_bytes());
        extra.extend(5u16.to_le_bytes());
        extra.push(1);
        extra.extend(1_700_000_000i32.to_le_bytes());
        Record {
            name: name.into(),
            size,
            compressed: size / 2,
            extra,
            ..Record::default()
        }
    }

    #[test]
    fn lists_in_columns() {
        let (a, b) = (record("a", 5), record("dir/b", 12345));
        let text = list(&[&a, &b], Paint::never());
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines[0], " SIZE  MODIFIED          NAME");
        assert!(
            lines[1].starts_with("    5  ") && lines[1].ends_with("  a"),
            "{}",
            lines[1]
        );
        assert!(lines[2].starts_with("12345  ") && lines[2].ends_with("  dir/b"));
    }

    #[test]
    fn writes_json() {
        let a = record("a", 5);
        assert_eq!(
            json(&[&a]),
            "[{\"path\":\"a\",\"size\":5,\"compressed\":2,\"modified\":\"2023-11-14T22:13:20Z\"}]\n"
        );
    }
}
