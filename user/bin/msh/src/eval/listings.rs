//! The folder listings that completion and suggestions read, kept
//! between keys and read again only once a folder's modification time
//! changes. A folder whose time the system does not tell, or that
//! changed within the last moments, when another change could come
//! with the same time, is read each time. The names of folders are kept
//! for one line only, as some file systems, FAT's among them, do not
//! change a folder's time when its names change; the programs of the
//! folders of `PATH` are kept across lines, and on such a file system
//! show a new program only once the folder's time changes.

use std::cell::RefCell;
use std::collections::HashMap;
use std::time::{Duration, SystemTime};

/// The most folders whose listings are kept; past them, all are dropped,
/// and read again when asked for.
const KEPT_MAX: usize = 64;
/// How long after its last change a folder's listing is not kept, as a
/// change within the same tick of its time would not show.
const SETTLED: Duration = Duration::from_secs(2);

/// The kept listings, by the absolute path of their folder.
#[derive(Default)]
pub struct Listings {
    kept: RefCell<HashMap<String, Listing>>,
}

/// A folder's names, and the programs among them once asked for, as
/// they were when it was modified at `modified`.
struct Listing {
    modified: SystemTime,
    entries: Option<Option<Vec<(String, bool)>>>,
    programs: Option<Vec<String>>,
}

impl Listings {
    /// The names in `folder`, an absolute path, each with whether it is a
    /// folder: the kept ones when the folder, last modified at
    /// `modified` as of `now`, has not changed since, or else `read`'s.
    pub fn entries(
        &self,
        folder: &str,
        modified: Option<SystemTime>,
        now: SystemTime,
        read: impl FnOnce() -> Option<Vec<(String, bool)>>,
    ) -> Option<Vec<(String, bool)>> {
        let Some(modified) = settled(modified, now) else {
            return read();
        };
        if let Some(entries) = self.kept(folder, modified, |l| l.entries.clone()) {
            return entries;
        }
        let entries = read();
        self.keep(folder, modified, |l| l.entries = Some(entries.clone()));
        entries
    }

    /// The programs in `folder`, kept or `read` as `entries` are.
    pub fn programs(
        &self,
        folder: &str,
        modified: Option<SystemTime>,
        now: SystemTime,
        read: impl FnOnce() -> Vec<String>,
    ) -> Vec<String> {
        let Some(modified) = settled(modified, now) else {
            return read();
        };
        if let Some(programs) = self.kept(folder, modified, |l| l.programs.clone()) {
            return programs;
        }
        let programs = read();
        self.keep(folder, modified, |l| l.programs = Some(programs.clone()));
        programs
    }

    /// Drops the kept names of folders, keeping their programs, so that
    /// a new line lists folders as they are now.
    pub fn forget_entries(&self) {
        for listing in self.kept.borrow_mut().values_mut() {
            listing.entries = None;
        }
    }

    /// What `take` finds in the listing of `folder`, if one is kept for
    /// the time `modified`.
    fn kept<T>(
        &self,
        folder: &str,
        modified: SystemTime,
        take: impl FnOnce(&Listing) -> Option<T>,
    ) -> Option<T> {
        let kept = self.kept.borrow();
        kept.get(folder)
            .filter(|l| l.modified == modified)
            .and_then(take)
    }

    /// Keeps, with `set`, part of the listing of `folder` as of the time
    /// `modified`, dropping what was kept for another time.
    fn keep(&self, folder: &str, modified: SystemTime, set: impl FnOnce(&mut Listing)) {
        let mut kept = self.kept.borrow_mut();
        if kept.len() >= KEPT_MAX && !kept.contains_key(folder) {
            kept.clear();
        }
        let fresh = || Listing {
            modified,
            entries: None,
            programs: None,
        };
        let listing = kept.entry(folder.to_owned()).or_insert_with(fresh);
        if listing.modified != modified {
            *listing = fresh();
        }
        set(listing);
    }
}

/// `modified`, when it is known and lies `SETTLED` or more before `now`.
fn settled(modified: Option<SystemTime>, now: SystemTime) -> Option<SystemTime> {
    modified.filter(|&m| now.duration_since(m).is_ok_and(|age| age >= SETTLED))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    fn at(seconds: u64) -> SystemTime {
        SystemTime::UNIX_EPOCH + Duration::from_secs(seconds)
    }

    #[test]
    fn reads_a_folder_again_only_once_it_changed() {
        let listings = Listings::default();
        let reads = Cell::new(0);
        let read = |names: &[&str]| {
            reads.set(reads.get() + 1);
            names.iter().map(|n| n.to_string()).collect::<Vec<_>>()
        };
        let now = at(100);
        assert_eq!(
            listings.programs("/bin", Some(at(1)), now, || read(&["cat"])),
            ["cat"]
        );
        assert_eq!(
            listings.programs("/bin", Some(at(1)), now, || read(&["ls"])),
            ["cat"]
        );
        assert_eq!(reads.get(), 1);
        assert_eq!(
            listings.programs("/bin", Some(at(2)), now, || read(&["ls"])),
            ["ls"]
        );
        assert_eq!(reads.get(), 2);
        // Entries are kept beside the programs of the same time.
        let entries = || Some(vec![("lib".to_string(), true)]);
        assert_eq!(
            listings.entries("/bin", Some(at(2)), now, entries),
            entries()
        );
        let other = || Some(vec![("other".to_string(), false)]);
        assert_eq!(listings.entries("/bin", Some(at(2)), now, other), entries());
        assert_eq!(
            listings.programs("/bin", Some(at(2)), now, || read(&[])),
            ["ls"]
        );
        assert_eq!(listings.entries("/none", Some(at(2)), now, || None), None);
        assert_eq!(listings.entries("/none", Some(at(2)), now, entries), None);
    }

    #[test]
    fn a_new_line_lists_folders_again_but_keeps_programs() {
        let listings = Listings::default();
        let now = at(100);
        let entries = |name: &str| Some(vec![(name.to_string(), false)]);
        listings.programs("/bin", Some(at(1)), now, || vec!["cat".to_string()]);
        listings.entries("/bin", Some(at(1)), now, || entries("a"));
        assert_eq!(
            listings.entries("/bin", Some(at(1)), now, || entries("b")),
            entries("a")
        );
        listings.forget_entries();
        assert_eq!(
            listings.entries("/bin", Some(at(1)), now, || entries("b")),
            entries("b")
        );
        assert_eq!(
            listings.programs("/bin", Some(at(1)), now, Vec::new),
            ["cat"]
        );
    }

    #[test]
    fn reads_each_time_without_a_settled_time() {
        let listings = Listings::default();
        let reads = Cell::new(0);
        let read = || {
            reads.set(reads.get() + 1);
            vec!["cat".to_string()]
        };
        for _ in 0..2 {
            listings.programs("/bin", None, at(100), read);
            listings.programs("/bin", Some(at(99)), at(100), read);
            listings.programs("/bin", Some(at(101)), at(100), read);
        }
        assert_eq!(reads.get(), 6);
    }

    #[test]
    fn keeps_a_bounded_number_of_folders() {
        let listings = Listings::default();
        for n in 0..=KEPT_MAX {
            listings.programs(&format!("/f{n}"), Some(at(1)), at(100), Vec::new);
        }
        assert!(listings.kept.borrow().len() <= KEPT_MAX);
    }
}
