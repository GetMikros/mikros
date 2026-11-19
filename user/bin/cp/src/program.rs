//! cp on Mikros: it copies contents, folders and links through the
//! standard library, shares contents with `CloneRange` and keeps access
//! control lists with `GetAcl` and `SetAcl` on the node connections
//! (MIKSPEC-0098.R33).

use crate::copying::{self, COMMAND, Clobber, Make, Options, Reflink, Refused};
use mikros_cli::{FAILURE, INVALID, Paint, Run, SUCCESS, complain, start};
use mikros_command::node;
use std::ffi::{CString, c_char, c_int};
use std::fs::{self, File, Metadata, OpenOptions};
use std::io::{self, Write};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

const FOREVER: i64 = i64::MAX;
const AT_FDCWD: c_int = -100;
const AT_SYMLINK_NOFOLLOW: c_int = 0x100;

/// The C library's `struct timespec`.
#[repr(C)]
struct Timespec {
    sec: i64,
    nsec: i64,
}

unsafe extern "C" {
    fn utimensat(dirfd: c_int, path: *const c_char, times: *const Timespec, flags: c_int) -> c_int;
}

struct Copier<'a> {
    run: &'a Run,
    options: Options,
    paint: Paint,
    failed: bool,
}

pub fn main() -> ExitCode {
    let run = start(&COMMAND, &mikros_command::list);
    if let Some(why) = copying::conflict(&run) {
        complain(&COMMAND, &run, why);
        return ExitCode::from(INVALID);
    }
    let is_folder = |p: &Path| fs::metadata(p).is_ok_and(|m| m.is_dir());
    let plan = copying::plan(&run.operands, run.value("-t"), run.has("-T"), &is_folder);
    let pairs = match plan {
        Ok(pairs) => pairs,
        Err(Refused::Invalid(why)) => {
            complain(&COMMAND, &run, &why);
            return ExitCode::from(INVALID);
        }
        Err(Refused::Failed(why)) => {
            complain(&COMMAND, &run, &why);
            return ExitCode::from(FAILURE);
        }
    };
    let mut copier = Copier {
        run: &run,
        options: Options::of(&run),
        paint: Paint::stdout(&run),
        failed: false,
    };
    for pair in pairs {
        match pair {
            Ok((source, destination)) => copier.top(&source, &destination),
            Err(why) => copier.fail(&why),
        }
    }
    match copier.failed {
        true => ExitCode::from(FAILURE),
        false => ExitCode::from(SUCCESS),
    }
}

