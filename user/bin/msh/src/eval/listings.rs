//! The folder listings that completion and suggestions read, kept
//! between keys and read again only once a folder's modification time
//! changes. A folder whose time the system does not tell, or that
//! changed within the last moments, when another change could come
//! with the same time, is read each time. The names of folders are kept
//! for one line only, as some file systems, FAT's among them, do not
//! change a folder's time when its names change; the programs of the
//! folders of `PATH` are kept across lines, and on such a file system
//! show a new program only once the folder's time changes.
//!
//! Tab reads what it needs on the spot. A suggestion does not wait: it
//! takes what is kept, and once a reader thread is started, has the
//! thread read what is missing or changed, which calls back when it
//! kept something new, so that the suggestion can be worked out again.

use std::collections::{HashMap, HashSet};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::thread;
use std::time::{Duration, SystemTime};

/// The most folders whose listings are kept; past them, all are dropped,
/// and read again when asked for.
const KEPT_MAX: usize = 64;
/// How long after its last change a folder's listing is not kept, as a
/// change within the same tick of its time would not show.
const SETTLED: Duration = Duration::from_secs(2);

/// Where the reader thread reads folders.
pub trait Disk: Send + Sync {
    /// When the folder at `path` last changed, if that is known.
    fn modified(&self, path: &str) -> Option<SystemTime>;
    /// The names in the folder at `path`, each with whether it is a
    /// folder; `None` when it cannot be read.
    fn entries(&self, path: &str) -> Option<Vec<(String, bool)>>;
    /// Whether the file at `path` may be run.
    fn runnable(&self, path: &str) -> bool;
}

/// The kept listings, by the absolute path of their folder, and the
/// reader thread, once started.
#[derive(Default)]
pub struct Listings {
    kept: Arc<Mutex<Kept>>,
    reader: Option<Sender<Ask>>,
}

/// A folder for the reader thread to read, and whether with its
/// programs.
type Ask = (String, bool);

#[derive(Default)]
struct Kept {
    listings: HashMap<String, Listing>,
    /// What the reader thread was asked for and has not read yet.
    asked: HashSet<Ask>,
}

/// A folder's names, and the programs among them once asked for, as
/// they were when it was modified at `modified`.
#[derive(Clone, PartialEq)]
struct Listing {
    modified: SystemTime,
    entries: Option<Option<Vec<(String, bool)>>>,
    programs: Option<Vec<String>>,
}

impl Listings {
    /// Starts the reader thread, which reads from `disk` and calls
    /// `changed` each time it keeps a listing other than the one kept
    /// before.
    pub fn start_reader(&mut self, disk: Arc<dyn Disk>, changed: impl Fn() + Send + 'static) {
        let (ask, asked) = mpsc::channel();
        let kept = self.kept.clone();
        let started = thread::Builder::new()
            .name("listings".into())
            .spawn(move || read_asked(&*disk, &kept, &asked, &changed));
        if started.is_ok() {
            self.reader = Some(ask);
        }
    }

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

    /// Whether a reader thread was started, so that suggestions need not
    /// read folders themselves.
    pub fn reading_in_background(&self) -> bool {
        self.reader.is_some()
    }

    /// The names in `folder`, an absolute path, without waiting: those
    /// kept, if any, after asking the reader thread to check them.
    pub fn entries_now(&self, folder: &str) -> Option<Vec<(String, bool)>> {
        self.ask(folder, false);
        let kept = self.lock();
        kept.listings
            .get(folder)
            .and_then(|l| l.entries.clone())
            .flatten()
    }

    /// The programs in `folder` without waiting, as `entries_now` gives
    /// the names.
    pub fn programs_now(&self, folder: &str) -> Vec<String> {
        self.ask(folder, true);
        let kept = self.lock();
        kept.listings
            .get(folder)
            .and_then(|l| l.programs.clone())
            .unwrap_or_default()
    }

    /// Drops the kept names of folders, keeping their programs, so that
    /// a new line lists folders as they are now.
    pub fn forget_entries(&self) {
        for listing in self.lock().listings.values_mut() {
            listing.entries = None;
        }
    }

    /// Asks the reader thread for `folder`, with its programs when
    /// `programs`, unless it was asked already.
    fn ask(&self, folder: &str, programs: bool) {
        let Some(reader) = &self.reader else {
            return;
        };
        let ask = (folder.to_owned(), programs);
        let mut kept = self.lock();
        if kept.asked.insert(ask.clone()) && reader.send(ask.clone()).is_err() {
            kept.asked.remove(&ask);
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
        let kept = self.lock();
        kept.listings
            .get(folder)
            .filter(|l| l.modified == modified)
            .and_then(take)
    }

    /// Keeps, with `set`, part of the listing of `folder` as of the time
    /// `modified`.
    fn keep(&self, folder: &str, modified: SystemTime, set: impl FnOnce(&mut Listing)) {
        keep(&mut self.lock(), folder, modified, set);
    }

    fn lock(&self) -> MutexGuard<'_, Kept> {
        lock(&self.kept)
    }
}

fn lock(kept: &Mutex<Kept>) -> MutexGuard<'_, Kept> {
    kept.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Keeps, with `set`, part of the listing of `folder` as of the time
