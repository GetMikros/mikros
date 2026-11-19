//! Creating a pax archive (MIKSPEC-0098.R57) from files, folders, links
//! and hard links, read through the standard library: folders with what
//! they hold, in the order of names, links as links, and each further
//! name of a file as a hard link to the first. Each entry keeps its
//! stored mode and modified time; owners are written as 0, without
//! names.

use crate::args::Select;
use crate::header::{self, BLOCK, Entry, Kind};
use std::collections::HashMap;
use std::fs::{self, File, Metadata};
use std::io::{self, Read, Write};
use std::os::unix::fs::MetadataExt;
use std::path::Path;

/// What a run tells as it goes.
pub trait Report {
    /// An entry's name, as `-v` prints it.
    fn name(&mut self, name: &str);
    /// A failure, after which the run exits with 1.
    fn error(&mut self, message: &str);
    /// A message that is not a failure.
    fn note(&mut self, message: &str);
}

/// Writes an archive to `out`.
pub struct Creator<'a, W: Write> {
    out: W,
    written: u64,
    select: &'a mut Select,
    report: &'a mut dyn Report,
    /// The first name of each file with more than one, by device and
    /// inode.
    names: HashMap<(u64, u64), String>,
    /// The device and inode of the archive, which is not stored.
    archive: Option<(u64, u64)>,
    /// Whether the notes about names cut at their start were given.
    told: (bool, bool),
}

