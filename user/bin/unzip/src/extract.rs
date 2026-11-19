//! Extracting the entries of a zip archive (MIKSPEC-0098.R66) into a
//! folder and only into it (MIKSPEC-0098.R60), within the space of its
//! volume (MIKSPEC-0098.R67): an archive whose entries overlap is
//! refused, and so is one whose entries add up to more than the volume
//! can take, before anything is written. Each entry is checked by
//! `inside` before anything is written for it. Folders get their mode
//! and time at the end.

use crate::args::Existing;
use crate::central::{Directory, Record, S_IFMT, S_IFREG};
use crate::data::{self, Crc};
use crate::inside::{self, Space, Target};
use crate::listing::entry_type;
use mikros_cli::EntryType;
use std::fs;
use std::io::{Read, Seek};
use std::path::Path;

/// The longest target of a link that unzip takes.
const TARGET_MAX: u64 = 4096;

/// What a run tells as it goes.
pub trait Report {
    /// What was done with an entry, as `inflating`, and its name.
    fn did(&mut self, what: &str, name: &str, kind: EntryType);
    /// A failure, after which the run exits with 1.
    fn error(&mut self, message: &str);
    /// A message that is not a failure.
    fn note(&mut self, message: &str);
    /// Whether to replace the existing file `name`; none when there is
    /// no one to ask.
    fn replace(&mut self, name: &str) -> Option<bool>;
}

/// How a run extracts.
#[derive(Clone, Copy, Debug)]
pub struct Options {
    /// `-j`.
    pub junk: bool,
    pub existing: Existing,
}

/// Why an entry did not go as asked.
enum Failed {
    Entry(String),
    Stop(String),
}

fn refused(why: String) -> Failed {
    Failed::Entry(format!("refused: {why}"))
}

/// Extracts the records of `directory` at `chosen` from `file` into
/// `root`, which is made when it is not there; whether every one went
/// as asked.
pub fn extract<F: Read + Seek>(
    file: &mut F,
    directory: &Directory,
    chosen: &[usize],
    root: &Path,
    space: &mut dyn Space,
    options: Options,
    report: &mut dyn Report,
) -> bool {
    if let Err(why) = data::overlapping(file, directory) {
        report.error(&format!("refused: {why}"));
        return false;
    }
    let total = chosen
        .iter()
        .map(|&i| &directory.records[i])
        .filter(|r| !r.is_folder())
        .fold(0u64, |sum, r| sum.saturating_add(r.size));
    let mut there = root;
    while !there.is_dir() {
        match there.parent() {
            Some(parent) if !parent.as_os_str().is_empty() => there = parent,
            _ => {
                there = Path::new(".");
                break;
            }
        }
    }
    let fits = space
        .volume(there)
        .and_then(|volume| inside::fits("the entries", total, &volume));
    if let Err(why) = fits {
        report.error(&why);
        return false;
    }
    if let Err(e) = fs::create_dir_all(root) {
        report.error(&format!("cannot create {}: {e}", root.display()));
        return false;
    }
    let target = match Target::new(root) {
        Ok(target) => target,
        Err(e) => {
            report.error(&format!("cannot extract into {}: {e}", root.display()));
            return false;
        }
    };
    let mut ok = true;
    let mut folders: Vec<(Vec<String>, Option<u32>, i64)> = Vec::new();
    for &i in chosen {
        let record = &directory.records[i];
        match one(file, record, &target, space, options, &mut folders, report) {
            Ok(()) => {}
            Err(Failed::Entry(why)) => {
                report.error(&format!("{}: {why}", record.name));
                ok = false;
            }
            Err(Failed::Stop(why)) => {
                report.error(&why);
                ok = false;
                break;
            }
        }
    }
    for (parts, mode, modified) in folders.iter().rev() {
        let set = match mode {
            Some(mode) => target.mode(parts, *mode),
            None => Ok(()),
        };
        if let Err(e) = set.and_then(|()| target.modified(parts, *modified)) {
            report.error(&format!(
                "{}: cannot set its mode and time: {e}",
                inside::shown(parts)
            ));
            ok = false;
        }
    }
    ok
}