/// `modified`, dropping what was kept for another time.
fn keep(kept: &mut Kept, folder: &str, modified: SystemTime, set: impl FnOnce(&mut Listing)) {
    if kept.listings.len() >= KEPT_MAX && !kept.listings.contains_key(folder) {
        kept.listings.clear();
    }
    let fresh = || Listing {
        modified,
        entries: None,
        programs: None,
    };
    let listing = kept.listings.entry(folder.to_owned()).or_insert_with(fresh);
    if listing.modified != modified {
        *listing = fresh();
    }
    set(listing);
}

/// `modified`, when it is known and lies `SETTLED` or more before `now`.
fn settled(modified: Option<SystemTime>, now: SystemTime) -> Option<SystemTime> {
    modified.filter(|&m| now.duration_since(m).is_ok_and(|age| age >= SETTLED))
}

/// The reader thread: checks each folder it is asked for on `disk`,
/// reads it when what is kept of it is missing or older than the
/// folder, keeps what it read when the folder's time is settled, and
/// calls `changed` when that differs from what was kept.
fn read_asked(disk: &dyn Disk, kept: &Mutex<Kept>, asked: &Receiver<Ask>, changed: &dyn Fn()) {
    for (folder, programs) in asked {
        let modified = disk.modified(&folder);
        // A folder read again for its names keeps its programs too.
        let programs = programs
            || lock(kept)
                .listings
                .get(&folder)
                .is_some_and(|l| l.programs.is_some());
        {
            let mut k = lock(kept);
            let current = settled(modified, SystemTime::now()).is_some_and(|m| {
                k.listings.get(&folder).is_some_and(|l| {
                    l.modified == m && l.entries.is_some() && (!programs || l.programs.is_some())
                })
            });
            if current {
                k.asked.remove(&(folder.clone(), false));
                k.asked.remove(&(folder.clone(), true));
                continue;
            }
        }
        let entries = disk.entries(&folder);
        let found = programs.then(|| {
            let base = folder.trim_end_matches('/');
            entries
                .iter()
                .flatten()
                .filter(|(name, is_folder)| !is_folder && disk.runnable(&format!("{base}/{name}")))
                .map(|(name, _)| name.clone())
                .collect::<Vec<_>>()
        });
        let mut k = lock(kept);
        k.asked.remove(&(folder.clone(), false));
        k.asked.remove(&(folder.clone(), true));
        let Some(modified) = settled(modified, SystemTime::now()) else {
            continue;
        };
        let before = k.listings.get(&folder).cloned();
        keep(&mut k, &folder, modified, |l| {
            l.entries = Some(entries);
            if let Some(found) = found {
                l.programs = Some(found);
            }
        });
        let differs = k.listings.get(&folder) != before.as_ref();
        drop(k);
        if differs {
            changed();
        }
    }
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
        assert!(listings.lock().listings.len() <= KEPT_MAX);
    }

    /// Folders by path, each with its modification time in seconds and
    /// its names, where a name ending in `*` may be run and one ending in
    /// `/` is a folder.
    #[derive(Default)]
    struct Fake {
        folders: Mutex<HashMap<String, (u64, Vec<&'static str>)>>,
    }

    impl Fake {
        fn set(&self, path: &str, time: u64, names: &[&'static str]) {
            let mut folders = self.folders.lock().unwrap();
            folders.insert(path.into(), (time, names.to_vec()));
        }
    }

    impl Disk for Fake {
        fn modified(&self, path: &str) -> Option<SystemTime> {
            let folders = self.folders.lock().unwrap();
            folders.get(path).map(|(time, _)| at(*time))
        }
        fn entries(&self, path: &str) -> Option<Vec<(String, bool)>> {
            let folders = self.folders.lock().unwrap();
            let (_, names) = folders.get(path)?;
            let entry = |n: &&str| (n.trim_end_matches(['*', '/']).to_owned(), n.ends_with('/'));
            Some(names.iter().map(entry).collect())
        }
        fn runnable(&self, path: &str) -> bool {
            let (folder, name) = path.rsplit_once('/').unwrap();
            let folders = self.folders.lock().unwrap();
            folders[folder].1.contains(&format!("{name}*").as_str())
        }
    }

    #[test]
    fn suggestions_take_what_is_kept_while_the_thread_reads() {
        let fake = Arc::new(Fake::default());
        let (tell, told) = mpsc::channel();
        let mut listings = Listings::default();
        assert!(!listings.reading_in_background());
        listings.start_reader(fake.clone(), move || {
            let _ = tell.send(());
        });
        assert!(listings.reading_in_background());
        fake.set("/bin", 1, &["cat*", "notes", "lib/"]);
        assert!(listings.programs_now("/bin").is_empty());
        told.recv_timeout(Duration::from_secs(5)).unwrap();
        assert_eq!(listings.programs_now("/bin"), ["cat"]);
        assert_eq!(listings.entries_now("/bin").map(|n| n.len()), Some(3));
        // A changed folder shows what was kept until the thread read it.
        fake.set("/bin", 2, &["cat*", "ls*"]);
        assert_eq!(listings.programs_now("/bin"), ["cat"]);
        told.recv_timeout(Duration::from_secs(5)).unwrap();
        assert_eq!(listings.programs_now("/bin"), ["cat", "ls"]);
        // An unchanged folder is not read again, and tells nothing.
        assert_eq!(listings.programs_now("/bin"), ["cat", "ls"]);
        assert!(told.recv_timeout(Duration::from_millis(200)).is_err());
    }
}