impl Copier<'_> {
    fn fail(&mut self, message: &str) {
        complain(&COMMAND, self.run, message);
        self.failed = true;
    }

    /// Copies an operand, refusing a folder into itself.
    fn top(&mut self, source: &Path, destination: &Path) {
        let metadata = match self.options.follow {
            true => fs::metadata(source),
            false => fs::symlink_metadata(source),
        };
        let metadata = match metadata {
            Ok(m) => m,
            Err(e) => return self.fail(&format!("cannot access {}: {e}", source.display())),
        };
        if metadata.is_dir()
            && let (Ok(from), Some(to)) = (fs::canonicalize(source), resolved(destination))
            && copying::within(&from, &to)
        {
            return self.fail(&format!(
                "cannot copy the folder {} into itself, {}",
                source.display(),
                destination.display()
            ));
        }
        self.copy(source, destination, &metadata);
    }

    /// Copies `source`, of `metadata`, to `destination`.
    fn copy(&mut self, source: &Path, destination: &Path, metadata: &Metadata) {
        let there = fs::symlink_metadata(destination).ok();
        if metadata.is_dir() {
            return self.folder(source, destination, metadata, there.as_ref());
        }
        if let Some(there) = &there {
            if fs::metadata(destination)
                .is_ok_and(|d| d.dev() == metadata.dev() && d.ino() == metadata.ino())
            {
                let (s, d) = (source.display(), destination.display());
                return self.fail(&format!("{s} and {d} are the same file"));
            }
            if there.is_dir() {
                let d = destination.display();
                return self.fail(&format!("cannot replace the folder {d} with a file"));
            }
            if self.options.update
                && !copying::newer(
                    (metadata.mtime(), metadata.mtime_nsec()),
                    (there.mtime(), there.mtime_nsec()),
                )
            {
                return;
            }
            match self.options.clobber {
                Clobber::Never => return,
                Clobber::Ask if !ask(&format!("overwrite {}", destination.display())) => return,
                _ => {}
            }
        }
        let kind = metadata.file_type();
        let replace = self.options.force || self.options.clobber == Clobber::Ask;
        let made = match self.options.make {
            // A name in the way of a new link goes only with -f, or once
            // -i asked.
            Make::Hard => self.replacing(there.is_some() && replace, destination, || {
                fs::hard_link(source, destination)
            }),
            Make::Symbolic => {
                let target = std::path::absolute(source).unwrap_or_else(|_| source.into());
                self.replacing(there.is_some() && replace, destination, || {
                    std::os::unix::fs::symlink(&target, destination)
                })
            }
            Make::Copy if kind.is_symlink() => self.link(source, destination, there.is_some()),
            Make::Copy if kind.is_file() => self.file(source, destination, metadata),
            Make::Copy => Err(format!(
                "cannot copy {}: not a file, folder or link",
                source.display()
            )),
        };
        match made {
            Ok(()) => {
                if self.options.preserve && self.options.make == Make::Copy {
                    self.keep(metadata, source, destination);
                }
                self.tell(source, destination);
            }
            Err(why) => self.fail(&why),
        }
    }

    /// Makes a link with `make`, removing what is at `destination`
    /// first when `remove`.
    fn replacing(
        &self,
        remove: bool,
        destination: &Path,
        make: impl Fn() -> io::Result<()>,
    ) -> Result<(), String> {
        let shown = destination.display();
        if remove {
            fs::remove_file(destination).map_err(|e| format!("cannot remove {shown}: {e}"))?;
        }
        make().map_err(|e| format!("cannot make the link {shown}: {e}"))
    }

    /// Copies the link `source` as a link.
    fn link(&self, source: &Path, destination: &Path, there: bool) -> Result<(), String> {
        let target = fs::read_link(source)
            .map_err(|e| format!("cannot read the link {}: {e}", source.display()))?;
        self.replacing(there, destination, || {
            std::os::unix::fs::symlink(&target, destination)
        })
    }

    /// Copies the contents of the file `source`, sharing them where
    /// `--reflink` allows and the file system can.
    fn file(&self, source: &Path, destination: &Path, metadata: &Metadata) -> Result<(), String> {
        let (s, d) = (source.display(), destination.display());
        let mut from = File::open(source).map_err(|e| format!("cannot open {s}: {e}"))?;
        let open = || {
            OpenOptions::new()
                .write(true)
                .create(true)
                .truncate(true)
                .mode(metadata.mode() & 0o777)
                .open(destination)
        };
        let mut to = match open() {
            Err(_) if self.options.force && fs::symlink_metadata(destination).is_ok() => {
                fs::remove_file(destination).map_err(|e| format!("cannot remove {d}: {e}"))?;
                open()
            }
            other => other,
        }
        .map_err(|e| format!("cannot create {d}: {e}"))?;
        let length = metadata.len();
        if self.options.reflink != Reflink::Never && length > 0 {
            let shared = node::node(&to).map_err(|_| "Io".to_string()).and_then(|n| {
                node::clone_range(&n, &from, 0, 0, length).map_err(|e| format!("{e:?}"))
            });
            match shared {
                Ok(()) => return Ok(()),
                Err(e) if self.options.reflink == Reflink::Always => {
                    return Err(format!("cannot share the contents of {s} with {d}: {e}"));
                }
                Err(_) => {}
            }
        }
        io::copy(&mut from, &mut to).map_err(|e| format!("cannot copy {s} to {d}: {e}"))?;
        Ok(())
    }

    /// Copies the folder `source` and what it holds.
    fn folder(
        &mut self,
        source: &Path,
        destination: &Path,
        metadata: &Metadata,
        there: Option<&Metadata>,
    ) {
        let (s, d) = (source.display(), destination.display());
        if !self.options.recursive {
            return self.fail(&format!("cannot copy the folder {s} without -r"));
        }
        match there {
            Some(there) if !there.is_dir() => {
                return self.fail(&format!("cannot replace {d}, not a folder, with a folder"));
            }
            Some(_) => {}
            None => {
                if let Err(e) = fs::create_dir(destination) {
                    return self.fail(&format!("cannot create the folder {d}: {e}"));
                }
                self.tell(source, destination);
            }
        }
        let entries = match fs::read_dir(source) {
            Ok(entries) => entries,
            Err(e) => return self.fail(&format!("cannot open the folder {s}: {e}")),
        };
        let mut names: Vec<_> = Vec::new();
        for entry in entries {
            match entry {
                Ok(entry) => names.push(entry.file_name()),
                Err(e) => self.fail(&format!("cannot read the folder {s}: {e}")),
            }
        }
        names.sort();
        for name in names {
            let from = source.join(&name);
            match fs::symlink_metadata(&from) {
                Ok(m) => self.copy(&from, &destination.join(&name), &m),
                Err(e) => self.fail(&format!("cannot access {}: {e}", from.display())),
            }
        }
        if self.options.preserve {
            self.keep(metadata, source, destination);
        }
    }

    /// Gives `destination` the times, mode and access control lists of
    /// `source`, of `metadata`; a link keeps only its times.
    fn keep(&mut self, metadata: &Metadata, source: &Path, destination: &Path) {
        let d = destination.display();
        let link = metadata.file_type().is_symlink();
        if !link {
            let mode = fs::Permissions::from_mode(metadata.mode() & 0o7777);
            if let Err(e) = fs::set_permissions(destination, mode) {
                self.fail(&format!("cannot keep the mode of {d}: {e}"));
            }
            match acls(source, destination, metadata.is_dir()) {
                Ok(()) => {}
                Err(why) if why == NOT_AUTHORIZED => self.fail(NOT_AUTHORIZED),
                Err(why) => self.fail(&format!(
                    "cannot keep the access control list of {d}: {why}"
                )),
            }
        }
        if let Err(e) = times(destination, metadata, link) {
            self.fail(&format!("cannot keep the times of {d}: {e}"));
        }
    }

    fn tell(&self, source: &Path, destination: &Path) {
        if !self.options.verbose {
            return;
        }
        let (source, destination) = (self.paint.entry(source), self.paint.entry(destination));
        let _ = writeln!(io::stdout(), "{source} -> {destination}");
    }
}

