//! The lines of sums that the sum commands write and check, by the
//! algorithm each uses (MIKSPEC-0098.R51).

use crate::Algorithm;
use mikros_cli::{Paint, Role, json_string};

/// How lines of sums are written.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Form {
    pub binary: bool,
    pub tag: bool,
    /// Lines end with NUL and names are not escaped.
    pub zero: bool,
}

/// `name` with backslashes, line feeds and carriage returns escaped, and
/// whether any was.
pub fn escape(name: &[u8]) -> (bool, Vec<u8>) {
    let mut out = Vec::with_capacity(name.len());
    let mut escaped = false;
    for &b in name {
        let with = match b {
            b'\\' => b'\\',
            b'\n' => b'n',
            b'\r' => b'r',
            _ => {
                out.push(b);
                continue;
            }
        };
        out.extend_from_slice(&[b'\\', with]);
        escaped = true;
    }
    (escaped, out)
}

/// The line of the sum `hash` of `name` by `algorithm`, with its end.
pub fn line(algorithm: &Algorithm, hash: &str, name: &[u8], form: Form, paint: Paint) -> Vec<u8> {
    let (escaped, shown) = match form.zero {
        true => (false, name.to_vec()),
        false => escape(name),
    };
    let shown = paint_bytes(paint, Role::Path, &shown);
    let hash = paint.paint(Role::Number, hash);
    let mut out = Vec::new();
    if escaped {
        out.push(b'\\');
    }
    if form.tag {
        out.extend_from_slice(format!("{} (", algorithm.name).as_bytes());
        out.extend_from_slice(&shown);
        out.extend_from_slice(b") = ");
        out.extend_from_slice(hash.as_bytes());
    } else {
        out.extend_from_slice(hash.as_bytes());
        out.extend_from_slice(if form.binary { b" *" } else { b"  " });
        out.extend_from_slice(&shown);
    }
    out.push(if form.zero { 0 } else { b'\n' });
    out
}

/// `bytes` in the colour of `role`, when `paint` colours.
pub fn paint_bytes(paint: Paint, role: Role, bytes: &[u8]) -> Vec<u8> {
    match std::str::from_utf8(bytes) {
        Ok(text) => paint.paint(role, text).into_bytes(),
        Err(_) => bytes.to_vec(),
    }
}

/// The sums of `rows`, each a path and its sum, as a JSON array of
/// objects with `path` and `hash`, on one line.
pub fn json(rows: &[(String, String)]) -> String {
    let objects: Vec<String> = rows
        .iter()
        .map(|(path, hash)| format!("{{\"path\":{},\"hash\":\"{hash}\"}}", json_string(path)))
        .collect();
    format!("[{}]\n", objects.join(","))
}

/// A line of a list of sums: the sum and the name of the file.
#[derive(Debug, PartialEq, Eq)]
pub struct Listed {
    pub hash: String,
    pub name: Vec<u8>,
}

/// What a line of a list says, `None` when it is not a line of sums.
/// A line may be of either form, may start with blanks, and starts with
/// a backslash when its name is escaped.
pub fn parse(algorithm: &Algorithm, line: &[u8]) -> Option<Listed> {
    let digits = algorithm.digits;
    let line = line.strip_suffix(b"\r").unwrap_or(line);
    let start = line.iter().position(|b| !matches!(b, b' ' | b'\t'))?;
    let mut line = &line[start..];
    let escaped = line.first() == Some(&b'\\');
    if escaped {
        line = &line[1..];
    }
    let prefix = format!("{} (", algorithm.name);
    let (hash, name) = match line.strip_prefix(prefix.as_bytes()) {
        Some(rest) => {
            let at = rest.windows(4).rposition(|w| w == b") = ")?;
            (&rest[at + 4..], &rest[..at])
        }
        None => {
            if line.len() < digits + 2 || !matches!(line[digits + 1], b' ' | b'*') {
                return None;
            }
            if line[digits] != b' ' {
                return None;
            }
            (&line[..digits], &line[digits + 2..])
        }
    };
    if hash.len() != digits || !hash.iter().all(u8::is_ascii_hexdigit) || name.is_empty() {
        return None;
    }
    let name = match escaped {
        true => unescape(name)?,
        false => name.to_vec(),
    };
    Some(Listed {
        hash: String::from_utf8_lossy(hash).to_ascii_lowercase(),
        name,
    })
}

fn unescape(name: &[u8]) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(name.len());
    let mut bytes = name.iter();
    while let Some(&b) = bytes.next() {
        if b != b'\\' {
            out.push(b);
            continue;
        }
        out.push(match bytes.next()? {
            b'\\' => b'\\',
            b'n' => b'\n',
            b'r' => b'\r',
            _ => return None,
        });
    }
    Some(out)
}

