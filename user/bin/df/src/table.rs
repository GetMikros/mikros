//! What df takes, and how it writes the volumes: aligned columns under a
//! header line, or JSON (MIKSPEC-0098.R39).

use mikros_cli::table::{Cell, columns};
use mikros_cli::{Kind, Operand, Opt, Paint, Role, Spec, json_string, size};

pub static COMMAND: Spec = Spec {
    operands: &[Operand::any(
        "path",
        Kind::Path,
        "show the volumes that hold these paths",
    )],
    options: &[
        Opt::both("human-readable", 'h', "show sizes with K, M and G"),
        Opt::both(
            "print-type",
            'T',
            "show the file system type, which is always shown",
        ),
        Opt::long("json", "write the volumes as a JSON array of objects"),
    ],
    ..Spec::new(
        "df",
        "Shows each volume of your namespace, or those that hold the paths\n\
         named: its path, file system type, label, volume ID, size, used and\n\
         free space, and whether it is read-only.",
    )
};

/// A volume, as its file system describes it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Volume {
    /// Where it is in the namespace.
    pub path: String,
    /// The type of its file system.
    pub kind: String,
    pub label: String,
    pub id: String,
    /// Its size and free space, in bytes.
    pub size: u64,
    pub free: u64,
    pub read_only: bool,
}

impl Volume {
    pub fn used(&self) -> u64 {
        self.size.saturating_sub(self.free)
    }

    /// Whether it describes the same volume as `other`, by everything
    /// but the path.
    pub fn same(&self, other: &Volume) -> bool {
        Volume {
            path: String::new(),
            ..self.clone()
        } == Volume {
            path: String::new(),
            ..other.clone()
        }
    }
}

/// A cell of text in `role`, or `-` in the secondary colour when empty.
fn cell(text: &str, role: Role) -> Cell {
    match text.is_empty() {
        true => ("-".into(), Some(Role::Secondary)),
        false => (text.into(), Some(role)),
    }
}

/// `bytes` as the table shows them: with a suffix for `-h`, and in units
/// of 1024 bytes, rounded up, otherwise.
fn bytes(bytes: u64, human: bool) -> Cell {
    let text = match human {
        true => size::human(bytes),
        false => bytes.div_ceil(1024).to_string(),
    };
    (text, Some(Role::Number))
}

/// The volumes in aligned columns under a header line.
pub fn text(volumes: &[Volume], human: bool, paint: Paint) -> String {
    let size = if human { "SIZE" } else { "1K-BLOCKS" };
    let header = [
        "PATH",
        "TYPE",
        "LABEL",
        "ID",
        size,
        "USED",
        "FREE",
        "READ-ONLY",
    ];
    let rows: Vec<Vec<Cell>> = volumes
        .iter()
        .map(|v| {
            vec![
                cell(&v.path, Role::Path),
                cell(&v.kind, Role::String),
                cell(&v.label, Role::String),
                cell(&v.id, Role::Number),
                bytes(v.size, human),
                bytes(v.used(), human),
                bytes(v.free, human),
                (if v.read_only { "yes" } else { "no" }.into(), None),
            ]
        })
        .collect();
    let right = [false, false, false, false, true, true, true, false];
    columns(&header, &rows, &right, paint)
}

/// The name of a status, as `NotSupported`, in lowercase words, as
/// `not supported`.
pub fn words(name: &str) -> String {
    let mut text = String::new();
    for c in name.chars() {
        if c.is_ascii_uppercase() && !text.is_empty() {
            text.push(' ');
        }
        text.push(c.to_ascii_lowercase());
    }
    text
}

fn string_or_null(text: &str) -> String {
    match text.is_empty() {
        true => "null".into(),
        false => json_string(text),
    }
}

/// The volumes as a JSON array of objects with `path`, `type`, `label`
/// and `id`, `null` when empty, `size`, `used` and `free` in bytes, and
/// `read_only`, on one line.
pub fn json(volumes: &[Volume]) -> String {
    let objects: Vec<String> = volumes
        .iter()
        .map(|v| {
            format!(
                "{{\"path\":{},\"type\":{},\"label\":{},\"id\":{},\"size\":{},\"used\":{},\
                 \"free\":{},\"read_only\":{}}}",
                json_string(&v.path),
                string_or_null(&v.kind),
                string_or_null(&v.label),
                string_or_null(&v.id),
                v.size,
                v.used(),
                v.free,
                v.read_only
            )
        })
        .collect();
    format!("[{}]\n", objects.join(","))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Vec<Volume> {
        vec![
            Volume {
                path: "/system".into(),
                kind: "btrfs".into(),
                label: "Mikros".into(),
                id: "4f2a".into(),
                size: 10 << 30,
                free: 4 << 30,
                read_only: true,
            },
            Volume {
                path: "/tmp".into(),
                kind: "tmpfs".into(),
                label: String::new(),
                id: String::new(),
                size: 1 << 20,
                free: 1 << 19,
                read_only: false,
            },
        ]
    }

    #[test]
    fn writes_aligned_columns() {
        let text = text(&sample(), false, Paint::never());
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(
            lines[0],
            "PATH     TYPE   LABEL   ID    1K-BLOCKS     USED     FREE  READ-ONLY"
        );
        assert_eq!(
            lines[1],
            "/system  btrfs  Mikros  4f2a   10485760  6291456  4194304  yes"
        );
        assert_eq!(
            lines[2],
            "/tmp     tmpfs  -       -          1024      512      512  no"
        );
    }

    #[test]
    fn writes_suffixes_with_h() {
        let text = text(&sample()[1..], true, Paint::never());
        assert_eq!(
            text,
            "PATH  TYPE   LABEL  ID  SIZE  USED  FREE  READ-ONLY\n\
             /tmp  tmpfs  -      -   1.0M  512K  512K  no\n"
        );
    }

    #[test]
    fn writes_json() {
        assert_eq!(
            json(&sample()[1..]),
            "[{\"path\":\"/tmp\",\"type\":\"tmpfs\",\"label\":null,\"id\":null,\
             \"size\":1048576,\"used\":524288,\"free\":524288,\"read_only\":false}]\n"
        );
    }

    #[test]
    fn tells_the_same_volume() {
        let volumes = sample();
        let mut other = volumes[0].clone();
        other.path = "/apps".into();
        assert!(volumes[0].same(&other));
        assert!(!volumes[0].same(&volumes[1]));
    }

    #[test]
    fn names_statuses_in_words() {
        assert_eq!(words("NotSupported"), "not supported");
        assert_eq!(words("Io"), "io");
    }

    #[test]
    fn declares_what_it_takes() {
        let file = concat!(env!("CARGO_MANIFEST_DIR"), "/df.msh");
        mikros_cli::declared(&COMMAND, std::path::Path::new(file)).unwrap();
    }
}