impl<'a, W: Write> Creator<'a, W> {
    pub fn new(
        out: W,
        select: &'a mut Select,
        report: &'a mut dyn Report,
        archive: Option<(u64, u64)>,
    ) -> Creator<'a, W> {
        Creator {
            out,
            written: 0,
            select,
            report,
            names: HashMap::new(),
            archive,
            told: (false, false),
        }
    }

    /// Stores `operand`, found at `path`, with all it holds. An error
    /// writing the archive ends the run.
    pub fn add(&mut self, path: &Path, operand: &str) -> io::Result<()> {
        let name = self.stored(operand);
        self.store(path, name)
    }

    /// Ends the archive and gives back where it went.
    pub fn finish(mut self) -> io::Result<W> {
        let tail = header::end(self.written);
        self.out.write_all(&tail)?;
        self.out.flush()?;
        Ok(self.out)
    }

    /// The name that `operand` is stored as: without `/` at its start,
    /// and without what comes before its last `..` part, with a note.
    fn stored(&mut self, operand: &str) -> String {
        let mut name = operand;
        if name.starts_with('/') {
            name = name.trim_start_matches('/');
            if !self.told.0 {
                self.told.0 = true;
                self.report.note("removing the leading / from names");
            }
        }
        let parts: Vec<&str> = name.split('/').collect();
        if let Some(last) = parts.iter().rposition(|p| *p == "..") {
            if !self.told.1 {
                self.told.1 = true;
                self.report.note("removing what comes before .. from names");
            }
            return parts[last + 1..].join("/");
        }
        match name.is_empty() {
            true => ".".into(),
            false => name.into(),
        }
    }

    fn store(&mut self, path: &Path, name: String) -> io::Result<()> {
        let metadata = match fs::symlink_metadata(path) {
            Ok(m) => m,
            Err(e) => {
                self.report
                    .error(&format!("cannot access {}: {e}", path.display()));
                return Ok(());
            }
        };
        if self.select.excluded(&name) {
            return Ok(());
        }
        if self.archive == Some((metadata.dev(), metadata.ino())) {
            self.report
                .note(&format!("{name}: is the archive, not stored"));
            return Ok(());
        }
        let file_type = metadata.file_type();
        if file_type.is_dir() {
            let mut folder = name.trim_end_matches('/').to_string();
            folder.push('/');
            self.write_entry(&metadata, folder.clone(), Kind::Folder, String::new())?;
            let mut children: Vec<_> = match fs::read_dir(path) {
                Ok(listing) => listing
                    .filter_map(|item| match item {
                        Ok(item) => Some(item.file_name()),
                        Err(e) => {
                            self.report
                                .error(&format!("reading {}: {e}", path.display()));
                            None
                        }
                    })
                    .collect(),
                Err(e) => {
                    self.report
                        .error(&format!("cannot open folder {}: {e}", path.display()));
                    return Ok(());
                }
            };
            children.sort();
            for child in children {
                let child_name = format!("{folder}{}", child.to_string_lossy());
                self.store(&path.join(&child), child_name)?;
            }
        } else if file_type.is_symlink() {
            match fs::read_link(path) {
                Ok(target) => {
                    let target = target.to_string_lossy().into_owned();
                    self.write_entry(&metadata, name, Kind::Link, target)?;
                }
                Err(e) => self
                    .report
                    .error(&format!("cannot read {}: {e}", path.display())),
            }
        } else if file_type.is_file() {
            let key = (metadata.dev(), metadata.ino());
            if metadata.nlink() > 1 {
                if let Some(first) = self.names.get(&key).cloned() {
                    return self.write_entry(&metadata, name, Kind::Hard, first);
                }
                self.names.insert(key, name.clone());
            }
            match File::open(path) {
                Ok(file) => self.write_file(&metadata, name, file)?,
                Err(e) => self
                    .report
                    .error(&format!("cannot open {}: {e}", path.display())),
            }
        } else {
            self.report.note(&format!(
                "{name}: is not a file, folder or link, not stored"
            ));
        }
        Ok(())
    }

    fn entry(metadata: &Metadata, path: String, kind: Kind, target: String) -> Entry {
        Entry {
            path,
            kind,
            size: match kind {
                Kind::File => metadata.len(),
                _ => 0,
            },
            mode: metadata.mode() & 0o7777,
            modified: metadata.mtime(),
            target,
        }
    }

    fn write_entry(
        &mut self,
        metadata: &Metadata,
        path: String,
        kind: Kind,
        target: String,
    ) -> io::Result<()> {
        self.report.name(&path);
        let entry = Self::entry(metadata, path, kind, target);
        self.put(&header::write(&entry))
    }

    /// Stores a file with as many bytes as it had when it was looked at:
    /// what it lost since is filled with zeros, and what it gained left
    /// out.
    fn write_file(&mut self, metadata: &Metadata, path: String, file: File) -> io::Result<()> {
        self.report.name(&path);
        let entry = Self::entry(metadata, path, Kind::File, String::new());
        self.put(&header::write(&entry))?;
        let mut data = file.take(entry.size);
        let mut buffer = vec![0; 64 * 1024];
        let mut copied = 0;
        loop {
            let got = match data.read(&mut buffer) {
                Ok(0) => break,
                Ok(got) => got,
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                Err(e) => {
                    self.report.error(&format!("reading {}: {e}", entry.path));
                    break;
                }
            };
            self.put(&buffer[..got])?;
            copied += got as u64;
        }
        if copied < entry.size {
            let missing = entry.size - copied;
            self.report.error(&format!(
                "{}: it became shorter while it was read; {missing} bytes are stored as zeros",
                entry.path
            ));
            let zeros = vec![0; BLOCK];
            let mut left = missing;
            while left > 0 {
                let n = left.min(BLOCK as u64) as usize;
                self.put(&zeros[..n])?;
                left -= n as u64;
            }
        }
        let pad = entry.size.next_multiple_of(BLOCK as u64) - entry.size;
        self.put(&vec![0; pad as usize])
    }

    fn put(&mut self, bytes: &[u8]) -> io::Result<()> {
        self.out.write_all(bytes)?;
        self.written += bytes.len() as u64;
        Ok(())
    }
}

#[cfg(test)]
pub mod tests {
    use super::*;
    use crate::reader::{Next, Reader};
    use mikros_extract::testing::Scratch;
    use std::os::unix::fs::{PermissionsExt, symlink};

    /// What a run told, for tests.
    #[derive(Default)]
    pub struct Told {
        pub names: Vec<String>,
        pub errors: Vec<String>,
        pub notes: Vec<String>,
    }

    impl Report for Told {
        fn name(&mut self, name: &str) {
            self.names.push(name.into());
        }
        fn error(&mut self, message: &str) {
            self.errors.push(message.into());
        }
        fn note(&mut self, message: &str) {
            self.notes.push(message.into());
        }
    }

