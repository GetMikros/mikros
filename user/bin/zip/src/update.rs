//! What zip stores (MIKSPEC-0098.R58): the paths given, folders with
//! what they hold under `-r`, links as links under `-y` and otherwise
//! what they lead to, less what `-x` matches; and a new archive made of
//! the entries of the old one, with those of the same name replaced and
//! the new ones after them, or without the entries that `-d` names.

use crate::args;
use crate::central::{Directory, S_IFDIR, S_IFLNK, S_IFREG};
use crate::pattern::Pattern;
use crate::writing::{Data, Writer};
use std::collections::HashSet;
use std::fs::{self, File, Metadata};
use std::io::{self, Read, Seek, Write};
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

/// What a run tells as it goes.
pub trait Report {
    /// What was done with an entry, as `adding`, its name, and how it
    /// was compressed.
    fn did(&mut self, what: &str, name: &str, how: &str);
    /// A failure, after which the run exits with 1.
    fn error(&mut self, message: &str);
    /// A message that is not a failure.
    fn note(&mut self, message: &str);
}

/// How a run gathers and stores.
pub struct Options {
    pub recurse: bool,
    pub junk: bool,
    /// `-y`.
    pub links: bool,
    pub exclude: Vec<Pattern>,
}

/// What an entry to store is.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum What {
    File,
    Folder,
    Link(String),
}

/// A path to store.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Member {
    pub name: String,
    pub path: PathBuf,
    pub what: What,
    /// The stored mode, with its type.
    pub mode: u32,
    pub modified: i64,
}

/// The paths to store, from `operands`, leaving out the archive, by its
/// device and inode.
pub fn gather(
    operands: &[String],
    options: &Options,
    archive: Option<(u64, u64)>,
    report: &mut dyn Report,
) -> Vec<Member> {
    let mut members = Vec::new();
    for operand in operands {
        visit(
            Path::new(operand),
            operand,
            options,
            archive,
            &mut Vec::new(),
            &mut members,
            report,
        );
    }
    members
}

fn visit(
    path: &Path,
    given: &str,
    options: &Options,
    archive: Option<(u64, u64)>,
    folders: &mut Vec<(u64, u64)>,
    members: &mut Vec<Member>,
    report: &mut dyn Report,
) {
    let metadata = match options.links {
        true => fs::symlink_metadata(path),
        false => fs::metadata(path),
    };
    let metadata: Metadata = match metadata {
        Ok(m) => m,
        Err(e) => {
            report.error(&format!("cannot access {given}: {e}"));
            return;
        }
    };
    let key = (metadata.dev(), metadata.ino());
    if archive == Some(key) {
        return;
    }
    let name = args::stored(given, options.junk);
    if !name.is_empty() && options.exclude.iter().any(|p| p.matches(&name)) {
        return;
    }
    let mode = metadata.mode() & 0o7777;
    let modified = metadata.mtime();
    let member = |name: String, what: What, kind: u32| Member {
        name,
        path: path.to_path_buf(),
        what,
        mode: kind | mode,
        modified,
    };
    let file_type = metadata.file_type();
    if file_type.is_symlink() {
        match fs::read_link(path) {
            Ok(target) => {
                let target = target.to_string_lossy().into_owned();
                members.push(member(name, What::Link(target), S_IFLNK));
            }
            Err(e) => report.error(&format!("cannot read {given}: {e}")),
        }
    } else if file_type.is_file() {
        members.push(member(name, What::File, S_IFREG));
    } else if file_type.is_dir() {
        if !name.is_empty() && !options.junk {
            members.push(member(format!("{name}/"), What::Folder, S_IFDIR));
        }
        if !options.recurse {
            return;
        }
        if folders.contains(&key) {
            report.note(&format!(
                "{given}: a link leads back to a folder it is in, not followed"
            ));
            return;
        }
        let mut children: Vec<_> = match fs::read_dir(path) {
            Ok(listing) => listing
                .filter_map(Result::ok)
                .map(|e| e.file_name())
                .collect(),
            Err(e) => {
                report.error(&format!("cannot open folder {given}: {e}"));
                return;
            }
        };
        children.sort();
        folders.push(key);
        for child in children {
            let child_given = format!(
                "{}/{}",
                given.trim_end_matches('/'),
                child.to_string_lossy()
            );
            visit(
                &path.join(&child),
                &child_given,
                options,
                archive,
                folders,
                members,
                report,
            );
        }
        folders.pop();
    } else {
        report.note(&format!("{given}: not a file, folder or link, not stored"));
    }
}

