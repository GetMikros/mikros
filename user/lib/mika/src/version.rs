//! Package versions and version ranges (MIKSPEC-0101.R5), ordered as
//! Semantic Versioning 2.0.0 orders versions.

use alloc::string::String;
use alloc::vec::Vec;
use core::cmp::Ordering;
use core::fmt;

/// `<major>.<minor>.<patch>`, with a pre-release when it has one.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Version {
    pub major: u64,
    pub minor: u64,
    pub patch: u64,
    pub pre: Option<String>,
}

/// A decimal number without leading zeros.
fn number(text: &str) -> Option<u64> {
    let bytes = text.as_bytes();
    let plain = !bytes.is_empty()
        && bytes.iter().all(u8::is_ascii_digit)
        && (bytes.len() == 1 || bytes[0] != b'0');
    plain.then(|| text.parse().ok()).flatten()
}

impl Version {
    /// The version `text` names, or `None` when it is no version.
    pub fn parse(text: &str) -> Option<Version> {
        let (core, pre) = match text.split_once('-') {
            Some((core, pre)) => (core, Some(pre)),
            None => (text, None),
        };
        let mut parts = core.split('.');
        let major = number(parts.next()?)?;
        let minor = number(parts.next()?)?;
        let patch = number(parts.next()?)?;
        if parts.next().is_some() {
            return None;
        }
        // Pre-releases of letters, digits, `.` and `-`, whose identifiers
        // between dots are not empty, as Semantic Versioning orders them.
        let pre = match pre {
            None => None,
            Some(pre) => {
                let chars = pre
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-');
                if !chars || pre.split('.').any(str::is_empty) {
                    return None;
                }
                Some(String::from(pre))
            }
        };
        Some(Version {
            major,
            minor,
            patch,
            pre,
        })
    }
}

impl fmt::Display for Version {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}.{}", self.major, self.minor, self.patch)?;
        match &self.pre {
            Some(pre) => write!(f, "-{pre}"),
            None => Ok(()),
        }
    }
}

/// The order of two pre-release identifiers: numeric ones by value and
/// before all others, the others by their ASCII bytes.
fn identifier(a: &str, b: &str) -> Ordering {
    let numeric = |s: &str| {
        s.bytes()
            .all(|c| c.is_ascii_digit())
            .then(|| s.parse::<u64>().ok())
            .flatten()
    };
    match (numeric(a), numeric(b)) {
        (Some(x), Some(y)) => x.cmp(&y),
        (Some(_), None) => Ordering::Less,
        (None, Some(_)) => Ordering::Greater,
        (None, None) => a.cmp(b),
    }
}

impl Ord for Version {
    fn cmp(&self, other: &Version) -> Ordering {
        let core =
            (self.major, self.minor, self.patch).cmp(&(other.major, other.minor, other.patch));
        core.then_with(|| match (&self.pre, &other.pre) {
            (None, None) => Ordering::Equal,
            // A pre-release comes before its release.
            (Some(_), None) => Ordering::Less,
            (None, Some(_)) => Ordering::Greater,
            (Some(a), Some(b)) => {
                let (mut a, mut b) = (a.split('.'), b.split('.'));
                loop {
                    match (a.next(), b.next()) {
                        (None, None) => return Ordering::Equal,
                        (None, Some(_)) => return Ordering::Less,
                        (Some(_), None) => return Ordering::Greater,
                        (Some(x), Some(y)) => match identifier(x, y) {
                            Ordering::Equal => {}
                            order => return order,
                        },
                    }
                }
            }
        })
    }
}

impl PartialOrd for Version {
    fn partial_cmp(&self, other: &Version) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Comparison {
    Equal,
    Greater,
    GreaterOrEqual,
    Less,
    LessOrEqual,
}

/// One or more comparisons, all of which a version must satisfy.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Range(pub Vec<(Comparison, Version)>);

impl Range {
    /// The range `text` names, comparisons separated by `,`, or `None`.
    pub fn parse(text: &str) -> Option<Range> {
        let mut comparisons = Vec::new();
        for part in text.split(',') {
            let (comparison, rest) = if let Some(rest) = part.strip_prefix(">=") {
                (Comparison::GreaterOrEqual, rest)
            } else if let Some(rest) = part.strip_prefix("<=") {
                (Comparison::LessOrEqual, rest)
            } else if let Some(rest) = part.strip_prefix('>') {
                (Comparison::Greater, rest)
            } else if let Some(rest) = part.strip_prefix('<') {
                (Comparison::Less, rest)
            } else {
                (Comparison::Equal, part.strip_prefix('=')?)
            };
            comparisons.push((comparison, Version::parse(rest)?));
        }
        Some(Range(comparisons))
    }

    /// Whether `version` satisfies every comparison of the range.
    pub fn contains(&self, version: &Version) -> bool {
        self.0.iter().all(|(comparison, bound)| {
            let order = version.cmp(bound);
            match comparison {
                Comparison::Equal => order == Ordering::Equal,
                Comparison::Greater => order == Ordering::Greater,
                Comparison::GreaterOrEqual => order != Ordering::Less,
                Comparison::Less => order == Ordering::Less,
                Comparison::LessOrEqual => order != Ordering::Greater,
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::string::ToString;

    fn v(text: &str) -> Version {
        Version::parse(text).unwrap()
    }

    #[test]
    fn parses_versions() {
        assert_eq!(v("1.20.3").to_string(), "1.20.3");
        assert_eq!(v("0.0.0-rc.1-x").pre.as_deref(), Some("rc.1-x"));
        for bad in [
            "1.2",
            "1.2.3.4",
            "01.2.3",
            "1.02.3",
            "1.2.3-",
            "1.2.3-a..b",
            "1.2.3-a_b",
            "a.b.c",
            "1.2.3 ",
        ] {
            assert_eq!(Version::parse(bad), None, "{bad}");
        }
    }

    #[test]
    fn orders_versions_as_semantic_versioning() {
        // The example order of Semantic Versioning 2.0.0.
        let ordered = [
            "1.0.0-alpha",
            "1.0.0-alpha.1",
            "1.0.0-alpha.beta",
            "1.0.0-beta",
            "1.0.0-beta.2",
            "1.0.0-beta.11",
            "1.0.0-rc.1",
            "1.0.0",
            "1.0.1",
            "1.1.0",
            "2.0.0",
            "10.0.0",
        ];
        for pair in ordered.windows(2) {
            assert!(v(pair[0]) < v(pair[1]), "{} < {}", pair[0], pair[1]);
        }
        assert_eq!(v("1.0.0").cmp(&v("1.0.0")), Ordering::Equal);
    }

    #[test]
    fn checks_ranges() {
        let range = Range::parse(">=1.2.0,<2.0.0").unwrap();
        assert!(range.contains(&v("1.2.0")));
        assert!(range.contains(&v("1.9.9")));
        assert!(!range.contains(&v("2.0.0")));
        // A pre-release of 2.0.0 comes before 2.0.0.
        assert!(range.contains(&v("2.0.0-rc.1")));
        assert!(!range.contains(&v("1.1.9")));
        assert!(Range::parse("=1.0.0").unwrap().contains(&v("1.0.0")));
        assert!(Range::parse(">1.0.0").unwrap().contains(&v("1.0.1")));
        assert!(Range::parse("<=1.0.0").unwrap().contains(&v("1.0.0")));
        for bad in ["", "1.0.0", ">= 1.0.0", ">=1.0.0,", "~1.0.0", "=>1.0.0"] {
            assert_eq!(Range::parse(bad), None, "{bad}");
        }
    }
}