/// Extracts one entry.
fn one<F: Read + Seek>(
    file: &mut F,
    record: &Record,
    target: &Target,
    space: &mut dyn Space,
    options: Options,
    folders: &mut Vec<(Vec<String>, Option<u32>, i64)>,
    report: &mut dyn Report,
) -> Result<(), Failed> {
    let failed = Failed::Entry;
    let mut parts = inside::parts(&record.name).map_err(refused)?;
    let folder = record.is_folder();
    let link = record.is_link();
    if let Some(mode) = record.mode()
        && !folder
        && !link
        && mode & S_IFMT != S_IFREG
    {
        report.note(&format!("{}: skipped, a special entry", record.name));
        return Ok(());
    }
    if options.junk {
        if folder {
            return Ok(());
        }
        parts = parts.split_off(parts.len().saturating_sub(1));
    }
    if parts.is_empty() {
        return match folder {
            true => Ok(()),
            false => Err(refused("it names the folder extracted into".into())),
        };
    }
    let at = target.resolve(&parts, folder).map_err(refused)?;
    let size = if folder { 0 } else { record.size };
    target
        .room(space, &at, &record.name, size)
        .map_err(Failed::Stop)?;
    let modified = record.modified();
    if folder {
        target.folder(&at).map_err(failed)?;
        report.did("creating", &record.name, EntryType::Folder);
        folders.push((at, record.mode(), modified));
        return Ok(());
    }
    match target.what(&at) {
        Ok(None) => {}
        Ok(Some(t)) if t.is_dir() => return Err(failed("a folder is in its place".into())),
        Ok(Some(_)) => {
            let replace = match options.existing {
                Existing::Overwrite => true,
                Existing::Never => false,
                Existing::Ask => match report.replace(&inside::shown(&at)) {
                    Some(answer) => answer,
                    None => {
                        report.note(&format!(
                            "{}: skipped, a file is there already",
                            record.name
                        ));
                        false
                    }
                },
            };
            if !replace {
                return Ok(());
            }
            target.clear(&at).map_err(failed)?;
        }
        Err(e) => return Err(failed(format!("cannot look at it: {e}"))),
    }
    target
        .parents(&at)
        .map_err(|e| failed(format!("cannot create its folder: {e}")))?;
    let mut data = Crc::new(data::open(file, record).map_err(failed)?);
    if link {
        if record.size > TARGET_MAX {
            return Err(refused(format!(
                "its link target of {} bytes is longer than {TARGET_MAX}",
                record.size
            )));
        }
        let mut bytes = Vec::new();
        inside::copy(&mut data, &mut bytes, record.size).map_err(failed)?;
        data::checked(record, data.crc()).map_err(failed)?;
        let to =
            String::from_utf8(bytes).map_err(|_| failed("its link target is not UTF-8".into()))?;
        target
            .link(&at, &to)
            .map_err(|e| failed(format!("cannot create the link: {e}")))?;
        let link = format!("{} -> {to}", record.name);
        report.did("linking", &link, EntryType::Link);
    } else {
        let mut out = target
            .create(&at)
            .map_err(|e| failed(format!("cannot create it: {e}")))?;
        let copied = inside::copy(&mut data, &mut out, record.size)
            .and_then(|()| data::checked(record, data.crc()));
        drop(out);
        if let Err(why) = copied {
            target.remove(&at);
            return Err(failed(why));
        }
        if let Some(mode) = record.mode() {
            target
                .mode(&at, mode)
                .map_err(|e| failed(format!("cannot set its mode: {e}")))?;
        }
        let what = match record.method {
            0 => "extracting",
            _ => "inflating",
        };
        report.did(what, &record.name, entry_type(record));
    }
    target
        .modified(&at, modified)
        .map_err(|e| failed(format!("cannot set its modified time: {e}")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::central::{self, S_IFDIR, S_IFLNK};
    use crate::data::tests::{Crafted, archive, crafted};
    use mikros_extract::testing::{Fixed, Scratch};
    use std::io;
    use std::os::unix::fs::{MetadataExt, symlink};

    #[derive(Default)]
    struct Told {
        did: Vec<String>,
        errors: Vec<String>,
        notes: Vec<String>,
        answer: Option<bool>,
    }

    impl Report for Told {
        fn did(&mut self, what: &str, name: &str, _: EntryType) {
            self.did.push(format!("{what} {name}"));
        }
        fn error(&mut self, message: &str) {
            self.errors.push(message.into());
        }
        fn note(&mut self, message: &str) {
            self.notes.push(message.into());
        }
        fn replace(&mut self, _: &str) -> Option<bool> {
            self.answer
        }
    }

    fn run(entries: &[Crafted], root: &Path, existing: Existing, space: Fixed) -> (bool, Told) {
        let bytes = archive(entries);
        run_bytes(&bytes, root, existing, space, false)
    }

    fn run_bytes(
        bytes: &[u8],
        root: &Path,
        existing: Existing,
        space: Fixed,
        junk: bool,
    ) -> (bool, Told) {
        let mut file = io::Cursor::new(bytes);
        let directory = central::read(&mut file).unwrap();
        let chosen: Vec<usize> = (0..directory.records.len()).collect();
        let mut told = Told::default();
        let mut space = space;
        let options = Options { junk, existing };
        let ok = extract(
            &mut file, &directory, &chosen, root, &mut space, options, &mut told,
        );
        (ok, told)
    }

    fn plenty() -> Fixed {
        Fixed(1 << 40, 1 << 39)
    }

    fn folder(name: &str) -> Crafted {
        let mut e = crafted(name, b"");
        e.deflate = false;
        e.mode = Some(S_IFDIR | 0o755);
        e
    }

    fn link(name: &str, to: &str) -> Crafted {
        let mut e = crafted(name, to.as_bytes());
        e.mode = Some(S_IFLNK | 0o777);
        e
    }

    #[test]
    fn extracts_files_folders_and_links() {
        let s = Scratch::new("extract");
        let root = s.join("made/here");
        let mut f = crafted("d/f", b"contents");
        f.mode = Some(0o100750);
        let mut plain = crafted("d/plain", b"no mode");
        plain.mode = None;
        let entries = [folder("d/"), f, plain, link("d/l", "/far/away")];
        let (ok, told) = run(&entries, &root, Existing::Ask, plenty());
        assert!(ok, "{:?}", told.errors);
        assert_eq!(fs::read(root.join("d/f")).unwrap(), b"contents");
        assert_eq!(
            fs::metadata(root.join("d/f")).unwrap().mode() & 0o7777,
            0o750
        );
        assert_eq!(fs::read(root.join("d/plain")).unwrap(), b"no mode");
        assert_eq!(
            fs::read_link(root.join("d/l")).unwrap(),
            Path::new("/far/away")
        );
        let modified = fs::metadata(root.join("d/f")).unwrap().mtime();
        assert_eq!(modified, central::from_dos(0, 0x5b21));
        assert_eq!(told.did[1], "inflating d/f");
    }

    #[test]
    fn refuses_names_and_links_that_lead_out() {
        let s = Scratch::new("traversal");
        let root = s.join("root");
        fs::create_dir_all(s.join("outside")).unwrap();
        let outside = s.join("outside").display().to_string();
        let entries = [
            crafted("../escape", b"x"),
            crafted("a/../../escape", b"x"),
            crafted(&format!("{outside}/abs"), b"x"),
            link("out", "../outside"),
            crafted("out/through", b"x"),
            link("abs", &outside),
            folder("abs/sub/"),
            crafted("fine", b"y"),
        ];
        let (ok, told) = run(&entries, &root, Existing::Ask, plenty());
        assert!(!ok);
        assert_eq!(told.errors.len(), 5, "{:?}", told.errors);
        assert!(told.errors[0].starts_with("../escape: refused: "));
        assert!(told.errors[3].contains("leads out"), "{:?}", told.errors);
        assert!(fs::read_dir(s.join("outside")).unwrap().next().is_none());
        assert_eq!(fs::read(root.join("fine")).unwrap(), b"y");
    }

    #[test]
    fn never_writes_through_a_link_that_is_there() {
        let s = Scratch::new("through");
        let root = s.join("root");
        fs::create_dir_all(&root).unwrap();
        fs::write(s.join("victim"), b"keep").unwrap();
        symlink(s.join("victim"), root.join("f")).unwrap();
        let (ok, told) = run(
            &[crafted("f", b"new")],
            &root,
            Existing::Overwrite,
            plenty(),
        );
        assert!(ok, "{:?}", told.errors);
        assert_eq!(fs::read(s.join("victim")).unwrap(), b"keep");
        assert_eq!(fs::read(root.join("f")).unwrap(), b"new");
    }

    #[test]
    fn overwrites_only_as_asked() {
        let s = Scratch::new("existing");
        fs::write(s.join("f"), b"old").unwrap();
        let entries = [crafted("f", b"new")];
        let (ok, told) = run(&entries, &s.0, Existing::Never, plenty());
        assert!(ok && told.notes.is_empty());
        assert_eq!(fs::read(s.join("f")).unwrap(), b"old");
        let (ok, told) = run(&entries, &s.0, Existing::Ask, plenty());
        assert!(ok && told.notes[0].contains("skipped"));
        assert_eq!(fs::read(s.join("f")).unwrap(), b"old");
        let mut told = Told {
            answer: Some(true),
            ..Told::default()
        };
        let bytes = archive(&entries);
        let mut file = io::Cursor::new(&bytes);
        let directory = central::read(&mut file).unwrap();
        let options = Options {
            junk: false,
            existing: Existing::Ask,
        };
        assert!(extract(
            &mut file,
            &directory,
            &[0],
            &s.0,
            &mut plenty(),
            options,
            &mut told
        ));
        assert_eq!(fs::read(s.join("f")).unwrap(), b"new");
    }

    #[test]
    fn fails_and_removes_entries_whose_size_or_crc_lie() {
        let s = Scratch::new("lies");
        let mut longer = crafted("longer", &[b'a'; 10_000]);
        longer.size = Some(100);
        let mut shorter = crafted("shorter", b"short");
        shorter.size = Some(1000);
        let mut bad = crafted("bad-crc", b"data");
        bad.crc = Some(1);
        let entries = [longer, shorter, bad, crafted("good", b"g")];
        let (ok, told) = run(&entries, &s.0, Existing::Ask, plenty());
        assert!(!ok);
        assert!(
            told.errors[0].starts_with("longer: its data is longer"),
            "{:?}",
            told.errors
        );
        assert!(told.errors[1].starts_with("shorter: its data is shorter"));
        assert!(told.errors[2].starts_with("bad-crc: its CRC-32"));
        let names: Vec<String> = fs::read_dir(&s.0)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names, ["good"]);
    }

    #[test]
    fn checks_the_total_before_writing_anything() {
        let s = Scratch::new("total");
        let entries = [crafted("a", &[0; 60]), crafted("b", &[0; 60])];
        let (ok, told) = run(&entries, &s.join("new"), Existing::Ask, Fixed(1000, 150));
        assert!(!ok);
        assert!(
            told.errors[0].starts_with("the entries: its 120 bytes"),
            "{:?}",
            told.errors
        );
        assert!(!s.join("new").exists());
    }

    #[test]
    fn stops_before_an_entry_that_does_not_fit() {
        // The space that is left shrinks as other programs write.
        struct Shrinking(u64);
        impl Space for Shrinking {
            fn volume(&mut self, _: &Path) -> Result<inside::Volume, String> {
                let free = self.0;
                self.0 = self.0.saturating_sub(100);
                Ok(inside::Volume {
                    name: "v".into(),
                    total: 1000,
                    free,
                })
            }
        }
        let s = Scratch::new("each");
        let entries = [
            crafted("a", &[0; 10]),
            crafted("b", &[0; 10]),
            crafted("c", &[0; 10]),
        ];
        let bytes = archive(&entries);
        let mut file = io::Cursor::new(&bytes);
        let directory = central::read(&mut file).unwrap();
        let mut told = Told::default();
        let options = Options {
            junk: false,
            existing: Existing::Ask,
        };
        let ok = extract(
            &mut file,
            &directory,
            &[0, 1, 2],
            &s.0,
            &mut Shrinking(250),
            options,
            &mut told,
        );
        assert!(!ok);
        assert_eq!(told.errors.len(), 1);
        assert!(
            told.errors[0].starts_with("b: its 10 bytes"),
            "{:?}",
            told.errors
        );
        assert!(s.join("a").exists() && !s.join("b").exists() && !s.join("c").exists());
    }

    #[test]
    fn refuses_overlapping_archives_before_writing_anything() {
        let s = Scratch::new("overlap");
        let mut second = crafted("b", &[0; 1000]);
        second.offset = Some(0);
        let (ok, told) = run(
            &[crafted("a", &[0; 1000]), second],
            &s.join("out"),
            Existing::Ask,
            plenty(),
        );
        assert!(!ok);
        assert!(told.errors[0].contains("overlap"));
        assert!(!s.join("out").exists());
    }

    #[test]
    fn refuses_long_link_targets_and_skips_special_entries() {
        let s = Scratch::new("special");
        let mut fifo = crafted("fifo", b"");
        fifo.mode = Some(0o010644);
        let entries = [link("l", &"x".repeat(5000)), fifo];
        let (ok, told) = run(&entries, &s.0, Existing::Ask, plenty());
        assert!(!ok);
        assert!(
            told.errors[0].contains("longer than 4096"),
            "{:?}",
            told.errors
        );
        assert!(told.notes[0].starts_with("fifo: skipped"));
        assert!(fs::read_dir(&s.0).unwrap().next().is_none());
    }

    #[test]
    fn junks_folders_but_still_refuses_parent_parts() {
        let s = Scratch::new("junk");
        let bytes = archive(&[
            folder("a/"),
            crafted("a/b/c", b"c"),
            crafted("../x/y", b"y"),
        ]);
        let (ok, told) = run_bytes(&bytes, &s.0, Existing::Ask, plenty(), true);
        assert!(!ok && told.errors.len() == 1);
        assert_eq!(fs::read(s.join("c")).unwrap(), b"c");
        assert!(!s.join("y").exists() && !s.join("a").exists());
    }

    #[test]
    fn leaves_archives_inside_archives_as_they_are() {
        let s = Scratch::new("nested");
        let inner = archive(&[crafted("inner", b"i")]);
        let (ok, _) = run(
            &[crafted("inner.zip", &inner)],
            &s.0,
            Existing::Ask,
            plenty(),
        );
        assert!(ok);
        assert_eq!(fs::read(s.join("inner.zip")).unwrap(), inner);
        assert!(!s.join("inner").exists());
    }
}
