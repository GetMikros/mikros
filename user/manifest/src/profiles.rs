//! Storage profiles: the file that chooses the cache policy of chosen
//! disks and partitions.
//!
//! The file is YAML with the key `profiles`, a list of maps, each with
//! `cache`, either `write-back` or `quick-disconnect`, and exactly one of
//! `serial`, matching a disk's serial number, or `partition_guid`,
//! matching a partition's GUID. Other keys are ignored. Any other shape
//! makes the whole file invalid.

use alloc::string::String;
use alloc::vec::Vec;

/// A cache policy.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Cache {
    WriteBack,
    QuickDisconnect,
}

impl Cache {
    /// The value of the `cache` property.
    pub fn name(self) -> &'static str {
        match self {
            Cache::WriteBack => "write-back",
            Cache::QuickDisconnect => "quick-disconnect",
        }
    }

    fn from_name(name: &str) -> Option<Cache> {
        match name {
            "write-back" => Some(Cache::WriteBack),
            "quick-disconnect" => Some(Cache::QuickDisconnect),
            _ => None,
        }
    }
}

/// What a profile matches.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Match {
    Serial(String),
    PartitionGuid(String),
}

/// One entry of the file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Profile {
    pub matches: Match,
    pub cache: Cache,
}

/// The profiles of a valid file, and the keys it has that profiles do
/// not use, which are ignored, for logging.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProfilesFile {
    pub profiles: Vec<Profile>,
    pub unknown: Vec<String>,
}

/// The profiles of the file `bytes`, in order, or none when the file is
/// invalid. Unknown keys, at the top or in an entry, are ignored and
/// listed in `unknown`.
pub fn parse_profiles(bytes: &[u8]) -> Option<ProfilesFile> {
    let root = mikros_yaml::parse(bytes).ok()?;
    let map = root.map().ok()?;
    let mut unknown: Vec<String> = map.unknown(&["profiles"]).map(|e| e.key.clone()).collect();
    let mut profiles = Vec::new();
    for item in map.get("profiles")?.list().ok()? {
        let entry = item.map().ok()?;
        let known = ["cache", "serial", "partition_guid"];
        unknown.extend(entry.unknown(&known).map(|e| e.key.clone()));
        let cache = Cache::from_name(entry.get("cache")?.string().ok()?)?;
        let serial = entry.get("serial").map(|n| n.string());
        let guid = entry.get("partition_guid").map(|n| n.string());
        let matches = match (serial, guid) {
            (Some(serial), None) => Match::Serial(serial.ok()?.into()),
            (None, Some(guid)) => Match::PartitionGuid(guid.ok()?.into()),
            _ => return None,
        };
        profiles.push(Profile { matches, cache });
    }
    Some(ProfilesFile { profiles, unknown })
}

/// The cache policy of the first of `profiles` that matches a disk with
/// the serial number `serial` or a partition with the GUID
/// `partition_guid`, which matches without regard to case.
pub fn choose_cache(
    profiles: &[Profile],
    serial: Option<&str>,
    partition_guid: Option<&str>,
) -> Option<Cache> {
    profiles
        .iter()
        .find(|p| match &p.matches {
            Match::Serial(s) => serial == Some(s.as_str()),
            Match::PartitionGuid(g) => {
                partition_guid.is_some_and(|ours| ours.eq_ignore_ascii_case(g))
            }
        })
        .map(|p| p.cache)
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;

    const FILE: &str = "\
profiles:
  - serial: S1
    cache: quick-disconnect
  - partition_guid: 4E564D65-0000-4000-8000-000000000001
    cache: write-back
  - serial: S1
    cache: write-back
";

    #[test]
    fn reads_the_profiles_in_order() {
        let file = parse_profiles(FILE.as_bytes()).unwrap();
        assert!(file.unknown.is_empty());
        assert_eq!(
            file.profiles,
            vec![
                Profile {
                    matches: Match::Serial("S1".into()),
                    cache: Cache::QuickDisconnect,
                },
                Profile {
                    matches: Match::PartitionGuid("4E564D65-0000-4000-8000-000000000001".into()),
                    cache: Cache::WriteBack,
                },
                Profile {
                    matches: Match::Serial("S1".into()),
                    cache: Cache::WriteBack,
                },
            ]
        );
    }

    #[test]
    fn the_first_match_wins() {
        let profiles = parse_profiles(FILE.as_bytes()).unwrap().profiles;
        assert_eq!(
            choose_cache(&profiles, Some("S1"), None),
            Some(Cache::QuickDisconnect)
        );
        let guid = "4e564d65-0000-4000-8000-000000000001";
        assert_eq!(
            choose_cache(&profiles, None, Some(guid)),
            Some(Cache::WriteBack)
        );
        assert_eq!(choose_cache(&profiles, Some("S2"), None), None);
        assert_eq!(choose_cache(&profiles, None, None), None);
    }

    #[test]
    fn an_empty_list_is_valid() {
        let file = parse_profiles(b"profiles: []\n").unwrap();
        assert!(file.profiles.is_empty() && file.unknown.is_empty());
    }

    #[test]
    fn unknown_keys_are_ignored_and_listed() {
        let text = "\
version: 2
profiles:
  - serial: S1
    cache: write-back
    note: the build disk
";
        let file = parse_profiles(text.as_bytes()).unwrap();
        assert_eq!(
            file.profiles,
            vec![Profile {
                matches: Match::Serial("S1".into()),
                cache: Cache::WriteBack,
            }]
        );
        assert_eq!(file.unknown, ["version", "note"]);
    }

    #[test]
    fn other_shapes_make_the_file_invalid() {
        let invalid = [
            "",
            "profiles: {}\n",
            "other: []\n",
            "profiles:\n  - serial: S1\n",
            "profiles:\n  - cache: write-back\n",
            "profiles:\n  - serial: S1\n    cache: fast\n",
            "profiles:\n  - serial: S1\n    partition_guid: g\n    cache: write-back\n",
            "profiles:\n  - serial: [S1]\n    cache: write-back\n",
            "profiles: [\n",
        ];
        for text in invalid {
            assert_eq!(parse_profiles(text.as_bytes()), None, "{text:?}");
        }
    }
}