/// What checking one list found.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Tally {
    /// Lines of sums.
    pub listed: usize,
    /// Lines that were not lines of sums.
    pub improper: usize,
    /// Files whose sums differed.
    pub mismatched: usize,
    /// Files that could not be read.
    pub unread: usize,
}

impl Tally {
    /// The warnings that end the check of a list.
    pub fn warnings(&self) -> Vec<String> {
        let mut warnings = Vec::new();
        let counted = |n: usize, one: &str, many: &str| match n {
            1 => format!("WARNING: 1 {one}"),
            n => format!("WARNING: {n} {many}"),
        };
        if self.improper > 0 {
            warnings.push(counted(
                self.improper,
                "line is improperly formatted",
                "lines are improperly formatted",
            ));
        }
        if self.unread > 0 {
            warnings.push(counted(
                self.unread,
                "listed file could not be read",
                "listed files could not be read",
            ));
        }
        if self.mismatched > 0 {
            warnings.push(counted(
                self.mismatched,
                "computed checksum did NOT match",
                "computed checksums did NOT match",
            ));
        }
        warnings
    }
}

/// The options that only `-c` takes.
pub const CHECKING: [&str; 4] = ["--quiet", "--status", "--strict", "--warn"];

#[cfg(test)]
mod tests {
    use super::*;

    const ABC: &str = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad";

    static NOTHING: mikros_cli::Spec = mikros_cli::Spec::new("sums", "");
    static SHA256: Algorithm = Algorithm {
        name: "SHA256",
        digits: 64,
        digest: |_| Ok(String::new()),
        command: &NOTHING,
    };

    fn line(hash: &str, name: &[u8], form: Form, paint: Paint) -> Vec<u8> {
        super::line(&SHA256, hash, name, form, paint)
    }

    fn parse(line: &[u8]) -> Option<Listed> {
        super::parse(&SHA256, line)
    }

    #[test]
    fn writes_hexadecimal() {
        assert_eq!(crate::hex(&[0x0f, 0xa0]), "0fa0");
    }

    #[test]
    fn writes_lines_in_both_forms() {
        let line = |name: &[u8], form| String::from_utf8(line("ab", name, form, Paint::never()));
        let text = Form::default();
        assert_eq!(line(b"f", text).unwrap(), "ab  f\n");
        let binary = Form {
            binary: true,
            ..text
        };
        assert_eq!(line(b"f", binary).unwrap(), "ab *f\n");
        let tag = Form { tag: true, ..text };
        assert_eq!(line(b"f", tag).unwrap(), "SHA256 (f) = ab\n");
        assert_eq!(line(b"a\nb\\", text).unwrap(), "\\ab  a\\nb\\\\\n");
        let zero = Form { zero: true, ..text };
        assert_eq!(line(b"a\nb", zero).unwrap(), "ab  a\nb\0");
    }

    #[test]
    fn reads_lines_back() {
        let listed = |name: &[u8]| {
            Some(Listed {
                hash: ABC.into(),
                name: name.to_vec(),
            })
        };
        assert_eq!(parse(format!("{ABC}  f").as_bytes()), listed(b"f"));
        assert_eq!(parse(format!("{ABC} *a b\r").as_bytes()), listed(b"a b"));
        assert_eq!(
            parse(format!("  SHA256 (x) = y) = {ABC}").as_bytes()),
            listed(b"x) = y")
        );
        assert_eq!(
            parse(format!("{}  f", ABC.to_uppercase()).as_bytes()),
            listed(b"f")
        );
        assert_eq!(
            parse(format!("\\{ABC}  a\\nb\\\\").as_bytes()),
            listed(b"a\nb\\")
        );
        let bad = |line: String| assert_eq!(parse(line.as_bytes()), None, "{line}");
        bad(format!("{ABC} f"));
        bad(format!("{ABC}  "));
        bad(format!("{}  f", &ABC[1..]));
        bad(format!("\\{ABC}  a\\x"));
        bad("MD5 (f) = 00".into());
        bad(String::new());
    }

    #[test]
    fn warns_with_counts() {
        let tally = Tally {
            listed: 3,
            improper: 1,
            mismatched: 2,
            unread: 0,
        };
        assert_eq!(
            tally.warnings(),
            [
                "WARNING: 1 line is improperly formatted",
                "WARNING: 2 computed checksums did NOT match"
            ]
        );
        assert!(Tally::default().warnings().is_empty());
    }

    #[test]
    fn writes_json() {
        assert_eq!(
            json(&[("a".into(), "01".into()), ("-".into(), "02".into())]),
            "[{\"path\":\"a\",\"hash\":\"01\"},{\"path\":\"-\",\"hash\":\"02\"}]\n"
        );
    }
}