/// `path` absolute with links resolved, also when only its folder is
/// there.
fn resolved(path: &Path) -> Option<PathBuf> {
    if let Ok(path) = fs::canonicalize(path) {
        return Some(path);
    }
    let name = path.file_name()?;
    let folder = match path.parent() {
        Some(p) if !p.as_os_str().is_empty() => p,
        _ => Path::new("."),
    };
    Some(fs::canonicalize(folder).ok()?.join(name))
}

/// Why a list could not be set when its file system's question for an
/// administrator was refused.
const NOT_AUTHORIZED: &str = "not authorized";

/// Copies the access list of `source` to `destination`, and the default
/// list of a folder. A list that the file system does not keep is
/// skipped where the source's says no more than its mode.
fn acls(source: &Path, destination: &Path, folder: bool) -> Result<(), String> {
    let from = node::open(source, true).map_err(|e| e.to_string())?;
    let to = node::open(destination, true).map_err(|e| e.to_string())?;
    for default in [false, true] {
        if default && !folder {
            continue;
        }
        let entries = match node::acl(&from, default) {
            Ok(entries) => entries,
            Err(e) if format!("{e:?}") == "NotSupported" => continue,
            Err(e) => return Err(format!("{e:?}")),
        };
        if default && entries.is_empty() {
            continue;
        }
        let extended = node::extended(&entries);
        match to.set_acl(default, entries, FOREVER) {
            Ok(Ok(_)) => {}
            Ok(Err(e)) if format!("{e:?}") == "NotSupported" && !extended => {}
            // The file system asked for an administrator, who did not
            // allow it (MIKSPEC-0098.R64).
            Ok(Err(e)) if format!("{e:?}") == "AccessDenied" => return Err(NOT_AUTHORIZED.into()),
            Ok(Err(e)) => return Err(format!("{e:?}")),
            Err(e) => return Err(format!("{e:?}")),
        }
    }
    Ok(())
}

/// Gives `path` the access and modified times of `metadata`, to the
/// link itself when `link`.
fn times(path: &Path, metadata: &Metadata, link: bool) -> io::Result<()> {
    let path = CString::new(path.as_os_str().as_bytes())
        .map_err(|_| io::Error::from(io::ErrorKind::InvalidInput))?;
    let times = [
        Timespec {
            sec: metadata.atime(),
            nsec: metadata.atime_nsec(),
        },
        Timespec {
            sec: metadata.mtime(),
            nsec: metadata.mtime_nsec(),
        },
    ];
    let flags = if link { AT_SYMLINK_NOFOLLOW } else { 0 };
    // SAFETY: the path is NUL-terminated and the times are two timespecs.
    match unsafe { utimensat(AT_FDCWD, path.as_ptr(), times.as_ptr(), flags) } {
        0 => Ok(()),
        _ => Err(io::Error::last_os_error()),
    }
}

/// Asks `question` on standard error and reads the answer.
fn ask(question: &str) -> bool {
    eprint!("cp: {question}? ");
    let mut answer = String::new();
    io::stdin().read_line(&mut answer).is_ok() && copying::yes(&answer)
}