/// How an entry was compressed, as zip prints it.
fn how(size: u64, compressed: u64, method: u16) -> String {
    let saved = match size {
        0 => 0,
        _ => 100 - (u128::from(compressed) * 100 / u128::from(size)).min(100) as i64,
    };
    match method {
        0 => "(stored 0%)".into(),
        _ => format!("(deflated {saved}%)"),
    }
}

/// Writes `member`, or why it could not be; errors writing the archive
/// are errors of the result.
fn store<W: Write + Seek>(
    writer: &mut Writer<W>,
    member: &Member,
    level: u32,
) -> io::Result<Result<(u64, u64, u16), String>> {
    let added = match &member.what {
        What::Folder => writer.add(
            &member.name,
            member.mode,
            member.modified,
            Data::Bytes(b""),
            level,
        )?,
        What::Link(target) => {
            let data = Data::Bytes(target.as_bytes());
            writer.add(&member.name, member.mode, member.modified, data, level)?
        }
        What::File => {
            let mut file = match File::open(&member.path) {
                Ok(file) => file,
                Err(e) => return Ok(Err(format!("cannot open {}: {e}", member.path.display()))),
            };
            let length = match file.metadata() {
                Ok(m) => m.len(),
                Err(e) => return Ok(Err(format!("cannot access {}: {e}", member.path.display()))),
            };
            writer.add(
                &member.name,
                member.mode,
                member.modified,
                Data::File(&mut file, length),
                level,
            )?
        }
    };
    Ok(added
        .map(|r| (r.size, r.compressed, r.method))
        .map_err(|e| format!("reading {}: {e}", member.path.display())))
}

/// Writes to `out` the entries of `old`, read from `from`, with those
/// of the names of `members` replaced, and the other members after
/// them; gives back the output and the archive's length.
pub fn update<W: Write + Seek, F: Read + Seek>(
    out: W,
    old: Option<(&mut F, &Directory)>,
    members: &[Member],
    level: u32,
    report: &mut dyn Report,
) -> io::Result<(W, u64)> {
    let mut writer = Writer::new(out);
    // The members by name, the first of each; later ones are refused.
    let mut names = HashSet::new();
    let mut unique = Vec::new();
    for member in members {
        match names.insert(member.name.as_str()) {
            true => unique.push(member),
            false => report.error(&format!(
                "{}: the name is given twice, the second time by {}",
                member.name,
                member.path.display()
            )),
        }
    }
    let mut done = HashSet::new();
    if let Some((from, directory)) = old {
        for record in &directory.records {
            let replacing = unique.iter().find(|m| m.name == record.name);
            if let Some(member) = replacing {
                done.insert(member.name.as_str());
                match store(&mut writer, member, level)? {
                    Ok((size, compressed, method)) => {
                        report.did("updating", &member.name, &how(size, compressed, method));
                        continue;
                    }
                    Err(why) => report.error(&format!("{why}; the entry is kept as it was")),
                }
            }
            if let Err(why) = writer.copy(from, record) {
                report.error(&why);
            }
        }
    }
    for member in unique {
        if done.contains(member.name.as_str()) {
            continue;
        }
        match store(&mut writer, member, level)? {
            Ok((size, compressed, method)) => {
                report.did("adding", &member.name, &how(size, compressed, method))
            }
            Err(why) => report.error(&why),
        }
    }
    writer.finish()
}

