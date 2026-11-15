//! Counting boots of a new system version in the `MikrosBoot` variable.

use alloc::string::String;
use alloc::vec::Vec;

/// The vendor GUID of Mikros's UEFI variables, as UEFI stores it.
pub const VENDOR: [u8; 16] = [
    0xbd, 0xe1, 0x63, 0x06, 0xce, 0x86, 0x8b, 0x4e, 0xa0, 0x25, 0x50, 0x0d, 0x76, 0xc6, 0x0a, 0x1a,
];
/// The name of the variable.
pub const NAME: &str = "MikrosBoot";
/// `EFI_VARIABLE_NON_VOLATILE`, `EFI_VARIABLE_BOOTSERVICE_ACCESS` and
/// `EFI_VARIABLE_RUNTIME_ACCESS`.
pub const ATTRIBUTES: u32 = 7;
/// The size of the variable.
pub const SIZE: usize = 80;
/// The longest version name the variable holds.
pub const NAME_MAX: usize = 64;
/// The boots that a new version gets before the bootloader falls back.
pub const TRIES: u32 = 2;

const VERSION: u32 = 1;
const TRYING: u32 = 0;
const GOOD: u32 = 1;

/// What the variable holds.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Attempts {
    pub good: bool,
    pub tries_left: u32,
    pub version: Vec<u8>,
}

impl Attempts {
    /// The variable read with `attributes` and `data`, or `None` when it
    /// does not have the form of a valid one.
    pub fn decode(attributes: u32, data: &[u8]) -> Option<Attempts> {
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
        Some(Attempts {
            good,
            tries_left: word(8),
            version: data[16..16 + len].to_vec(),
        })
    }

    /// The variable's contents. `version` must be 1 to 64 bytes long.
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

/// Which version to boot when `current` names `current`, and what to
/// write to the variable first.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Choice {
    pub version: String,
    pub write: Option<Attempts>,
}

/// The choice for `current` with the variable as read: `Ok(None)` when it
/// does not exist or is not valid, `Err` when reading it failed otherwise.
/// `previous` is the version that `@system/previous` names, if any.
pub fn choose(current: &str, previous: Option<&str>, read: Result<Option<Attempts>, ()>) -> Choice {
    let boot = |version: &str, write| Choice {
        version: String::from(version),
        write,
    };
    if current.len() > NAME_MAX {
        return boot(current, None);
    }
    let Ok(read) = read else {
        return boot(current, None);
    };
    match read {
        Some(a) if a.version == current.as_bytes() && a.good => boot(current, None),
        Some(a) if a.version == current.as_bytes() && a.tries_left > 0 => {
            let write = Attempts {
                tries_left: a.tries_left - 1,
                ..a
            };
            boot(current, Some(write))
        }
        Some(a) if a.version == current.as_bytes() => match previous {
            Some(previous) if previous != current => boot(previous, None),
            _ => boot(current, None),
        },
        _ => {
            let write = Attempts {
                good: false,
                tries_left: TRIES,
                version: current.as_bytes().to_vec(),
            };
            boot(current, Some(write))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cmdline::guid_text;

    fn attempts(good: bool, tries_left: u32, version: &str) -> Attempts {
        Attempts {
            good,
            tries_left,
            version: version.as_bytes().to_vec(),
        }
    }

    #[test]
    fn knows_the_vendor() {
        assert_eq!(guid_text(&VENDOR), "0663e1bd-86ce-4e8b-a025-500d76c60a1a");
    }

    #[test]
    fn lays_out_the_variable() {
        let data = attempts(true, 3, "1.2-debug").encode();
        assert_eq!(data[..16], [1, 0, 0, 0, 1, 0, 0, 0, 3, 0, 0, 0, 9, 0, 0, 0]);
        assert_eq!(&data[16..25], b"1.2-debug");
        assert!(data[25..].iter().all(|&b| b == 0));
        assert_eq!(
            Attempts::decode(7, &data),
            Some(attempts(true, 3, "1.2-debug"))
        );
        let long = "x".repeat(64);
        let data = attempts(false, 0, &long).encode();
        assert_eq!(Attempts::decode(7, &data), Some(attempts(false, 0, &long)));
    }

    #[test]
    fn treats_other_forms_as_missing() {
        let data = attempts(false, 2, "v").encode();
        assert_eq!(Attempts::decode(6, &data), None, "attributes");
        assert_eq!(Attempts::decode(7, &data[..79]), None, "size");
        let mut longer = data.to_vec();
        longer.push(0);
        assert_eq!(Attempts::decode(7, &longer), None, "size");
        let patched = |at: usize, value: u32| {
            let mut d = data;
            d[at..at + 4].copy_from_slice(&value.to_le_bytes());
            Attempts::decode(7, &d)
        };
        assert_eq!(patched(0, 2), None, "version");
        assert_eq!(patched(4, 2), None, "state");
        assert_eq!(patched(12, 0), None, "no name");
        assert_eq!(patched(12, 65), None, "long name");
        assert_eq!(patched(12, 64).map(|a| a.version.len()), Some(64));
    }

    #[test]
    fn counts_the_tries_of_a_new_version() {
        let first = choose("2", Some("1"), Ok(None));
        assert_eq!(first.version, "2");
        assert_eq!(first.write, Some(attempts(false, 2, "2")));
        let second = choose("2", Some("1"), Ok(first.write));
        assert_eq!(second.write, Some(attempts(false, 1, "2")));
        let third = choose("2", Some("1"), Ok(second.write));
        assert_eq!(third.version, "2");
        assert_eq!(third.write, Some(attempts(false, 0, "2")));
        let fallen = choose("2", Some("1"), Ok(third.write));
        assert_eq!(
            fallen,
            Choice {
                version: "1".into(),
                write: None
            }
        );
    }

    #[test]
    fn boots_a_good_version_without_counting() {
        let good = choose("2", Some("1"), Ok(Some(attempts(true, 0, "2"))));
        assert_eq!(
            good,
            Choice {
                version: "2".into(),
                write: None
            }
        );
    }

    #[test]
    fn starts_over_for_another_version() {
        let other = choose("3", Some("2"), Ok(Some(attempts(true, 0, "2"))));
        assert_eq!(other.write, Some(attempts(false, 2, "3")));
        let failed = choose("3", Some("2"), Ok(Some(attempts(false, 0, "2"))));
        assert_eq!(failed.version, "3");
        assert_eq!(failed.write, Some(attempts(false, 2, "3")));
    }

    #[test]
    fn keeps_a_failed_version_without_a_previous_one() {
        let failed = Some(attempts(false, 0, "2"));
        for previous in [None, Some("2")] {
            let choice = choose("2", previous, Ok(failed.clone()));
            assert_eq!(
                choice,
                Choice {
                    version: "2".into(),
                    write: None
                }
            );
        }
    }

    #[test]
    fn does_not_count_when_it_cannot() {
        let unread = choose("2", Some("1"), Err(()));
        assert_eq!(
            unread,
            Choice {
                version: "2".into(),
                write: None
            }
        );
        let long = "x".repeat(65);
        let choice = choose(&long, Some("1"), Ok(None));
        assert_eq!(
            choice,
            Choice {
                version: long,
                write: None
            }
        );
    }
}
