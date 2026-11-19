//! Extracting a tar archive (MIKSPEC-0098.R65) into a folder and only
//! into it (MIKSPEC-0098.R60), within the space of its volume
//! (MIKSPEC-0098.R67). Each entry is checked by `inside` before anything
//! is written for it; refused entries are named and skipped. Folders get
//! their mode and time at the end, so that what is extracted into them
//! does not change their time.

use crate::args::Select;
use crate::create::Report;
use crate::header::{Entry, Kind};
use crate::inside::{self, Space, Target};
use crate::reader::{Next, Reader};
use std::io::Read;

/// How existing files are treated.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Existing {
    Replace,
    /// Kept, failing the entry, with `-k`.
    Keep,
}

/// Why an entry did not go as asked.
enum Failed {
    /// The entry is named and skipped.
    Entry(String),
    /// The run stops.
    Stop(String),
}

/// Extracts the entries of `reader` that `select` takes into `target`;
/// whether every one went as asked.
pub fn extract<R: Read>(
    reader: &mut Reader<R>,
    target: &Target,
    space: &mut dyn Space,
    select: &mut Select,
    existing: Existing,
    report: &mut dyn Report,
) -> bool {
    let mut ok = true;
    // The folders made, with their mode and time.
    let mut folders: Vec<(Vec<String>, u32, i64)> = Vec::new();
    loop {
        let entry = match reader.next() {
            Ok(Next::End) => break,
            Ok(Next::Refused(name, why)) => {
                if select.takes(&name) {
                    report.error(&format!("{name}: refused: {why}"));
                    ok = false;
                }
                continue;
            }
            Ok(Next::Entry(entry)) => entry,
            Err(why) => {
                report.error(&why);
                ok = false;
                break;
            }
        };
        if !select.takes(&entry.path) {
            continue;
        }
        match one(
            reader,
            target,
            space,
            existing,
            &entry,
            &mut folders,
            report,
        ) {
            Ok(()) => {}
            Err(Failed::Entry(why)) => {
                report.error(&format!("{}: {why}", entry.path));
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
        let set = target
            .mode(parts, *mode)
            .and_then(|()| target.modified(parts, *modified));
        if let Err(e) = set {
            report.error(&format!(
                "{}: cannot set its mode and time: {e}",
                inside::shown(parts)
            ));
            ok = false;
        }
    }
    for name in select.missing() {
        report.error(&format!("{name}: not found in the archive"));
        ok = false;
    }
    ok
}

fn refused(why: String) -> Failed {
    Failed::Entry(format!("refused: {why}"))
}

/// Extracts one entry.
fn one<R: Read>(
    reader: &mut Reader<R>,
    target: &Target,
    space: &mut dyn Space,
    existing: Existing,
    entry: &Entry,
    folders: &mut Vec<(Vec<String>, u32, i64)>,
    report: &mut dyn Report,
) -> Result<(), Failed> {
    if matches!(entry.kind, Kind::Device | Kind::Fifo | Kind::Other(_)) {
        report.note(&format!(
            "{}: skipped, a {} entry",
            entry.path,
            special(entry.kind)
        ));
        return Ok(());
    }
    let parts = inside::parts(&entry.path).map_err(refused)?;
    if parts.is_empty() {
        return match entry.kind {
            Kind::Folder => Ok(()),
            _ => Err(refused("it names the folder extracted into".into())),
        };
    }
    let folder = entry.kind == Kind::Folder;
    let at = target.resolve(&parts, folder).map_err(refused)?;
    target
        .room(space, &at, &entry.path, entry.size)
        .map_err(Failed::Stop)?;
    report.name(&entry.path);
    let failed = |why: String| Failed::Entry(why);
    if folder {
        target.folder(&at).map_err(failed)?;
        folders.push((at, entry.mode, entry.modified));
        return Ok(());
    }
    // What a hard link leads to, which must be in the folder.
    let to = match entry.kind {
        Kind::Hard => {
            let to = inside::parts(&entry.target)
                .and_then(|to| target.resolve(&to, true))
                .map_err(|why| refused(format!("the target of the hard link: {why}")))?;
            match target.what(&to) {
                Ok(Some(t)) if !t.is_dir() => {}
                _ => {
                    return Err(failed(format!(
                        "{} is not a file in the folder",
                        entry.target
                    )));
                }
            }
            if to == at {
                return Err(failed("the hard link leads to itself".into()));
            }
            Some(to)
        }
        _ => None,
    };
    if existing == Existing::Keep && target.what(&at).is_ok_and(|t| t.is_some()) {
        return Err(failed("it is there already, and kept".into()));
    }
    target.clear(&at).map_err(failed)?;
    target
        .parents(&at)
        .map_err(|e| failed(format!("cannot create its folder: {e}")))?;
    match entry.kind {
        Kind::Link => {
            target
                .link(&at, &entry.target)
                .map_err(|e| failed(format!("cannot create the link: {e}")))?;
        }
        Kind::Hard => {
            let to = to.unwrap_or_default();
            target
                .hard_link(&at, &to)
                .map_err(|e| failed(format!("cannot create the hard link: {e}")))?;
            return Ok(());
        }
        _ => {
            let mut file = target
                .create(&at)
                .map_err(|e| failed(format!("cannot create it: {e}")))?;
            if let Err(why) = inside::copy(reader, &mut file, entry.size) {
                drop(file);
                target.remove(&at);
                return Err(failed(why));
            }
            target
                .mode(&at, entry.mode)
                .map_err(|e| failed(format!("cannot set its mode: {e}")))?;
        }
    }
    target
        .modified(&at, entry.modified)
        .map_err(|e| failed(format!("cannot set its modified time: {e}")))
}

fn special(kind: Kind) -> &'static str {
    match kind {
        Kind::Device => "device",
        Kind::Fifo => "FIFO",
        _ => "special",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::create::tests::Told;
    use crate::header::{BLOCK, write};
    use crate::reader::tests::{archive, entry, raw};
    use mikros_extract::testing::{Fixed, Scratch};
    use std::fs;
    use std::os::unix::fs::{MetadataExt, symlink};
    use std::path::Path;

    fn link(path: &str, kind: Kind, target: &str) -> Entry {
        let mut e = entry(path, kind, 0);
        e.target = target.into();
        e
    }

    /// Extracts `bytes` into `root`, with the other options as given.
    fn run(
        bytes: &[u8],
        root: &Path,
        names: &[&str],
        existing: Existing,
        space: Fixed,
    ) -> (bool, Told) {
        let mut told = Told::default();
        let names: Vec<String> = names.iter().map(|n| n.to_string()).collect();
        let mut select = Select::new(&names, &[]);
        let target = Target::new(root).unwrap();
        let mut reader = Reader::new(bytes, true);
        let mut space = space;
        let ok = extract(
            &mut reader,
            &target,
            &mut space,
            &mut select,
            existing,
            &mut told,
        );
        (ok, told)
    }

    fn plenty() -> Fixed {
        Fixed(1 << 40, 1 << 39)
    }

    /// Every path under `folder`, sorted.
    fn tree(folder: &Path) -> Vec<String> {
        let mut out = Vec::new();
        let mut pending = vec![folder.to_path_buf()];
        while let Some(at) = pending.pop() {
            for item in fs::read_dir(&at).unwrap() {
                let path = item.unwrap().path();
                out.push(path.strip_prefix(folder).unwrap().display().to_string());
                if fs::symlink_metadata(&path).unwrap().is_dir() {
                    pending.push(path);
                }
            }
        }
        out.sort();
        out
    }

    #[test]
    fn extracts_with_modes_and_times() {
        let s = Scratch::new("extract");
        let root = s.join("root");
        fs::create_dir(&root).unwrap();
        let mut file = entry("d/f", Kind::File, 3);
        file.mode = 0o750;
        file.modified = 1_000_000;
        let mut folder = entry("d/", Kind::Folder, 0);
        folder.modified = 2_000_000;
        let bytes = archive(&[
            (folder, b""),
            (file, b"abc"),
            (link("d/l", Kind::Link, "/far/away"), b""),
            (link("d/h", Kind::Hard, "d/f"), b""),
            (entry("dev", Kind::Device, 0), b""),
        ]);
        let (ok, told) = run(&bytes, &root, &[], Existing::Replace, plenty());
        assert!(ok, "{:?}", told.errors);
        assert_eq!(tree(&root), ["d", "d/f", "d/h", "d/l"]);
        let f = fs::metadata(root.join("d/f")).unwrap();
        assert_eq!(
            (f.mode() & 0o7777, f.mtime(), f.nlink()),
            (0o750, 1_000_000, 2)
        );
        assert_eq!(fs::metadata(root.join("d")).unwrap().mtime(), 2_000_000);
        assert_eq!(
            fs::read_link(root.join("d/l")).unwrap(),
            Path::new("/far/away")
        );
        assert!(
            told.notes[0].starts_with("dev: skipped, a device"),
            "{:?}",
            told.notes
        );
        assert_eq!(told.names, ["d/", "d/f", "d/l", "d/h"]);
    }

    #[test]
    fn refuses_names_that_lead_out() {
        let s = Scratch::new("traversal");
        let root = s.join("root");
        fs::create_dir(&root).unwrap();
        let bytes = archive(&[
            (entry("../escape", Kind::File, 1), b"x"),
            (entry("a/../../escape", Kind::File, 1), b"x"),
            (
                entry(&s.join("absolute").display().to_string(), Kind::File, 1),
                b"x",
            ),
            (entry("fine", Kind::File, 1), b"y"),
        ]);
        let (ok, told) = run(&bytes, &root, &[], Existing::Replace, plenty());
        assert!(!ok);
        assert_eq!(told.errors.len(), 3, "{:?}", told.errors);
        assert!(told.errors[0].starts_with("../escape: refused: "));
        assert!(told.errors[2].contains("absolute"));
        assert_eq!(tree(&s.0), ["root", "root/fine"]);
    }

    #[test]
    fn refuses_paths_through_links_that_lead_out() {
        let s = Scratch::new("links");
        let root = s.join("root");
        fs::create_dir_all(&root).unwrap();
        fs::create_dir_all(s.join("outside")).unwrap();
        symlink(s.join("outside"), root.join("there")).unwrap();
        let bytes = archive(&[
            // Through a link that is there already.
            (entry("there/a", Kind::File, 1), b"x"),
            // Through a link that an earlier entry made, absolute or not.
            (link("made", Kind::Link, "../outside"), b""),
            (entry("made/b", Kind::File, 1), b"x"),
            (
                link("abs", Kind::Link, &s.join("outside").display().to_string()),
                b"",
            ),
            (entry("abs/c/", Kind::Folder, 0), b""),
            (link("abs/d", Kind::Link, "x"), b""),
            (link("made/e", Kind::Hard, "made"), b""),
            // A link inside may be passed through.
            (entry("in/", Kind::Folder, 0), b""),
            (link("ok", Kind::Link, "in"), b""),
            (entry("ok/f", Kind::File, 1), b"z"),
        ]);
        let (ok, told) = run(&bytes, &root, &[], Existing::Replace, plenty());
        assert!(!ok);
        assert_eq!(told.errors.len(), 5, "{:?}", told.errors);
        for error in &told.errors {
            assert!(error.contains("leads out of the folder"), "{error}");
        }
        assert!(fs::read_dir(s.join("outside")).unwrap().next().is_none());
        assert_eq!(fs::read(root.join("in/f")).unwrap(), b"z");
    }

    #[test]
    fn refuses_hard_links_that_lead_out() {
        let s = Scratch::new("hard");
        let root = s.join("root");
        fs::create_dir_all(&root).unwrap();
        fs::write(s.join("secret"), b"secret").unwrap();
        symlink(s.join("secret"), root.join("points-out")).unwrap();
        let bytes = archive(&[
            (link("h1", Kind::Hard, "../secret"), b""),
            (
                link("h2", Kind::Hard, &s.join("secret").display().to_string()),
                b"",
            ),
            (link("h3", Kind::Hard, "points-out"), b""),
            (link("h4", Kind::Hard, "missing"), b""),
        ]);
        let (ok, told) = run(&bytes, &root, &[], Existing::Replace, plenty());
        assert!(!ok);
        assert_eq!(told.errors.len(), 4, "{:?}", told.errors);
        assert_eq!(tree(&root), ["points-out"]);
        assert_eq!(fs::metadata(s.join("secret")).unwrap().nlink(), 1);
    }

    #[test]
    fn replaces_links_instead_of_writing_through_them() {
        let s = Scratch::new("replace");
        let root = s.join("root");
        fs::create_dir_all(&root).unwrap();
        fs::write(s.join("victim"), b"keep").unwrap();
        let bytes = archive(&[
            (
                link("f", Kind::Link, &s.join("victim").display().to_string()),
                b"",
            ),
            (entry("f", Kind::File, 3), b"new"),
        ]);
        let (ok, told) = run(&bytes, &root, &[], Existing::Replace, plenty());
        assert!(ok, "{:?}", told.errors);
        assert_eq!(fs::read(s.join("victim")).unwrap(), b"keep");
        assert_eq!(fs::read(root.join("f")).unwrap(), b"new");
        // With -k, what is there stays and the entry fails.
        let bytes = archive(&[(entry("f", Kind::File, 3), b"old")]);
        let (ok, told) = run(&bytes, &root, &[], Existing::Keep, plenty());
        assert!(!ok && told.errors[0].contains("kept"));
        assert_eq!(fs::read(root.join("f")).unwrap(), b"new");
    }

    #[test]
    fn removes_entries_whose_data_is_cut_short() {
        let s = Scratch::new("short");
        let bytes = archive(&[(entry("f", Kind::File, 2000), &[1; 2000])]);
        let (ok, told) = run(&bytes[..1500], &s.0, &[], Existing::Replace, plenty());
        assert!(!ok);
        assert!(told.errors[0].contains("shorter"), "{:?}", told.errors);
        assert!(tree(&s.0).is_empty());
    }

    #[test]
    fn stops_before_an_entry_that_does_not_fit() {
        let s = Scratch::new("space");
        let bytes = archive(&[
            (entry("small", Kind::File, 10), &[1; 10]),
            (entry("big", Kind::File, 100), &[1; 100]),
            (entry("after", Kind::File, 1), b"x"),
        ]);
        let (ok, told) = run(&bytes, &s.0, &[], Existing::Replace, Fixed(1000, 140));
        assert!(!ok);
        assert_eq!(told.errors.len(), 1);
        assert!(
            told.errors[0].starts_with("big: its 100 bytes"),
            "{:?}",
            told.errors
        );
        assert_eq!(tree(&s.0), ["small"]);
    }

    #[test]
    fn refuses_oversized_headers_and_goes_on() {
        let s = Scratch::new("oversized");
        let size = (1 << 20) + 1;
        let mut bytes = raw(b'L', "././@LongLink", size);
        bytes.extend(vec![b'n'; (size as usize).next_multiple_of(BLOCK)]);
        bytes.extend(write(&entry("victim", Kind::File, 0)));
        bytes.extend(archive(&[(entry("next", Kind::File, 1), b"n")]));
        let (ok, told) = run(&bytes, &s.0, &[], Existing::Replace, plenty());
        assert!(!ok);
        assert!(
            told.errors[0].starts_with("victim: refused: its GNU long name"),
            "{:?}",
            told.errors
        );
        assert_eq!(tree(&s.0), ["next"]);
    }

    #[test]
    fn takes_named_entries_and_names_those_not_found() {
        let s = Scratch::new("named");
        let bytes = archive(&[
            (entry("a/1", Kind::File, 1), b"1"),
            (entry("b", Kind::File, 1), b"2"),
        ]);
        let (ok, told) = run(&bytes, &s.0, &["a", "zzz"], Existing::Replace, plenty());
        assert!(!ok);
        assert_eq!(told.errors, ["zzz: not found in the archive"]);
        assert_eq!(tree(&s.0), ["a", "a/1"]);
    }

    #[test]
    fn leaves_archives_inside_archives_as_they_are() {
        let s = Scratch::new("nested");
        let inner = archive(&[(entry("inner", Kind::File, 1), b"i")]);
        let bytes = archive(&[(entry("inner.tar", Kind::File, inner.len() as u64), &inner)]);
        let (ok, _) = run(&bytes, &s.0, &[], Existing::Replace, plenty());
        assert!(ok);
        assert_eq!(tree(&s.0), ["inner.tar"]);
        assert_eq!(fs::read(s.join("inner.tar")).unwrap(), inner);
    }
}
