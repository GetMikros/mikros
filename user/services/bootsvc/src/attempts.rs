//! The `MikrosBoot` firmware variable, in which the bootloader counts
//! the boots of a new system version: the booted version's state, and
//! the variable that marking it good writes.

/// The variable's name.
pub const NAME: &str = "MikrosBoot";
/// The attributes the variable has and is written with: non-volatile,
/// with boot service and runtime access.
pub const ATTRIBUTES: u32 = 7;
/// The variable's size.
pub const SIZE: usize = 80;
/// The longest version name the variable holds.
const NAME_MAX: usize = 64;

const VERSION: u32 = 1;
const TRYING: u32 = 0;
const GOOD: u32 = 1;
/// How many boots of a version the bootloader allows while it is tried.
const TRIES: u32 = 3;

/// What the variable holds.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Record {
    pub version: Vec<u8>,
    pub good: bool,
    pub tries_left: u32,
}

impl Record {
    /// The variable with `attributes` and `data`, or `None` when it does
    /// not have the form of a valid one, which counts as missing.
    pub fn decode(attributes: u32, data: &[u8]) -> Option<Record> {
        if attributes != ATTRIBUTES || data.len() != SIZE {
            return None;
        }
        let word = |at: usize| u32::from_le_bytes(data[at..at + 4].try_into().expect("4 bytes"));
        let good = match word(4) {
            TRYING => false,
            GOOD => true,
            _ => return None,
        };
        let len = word(12) as usize;
        if word(0) != VERSION || len == 0 || len > NAME_MAX {
            return None;
        }
        Some(Record {
            version: data[16..16 + len].to_vec(),
            good,
            tries_left: word(8),
        })
    }

    /// The variable's contents; `version` must be 1 to 64 bytes long.
    pub fn encode(&self) -> [u8; SIZE] {
        let mut data = [0; SIZE];
        let state = if self.good { GOOD } else { TRYING };
        data[0..4].copy_from_slice(&VERSION.to_le_bytes());
        data[4..8].copy_from_slice(&state.to_le_bytes());
        data[8..12].copy_from_slice(&self.tries_left.to_le_bytes());
        data[12..16].copy_from_slice(&(self.version.len() as u32).to_le_bytes());
        data[16..16 + self.version.len()].copy_from_slice(&self.version);
        data
    }
}

/// The booted version: the value of the last `system=` option of
/// `cmdline`.
pub fn booted(cmdline: &str) -> Option<&str> {
    cmdline
        .split_ascii_whitespace()
        .filter_map(|option| option.strip_prefix("system="))
        .next_back()
}

/// What `GetState` says about the version `booted`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct State {
    pub version: String,
    pub good: bool,
    pub tries_left: u32,
    pub fell_back: bool,
}

/// The state of the version `booted`, with `record` as the variable holds
/// it: good when the variable names it as good; its tries left when the
/// variable names it; and fallen back when the variable names another
/// version that is being tried and has no tries left.
pub fn state(booted: &str, record: Option<&Record>) -> State {
    let ours = record.filter(|r| r.version == booted.as_bytes());
    State {
        version: booted.into(),
        good: ours.is_some_and(|r| r.good),
        tries_left: ours.map_or(0, |r| r.tries_left),
        fell_back: record
            .is_some_and(|r| r.version != booted.as_bytes() && !r.good && r.tries_left == 0),
    }
}

/// The variable that marking the version `booted` good writes, when the
/// variable `record` is missing or names that version; `None`, leaving
/// it as it is, when it names another, so that a version that the
/// bootloader fell back from stays failed, and for a name the variable
/// cannot hold.
pub fn mark_good(booted: &str, record: Option<&Record>) -> Option<Record> {
    if booted.is_empty() || booted.len() > NAME_MAX {
        return None;
    }
    if record.is_some_and(|r| r.version != booted.as_bytes()) {
        return None;
    }
    Some(Record {
        version: booted.as_bytes().to_vec(),
        good: true,
        tries_left: 0,
    })
}

/// The variable that starting to try the version `booted` again writes:
/// that version, being tried, with three boots left; `None` for a name
/// the variable cannot hold.
pub fn start_trying(booted: &str) -> Option<Record> {
    if booted.is_empty() || booted.len() > NAME_MAX {
        return None;
    }
    Some(Record {
        version: booted.as_bytes().to_vec(),
        good: false,
        tries_left: TRIES,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(version: &str, good: bool, tries_left: u32) -> Record {
        Record {
            version: version.as_bytes().to_vec(),
            good,
            tries_left,
        }
    }

    #[test]
    fn reads_and_writes_the_variable() {
        let data = record("1.2", false, 2).encode();
        assert_eq!(data[..16], [1, 0, 0, 0, 0, 0, 0, 0, 2, 0, 0, 0, 3, 0, 0, 0]);
        assert_eq!(&data[16..19], b"1.2");
        assert!(data[19..].iter().all(|&b| b == 0));
        assert_eq!(Record::decode(7, &data), Some(record("1.2", false, 2)));
    }

    #[test]
    fn treats_other_forms_as_missing() {
        let data = record("1.2", true, 0).encode();
        assert_eq!(Record::decode(6, &data), None);
        assert_eq!(Record::decode(7, &data[..79]), None);
        let patched = |at: usize, value: u32| {
            let mut d = data;
            d[at..at + 4].copy_from_slice(&value.to_le_bytes());
            Record::decode(7, &d)
        };
        assert_eq!(patched(0, 2), None, "version");
        assert_eq!(patched(4, 3), None, "state");
        assert_eq!(patched(12, 0), None, "no name");
        assert_eq!(patched(12, 65), None, "long name");
    }

    #[test]
    fn finds_the_booted_version() {
        assert_eq!(booted("root=x system=1.2 quiet"), Some("1.2"));
        assert_eq!(booted("system=1 system=2"), Some("2"));
        assert_eq!(booted("quiet subsystem=1"), None);
    }

    #[test]
    fn reports_the_state_of_the_booted_version() {
        let trying = state("2", Some(&record("2", false, 1)));
        assert_eq!(
            trying,
            State {
                version: "2".into(),
                good: false,
                tries_left: 1,
                fell_back: false
            }
        );
        let good = state("2", Some(&record("2", true, 0)));
        assert!(good.good && !good.fell_back);
        let fallen = state("1", Some(&record("2", false, 0)));
        assert_eq!(
            (fallen.good, fallen.tries_left, fallen.fell_back),
            (false, 0, true)
        );
        let other = state("1", Some(&record("2", false, 1)));
        assert!(!other.fell_back && other.tries_left == 0);
        let missing = state("1", None);
        assert!(!missing.good && !missing.fell_back && missing.tries_left == 0);
    }

    #[test]
    fn marks_only_the_counted_version_good() {
        let good = Some(record("2", true, 0));
        assert_eq!(mark_good("2", None), good);
        assert_eq!(mark_good("2", Some(&record("2", false, 1))), good);
        assert_eq!(mark_good("2", Some(&record("2", true, 0))), good);
        assert_eq!(mark_good("1", Some(&record("2", false, 0))), None);
        assert_eq!(mark_good(&"x".repeat(65), None), None);
    }

    #[test]
    fn starts_trying_the_booted_version_again() {
        assert_eq!(start_trying("2"), Some(record("2", false, 3)));
        assert_eq!(start_trying(""), None);
        assert_eq!(start_trying(&"x".repeat(65)), None);
    }
}
