//! ls on Mikros: it reads folders and entries through the standard
//! library, and asks each entry's node connection what the caller may do
//! and whether it has an extended access control list, for long lines
//! and JSON (MIKSPEC-0098.R33).

use crate::listing::{self, COMMAND, Entry, Hidden, Show, Type};
use mikros_cli::{FAILURE, Paint, Run, SUCCESS, complain, start};
use mikros_command::node;
use std::ffi::c_int;
use std::fs::{self, Metadata};
use std::io::{IsTerminal, Write};
use std::os::unix::fs::{FileTypeExt, MetadataExt};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

const TIOCGWINSZ: c_int = 0x5413;

#[repr(C)]
#[derive(Default)]
struct Winsize {
    rows: u16,
    cols: u16,
    xpixel: u16,
    ypixel: u16,
}

unsafe extern "C" {
    fn ioctl(fd: c_int, request: c_int, ...) -> c_int;
}

/// What the run asks and how it writes.
struct Lister<'a> {
    run: &'a Run,
    show: Show,
    hidden: Hidden,
    paint: Paint,
    columns: Option<usize>,
    /// Whether long lines or JSON need access and lists.
    details: bool,
    failed: bool,
    out: String,
    /// Every entry, for JSON.
    all: Vec<Entry>,
}

pub fn main() -> ExitCode {
    let run = start(&COMMAND, &mikros_command::list);
    let show = Show::of(&run);
    let paint = match show.json {
        true => Paint::never(),
        false => Paint::stdout(&run),
    };
    let mut lister = Lister {
        run: &run,
        show,
        hidden: Hidden::of(&run),
        paint,
        columns: columns(),
        details: show.long || show.json,
        failed: false,
        out: String::new(),
        all: Vec::new(),
    };
    let operands: Vec<String> = match run.operands.is_empty() {
        true => vec![".".into()],
        false => run.operands.clone(),
    };
    lister.list(&operands);
    let text = match show.json {
        true => listing::json(&lister.all),
        false => std::mem::take(&mut lister.out),
    };
    let written = std::io::stdout().write_all(text.as_bytes());
    match lister.failed || written.is_err() {
        true => ExitCode::from(FAILURE),
        false => ExitCode::from(SUCCESS),
    }
}

/// The terminal's width, when standard output is one.
fn columns() -> Option<usize> {
    if !std::io::stdout().is_terminal() {
        return None;
    }
    let mut size = Winsize::default();
    // SAFETY: the pointer is valid for a `Winsize`.
    let got = unsafe { ioctl(1, TIOCGWINSZ, &mut size as *mut Winsize) };
    match got == 0 && size.cols > 0 {
        true => Some(usize::from(size.cols)),
        false => Some(80),
    }
}

impl Lister<'_> {
    /// Lists the operands: the files together first, then each folder,
    /// with its path above it when there are several.
    fn list(&mut self, operands: &[String]) {
        let mut files = Vec::new();
        let mut folders = Vec::new();
        for operand in operands {
            let path = Path::new(operand);
            // Operands are followed, as their entries are not.
            let metadata = fs::metadata(path).or_else(|_| fs::symlink_metadata(path));
            match metadata {
                Err(e) => self.fail(&format!("cannot access {operand}: {e}")),
                Ok(m) if m.is_dir() && !self.run.has("-d") => folders.push(PathBuf::from(operand)),
                Ok(_) => {
                    if let Some(entry) = self.entry(path, operand, true) {
                        files.push(entry);
                    }
                }
            }
        }
        let titled = operands.len() > 1 || self.run.has("-R");
        let had_files = !files.is_empty();
        self.show_entries(files);
        let mut first = !had_files;
        let mut queue: Vec<PathBuf> = folders.into_iter().rev().collect();
        queue.sort_by(|a, b| b.cmp(a));
        while let Some(folder) = queue.pop() {
            if !first {
                self.out.push('\n');
            }
            first = false;
            if titled && !self.show.json {
                self.out.push_str(&format!("{}:\n", folder.display()));
            }
            let entries = self.folder(&folder);
            if self.run.has("-R") {
                let mut below: Vec<PathBuf> = entries
                    .iter()
                    .filter(|e| e.kind == Type::Folder && e.name != "." && e.name != "..")
                    .map(|e| folder.join(&e.name))
                    .collect();
                below.reverse();
                queue.extend(below);
            }
            self.show_entries(entries);
        }
    }

    /// The entries of `folder` to show, in order.
    fn folder(&mut self, folder: &Path) -> Vec<Entry> {
        let listing = match fs::read_dir(folder) {
            Ok(listing) => listing,
            Err(e) => {
                self.fail(&format!("cannot open folder {}: {e}", folder.display()));
                return Vec::new();
            }
        };
        let mut entries = Vec::new();
        if self.hidden == Hidden::All {
            for name in [".", ".."] {
                entries.extend(self.entry(&folder.join(name), name, true));
            }
        }
        for item in listing {
            let item = match item {
                Ok(item) => item,
                Err(e) => {
                    self.fail(&format!("reading {}: {e}", folder.display()));
                    continue;
                }
            };
            let name = item.file_name().to_string_lossy().into_owned();
            if self.hidden.shows(&name) {
                entries.extend(self.entry(&item.path(), &name, false));
            }
        }
        listing::order(&mut entries, self.run);
        entries
    }

    /// The entry at `path`, shown as `name`, following a link when
    /// `follow`.
    fn entry(&mut self, path: &Path, name: &str, follow: bool) -> Option<Entry> {
        let metadata = match follow {
            true => fs::metadata(path).or_else(|_| fs::symlink_metadata(path)),
            false => fs::symlink_metadata(path),
        };
        let metadata = match metadata {
            Ok(m) => m,
            Err(e) => {
                self.fail(&format!("cannot access {}: {e}", path.display()));
                return None;
            }
        };
        let kind = kind(&metadata);
        let target = match kind {
            Type::Link => fs::read_link(path).ok().map(|t| t.display().to_string()),
            _ => None,
        };
        let (access, acl) = match self.details {
            true => details(path, kind != Type::Link),
            false => (None, false),
        };
        Some(Entry {
            name: name.into(),
            kind,
            size: metadata.len(),
            modified: metadata.mtime(),
            inode: metadata.ino(),
            access,
            acl,
            target,
        })
    }

    fn show_entries(&mut self, entries: Vec<Entry>) {
        if self.show.json {
            self.all.extend(entries);
        } else if self.show.long {
            self.out
                .push_str(&listing::long(&entries, self.show, self.paint));
        } else {
            let text = listing::names(&entries, self.show, self.paint, self.columns);
            self.out.push_str(&text);
        }
    }

    fn fail(&mut self, message: &str) {
        complain(&COMMAND, self.run, message);
        self.failed = true;
    }
}

fn kind(metadata: &Metadata) -> Type {
    let t = metadata.file_type();
    if t.is_symlink() {
        Type::Link
    } else if t.is_dir() {
        Type::Folder
    } else if t.is_socket() {
        Type::Socket
    } else {
        Type::File
    }
}

/// What the caller may do with the entry at `path`, as `rwx`, and
/// whether it has an extended list; unknown where its node does not
/// open or answer.
fn details(path: &Path, follow: bool) -> (Option<String>, bool) {
    let Ok(node) = node::open(path, follow) else {
        return (None, false);
    };
    let access = node::access(&node).ok().map(node::rwx);
    let acl = node::acl(&node, false).is_ok_and(|entries| node::extended(&entries));
    (access, acl)
}