/// Writes to `out` the entries of `directory`, read from `from`, but
/// those that one of `names`, wildcard patterns, matches.
pub fn delete<W: Write + Seek, F: Read + Seek>(
    out: W,
    from: &mut F,
    directory: &Directory,
    names: &[String],
    report: &mut dyn Report,
) -> io::Result<(W, u64)> {
    let patterns: Vec<Pattern> = names.iter().map(|n| Pattern::new(n)).collect();
    let mut matched = vec![false; names.len()];
    let mut writer = Writer::new(out);
    for record in &directory.records {
        let mut removed = false;
        for (pattern, matched) in patterns.iter().zip(&mut matched) {
            if pattern.matches(&record.name) {
                *matched = true;
                removed = true;
            }
        }
        if removed {
            report.did("deleting", &record.name, "");
        } else if let Err(why) = writer.copy(from, record) {
            report.error(&why);
        }
    }
    for (name, matched) in names.iter().zip(matched) {
        if !matched {
            report.error(&format!("{name}: no entry matches it"));
        }
    }
    writer.finish()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::central;
    use crate::writing::tests::data;
    use std::os::unix::fs::{PermissionsExt, symlink};
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct Scratch(PathBuf);

    impl Scratch {
        fn new(name: &str) -> Scratch {
            static COUNT: AtomicUsize = AtomicUsize::new(0);
            let n = COUNT.fetch_add(1, Ordering::Relaxed);
            let path =
                std::env::temp_dir().join(format!("mikros-zip-{name}-{}-{n}", std::process::id()));
            let _ = fs::remove_dir_all(&path);
            fs::create_dir_all(&path).unwrap();
            Scratch(path)
        }

        fn at(&self, path: &str) -> String {
            self.0.join(path).display().to_string()
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[derive(Default)]
    struct Told {
        did: Vec<String>,
        errors: Vec<String>,
        notes: Vec<String>,
    }

    impl Report for Told {
        fn did(&mut self, what: &str, name: &str, _: &str) {
            self.did.push(format!("{what} {name}"));
        }
        fn error(&mut self, message: &str) {
            self.errors.push(message.into());
        }
        fn note(&mut self, message: &str) {
            self.notes.push(message.into());
        }
    }

    fn options() -> Options {
        Options {
            recurse: true,
            junk: false,
            links: true,
            exclude: Vec::new(),
        }
    }

    fn names(members: &[Member]) -> Vec<String> {
        members.iter().map(|m| m.name.clone()).collect()
    }

    /// A folder with a file, a link and a folder inside.
    fn sample(s: &Scratch) -> String {
        fs::create_dir_all(s.0.join("d/sub")).unwrap();
        fs::write(s.0.join("d/f.txt"), b"text").unwrap();
        fs::set_permissions(s.0.join("d/f.txt"), fs::Permissions::from_mode(0o750)).unwrap();
        fs::write(s.0.join("d/sub/g.o"), b"object").unwrap();
        symlink("f.txt", s.0.join("d/l")).unwrap();
        s.at("d")
    }

    fn relative(s: &Scratch, members: &[Member]) -> Vec<String> {
        let prefix = s.0.display().to_string();
        let prefix = prefix.trim_start_matches('/');
        names(members)
            .into_iter()
            .map(|n| {
                n.strip_prefix(prefix)
                    .unwrap_or(&n)
                    .trim_start_matches('/')
                    .to_string()
            })
            .collect()
    }

    #[test]
    fn gathers_folders_files_and_links() {
        let s = Scratch::new("gather");
        let d = sample(&s);
        let mut told = Told::default();
        let members = gather(std::slice::from_ref(&d), &options(), None, &mut told);
        assert_eq!(
            relative(&s, &members),
            ["d/", "d/f.txt", "d/l", "d/sub/", "d/sub/g.o"]
        );
        assert_eq!(members[1].mode, S_IFREG | 0o750);
        assert_eq!(members[2].what, What::Link("f.txt".into()));
        // Without -r, a folder alone; without -y, what links lead to.
        let plain = Options {
            recurse: false,
            links: false,
            ..options()
        };
        let members = gather(&[d.clone(), s.at("d/l")], &plain, None, &mut told);
        assert_eq!(relative(&s, &members), ["d/", "d/l"]);
        assert_eq!(members[1].what, What::File);
        // -j and -x.
        let junk = Options {
            junk: true,
            exclude: vec![Pattern::new("*.o")],
            ..options()
        };
        assert_eq!(names(&gather(&[d], &junk, None, &mut told)), ["f.txt", "l"]);
        assert!(told.errors.is_empty());
    }

    #[test]
    fn leaves_out_the_archive_and_names_what_is_missing() {
        let s = Scratch::new("missing");
        fs::write(s.0.join("a.zip"), b"").unwrap();
        let m = fs::metadata(s.0.join("a.zip")).unwrap();
        let mut told = Told::default();
        let members = gather(
            &[s.at("a.zip"), s.at("none")],
            &options(),
            Some((m.dev(), m.ino())),
            &mut told,
        );
        assert!(members.is_empty());
        assert!(told.errors[0].starts_with("cannot access ") && told.errors[0].contains("none"));
    }

    #[test]
    fn follows_links_without_going_round() {
        let s = Scratch::new("round");
        fs::create_dir_all(s.0.join("d")).unwrap();
        symlink("..", s.0.join("d/up")).unwrap();
        let mut told = Told::default();
        let follow = Options {
            links: false,
            ..options()
        };
        let members = gather(&[s.at("d")], &follow, None, &mut told);
        assert!(members.len() < 10, "{:?}", names(&members));
        assert!(!told.notes.is_empty());
    }

    fn write(old: Option<&[u8]>, members: &[Member], told: &mut Told) -> Vec<u8> {
        let out = io::Cursor::new(Vec::new());
        let mut from = io::Cursor::new(old.unwrap_or_default());
        let directory = old.map(|_| central::read(&mut from).unwrap());
        let old = directory.as_ref().map(|d| (&mut from, d));
        let (out, length) = update(out, old, members, 6, told).unwrap();
        let mut bytes = out.into_inner();
        bytes.truncate(length as usize);
        bytes
    }

    #[test]
    fn replaces_entries_of_the_same_name_and_adds_the_others() {
        let s = Scratch::new("update");
        let d = sample(&s);
        let mut told = Told::default();
        let junk = Options {
            junk: true,
            recurse: true,
            ..options()
        };
        let first = write(
            None,
            &gather(std::slice::from_ref(&d), &junk, None, &mut told),
            &mut told,
        );
        fs::write(s.0.join("d/f.txt"), b"changed").unwrap();
        fs::write(s.0.join("new"), b"new").unwrap();
        let mut told = Told::default();
        let members = gather(&[s.at("d/f.txt"), s.at("new")], &junk, None, &mut told);
        let second = write(Some(&first), &members, &mut told);
        assert_eq!(told.did, ["updating f.txt", "adding new"]);
        let directory = central::read(&mut io::Cursor::new(&second)).unwrap();
        let listed: Vec<&str> = directory.records.iter().map(|r| r.name.as_str()).collect();
        assert_eq!(listed, ["f.txt", "l", "g.o", "new"]);
        assert_eq!(data(&second, &directory.records[0]), b"changed");
        assert_eq!(data(&second, &directory.records[2]), b"object");
    }

    #[test]
    fn refuses_a_name_given_twice() {
        let s = Scratch::new("twice");
        fs::create_dir_all(s.0.join("a")).unwrap();
        fs::create_dir_all(s.0.join("b")).unwrap();
        fs::write(s.0.join("a/x"), b"1").unwrap();
        fs::write(s.0.join("b/x"), b"2").unwrap();
        let mut told = Told::default();
        let junk = Options {
            junk: true,
            ..options()
        };
        let members = gather(&[s.at("a/x"), s.at("b/x")], &junk, None, &mut told);
        let bytes = write(None, &members, &mut told);
        assert_eq!(told.errors.len(), 1);
        assert_eq!(
            central::read(&mut io::Cursor::new(&bytes))
                .unwrap()
                .records
                .len(),
            1
        );
    }

    #[test]
    fn deletes_the_entries_that_match() {
        let s = Scratch::new("delete");
        let d = sample(&s);
        let mut told = Told::default();
        let junk = Options {
            junk: true,
            ..options()
        };
        let first = write(None, &gather(&[d], &junk, None, &mut told), &mut told);
        let mut from = io::Cursor::new(&first);
        let directory = central::read(&mut from).unwrap();
        let mut told = Told::default();
        let patterns = ["*.txt".to_string(), "none".to_string()];
        let (out, length) = delete(
            io::Cursor::new(Vec::new()),
            &mut from,
            &directory,
            &patterns,
            &mut told,
        )
        .unwrap();
        let mut bytes = out.into_inner();
        bytes.truncate(length as usize);
        let left = central::read(&mut io::Cursor::new(&bytes)).unwrap();
        let listed: Vec<&str> = left.records.iter().map(|r| r.name.as_str()).collect();
        assert_eq!(listed, ["l", "g.o"]);
        assert_eq!(told.did, ["deleting f.txt"]);
        assert_eq!(told.errors, ["none: no entry matches it"]);
    }

    #[test]
    fn says_how_much_was_saved() {
        assert_eq!(how(100, 40, 8), "(deflated 60%)");
        assert_eq!(how(0, 2, 8), "(deflated 0%)");
        assert_eq!(how(10, 12, 8), "(deflated 0%)");
        assert_eq!(how(5, 5, 0), "(stored 0%)");
    }
}