    pub fn entries(bytes: &[u8]) -> Vec<(Entry, Vec<u8>)> {
        let mut reader = Reader::new(bytes, true);
        let mut out = Vec::new();
        while let Next::Entry(entry) = reader.next().unwrap() {
            let mut data = Vec::new();
            reader.read_to_end(&mut data).unwrap();
            out.push((entry, data));
        }
        out
    }

    #[test]
    fn stores_folders_files_links_and_hard_links() {
        let s = Scratch::new("create");
        fs::create_dir_all(s.join("d/sub")).unwrap();
        fs::write(s.join("d/b.txt"), b"bee").unwrap();
        fs::write(s.join("d/a.o"), b"object").unwrap();
        fs::set_permissions(s.join("d/b.txt"), fs::Permissions::from_mode(0o751)).unwrap();
        symlink("b.txt", s.join("d/link")).unwrap();
        fs::hard_link(s.join("d/b.txt"), s.join("d/sub/hard")).unwrap();
        let mut select = Select::new(&[], &["*.o"]);
        let mut told = Told::default();
        let mut creator = Creator::new(Vec::new(), &mut select, &mut told, None);
        creator.add(&s.join("d"), "d").unwrap();
        let bytes = creator.finish().unwrap();
        assert_eq!(bytes.len() % 10240, 0);
        let read = entries(&bytes);
        let names: Vec<&str> = read.iter().map(|(e, _)| e.path.as_str()).collect();
        assert_eq!(names, ["d/", "d/b.txt", "d/link", "d/sub/", "d/sub/hard"]);
        assert_eq!(told.names, names);
        let (b, data) = &read[1];
        assert_eq!(
            (b.kind, b.mode, data.as_slice()),
            (Kind::File, 0o751, &b"bee"[..])
        );
        assert_eq!(b.modified, fs::metadata(s.join("d/b.txt")).unwrap().mtime());
        assert_eq!(
            (read[2].0.kind, read[2].0.target.as_str()),
            (Kind::Link, "b.txt")
        );
        assert_eq!(
            (read[4].0.kind, read[4].0.target.as_str()),
            (Kind::Hard, "d/b.txt")
        );
        // Owners are 0 without names.
        assert_eq!(&bytes[108..124], b"0000000\x000000000\x00");
        assert!(told.errors.is_empty());
    }

    #[test]
    fn cuts_names_at_the_root_and_parent_parts() {
        let s = Scratch::new("names");
        fs::write(s.join("f"), b"x").unwrap();
        let mut select = Select::new(&[], &[]);
        let mut told = Told::default();
        let mut creator = Creator::new(Vec::new(), &mut select, &mut told, None);
        let absolute = s.join("f").display().to_string();
        creator.add(&s.join("f"), &absolute).unwrap();
        creator.add(&s.join("f"), "../up/f").unwrap();
        let bytes = creator.finish().unwrap();
        let read = entries(&bytes);
        assert_eq!(read[0].0.path, absolute.trim_start_matches('/'));
        assert_eq!(read[1].0.path, "up/f");
        assert_eq!(told.notes.len(), 2);
        // The archive itself is left out.
        let metadata = fs::metadata(s.join("f")).unwrap();
        let mut told = Told::default();
        let key = Some((metadata.dev(), metadata.ino()));
        let mut creator = Creator::new(Vec::new(), &mut select, &mut told, key);
        creator.add(&s.join("f"), "f").unwrap();
        assert!(entries(&creator.finish().unwrap()).is_empty());
        assert!(told.notes[0].contains("is the archive"));
    }

    #[test]
    fn names_what_it_cannot_store() {
        let s = Scratch::new("missing");
        let mut select = Select::new(&[], &[]);
        let mut told = Told::default();
        let mut creator = Creator::new(Vec::new(), &mut select, &mut told, None);
        creator.add(&s.join("none"), "none").unwrap();
        creator.finish().unwrap();
        assert!(
            told.errors[0].starts_with("cannot access ")
                && told.errors[0].ends_with("none: No such file or directory (os error 2)")
        );
    }
}
