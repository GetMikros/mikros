//! mv on Mikros: it renames through the standard library, and across
//! file systems copies contents, folders and links, keeping their times
//! and mode and, with `GetAcl` and `SetAcl` on the node connections,
//! their access control lists (MIKSPEC-0098.R33), before it removes the
//! source.

use crate::moving::{self, COMMAND, Clobber, Options, Refused};
use mikros_cli::{FAILURE, INVALID, Paint, Role, SUCCESS, complain, start};
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

pub fn main() -> ExitCode {
    let run = start(&COMMAND, &mikros_command::list);
    if let Some(why) = moving::conflict(&run) {
        complain(&COMMAND, &run, why);
        return ExitCode::from(INVALID);
    }
    let is_folder = |p: &Path| fs::metadata(p).is_ok_and(|m| m.is_dir());
    let pairs = match moving::plan(&run.operands, run.value("-t"), run.has("-T"), &is_folder) {
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
    let options = Options::of(&run);
    let paint = Paint::stdout(&run);
    let mut failed = false;
    for pair in pairs {
        let moved = pair.and_then(|(source, destination)| {
            let done = shift(options, &source, &destination)?;
            if done && options.verbose {
                let moved = Role::Path(paint.entry_type(&destination));
                let p = |path: &Path| paint.paint(moved, &path.display().to_string());
                let _ = writeln!(
                    io::stdout(),
                    "renamed {} -> {}",
                    p(&source),
                    p(&destination)
                );
            }
            Ok(())
        });
        if let Err(why) = moved {
            complain(&COMMAND, &run, &why);
            failed = true;
        }
    }
    match failed {
        true => ExitCode::from(FAILURE),
        false => ExitCode::from(SUCCESS),
    }
}

/// Moves `source` to `destination`, unless the options leave what is
/// there; whether it moved.
fn shift(options: Options, source: &Path, destination: &Path) -> Result<bool, String> {
    let (s, d) = (source.display(), destination.display());
    let metadata = fs::symlink_metadata(source).map_err(|e| format!("cannot access {s}: {e}"))?;
    if let Ok(there) = fs::symlink_metadata(destination) {
        if there.dev() == metadata.dev() && there.ino() == metadata.ino() {
            return Err(format!("{s} and {d} are the same file"));
        }
        if options.update
            && !there.is_dir()
            && !moving::newer(
                (metadata.mtime(), metadata.mtime_nsec()),
                (there.mtime(), there.mtime_nsec()),
            )
        {
            return Ok(false);
        }
        match options.clobber {
            Clobber::Never => return Ok(false),
            Clobber::Ask if !ask(&format!("overwrite {d}")) => return Ok(false),
            _ => {}
        }
    }
    if metadata.is_dir()
        && let (Ok(from), Some(to)) = (fs::canonicalize(source), resolved(destination))
        && moving::within(&from, &to)
    {
        return Err(format!("cannot move the folder {s} into itself, {d}"));
    }
    match fs::rename(source, destination) {
        Ok(()) => return Ok(true),
        Err(e) if !moving::across(&e) => return Err(format!("cannot move {s} to {d}: {e}")),
        Err(_) => {}
    }
    // Across file systems: what is in the way goes first, as a rename
    // would replace it, then the copy, then the source.
    if let Ok(there) = fs::symlink_metadata(destination) {
        let removed = match there.is_dir() {
            true => fs::remove_dir(destination),
            false => fs::remove_file(destination),
        };
        removed.map_err(|e| format!("cannot replace {d}: {e}"))?;
    }
    copy(source, destination, &metadata)?;
    let removed = match metadata.is_dir() {
        true => fs::remove_dir_all(source),
        false => fs::remove_file(source),
    };
    removed.map_err(|e| format!("cannot remove {s} after copying it: {e}"))?;
    Ok(true)
}

/// Copies `source`, of `metadata`, to `destination`, links as links,
/// with its times, mode and access control lists.
fn copy(source: &Path, destination: &Path, metadata: &Metadata) -> Result<(), String> {
    let (s, d) = (source.display(), destination.display());
    let kind = metadata.file_type();
    if kind.is_symlink() {
        let target = fs::read_link(source).map_err(|e| format!("cannot read the link {s}: {e}"))?;
        std::os::unix::fs::symlink(&target, destination)
            .map_err(|e| format!("cannot make the link {d}: {e}"))?;
    } else if kind.is_dir() {
        fs::create_dir(destination).map_err(|e| format!("cannot create the folder {d}: {e}"))?;
        let entries =
            fs::read_dir(source).map_err(|e| format!("cannot open the folder {s}: {e}"))?;
        for entry in entries {
            let entry = entry.map_err(|e| format!("cannot read the folder {s}: {e}"))?;
            let from = entry.path();
            let m = fs::symlink_metadata(&from)
                .map_err(|e| format!("cannot access {}: {e}", from.display()))?;
            copy(&from, &destination.join(entry.file_name()), &m)?;
        }
    } else if kind.is_file() {
        let mut from = File::open(source).map_err(|e| format!("cannot open {s}: {e}"))?;
        let mut to = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(metadata.mode() & 0o777)
            .open(destination)
            .map_err(|e| format!("cannot create {d}: {e}"))?;
        io::copy(&mut from, &mut to).map_err(|e| format!("cannot copy {s} to {d}: {e}"))?;
    } else {
        return Err(format!("cannot copy {s}: not a file, folder or link"));
    }
    if !kind.is_symlink() {
        fs::set_permissions(
            destination,
            fs::Permissions::from_mode(metadata.mode() & 0o7777),
        )
        .map_err(|e| format!("cannot keep the mode of {d}: {e}"))?;
        acls(source, destination, kind.is_dir()).map_err(|e| match e.as_str() {
            NOT_AUTHORIZED => e,
            _ => format!("cannot keep the access control list of {d}: {e}"),
        })?;
    }
    times(destination, metadata, kind.is_symlink())
        .map_err(|e| format!("cannot keep the times of {d}: {e}"))
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
    eprint!("mv: {question}? ");
    let mut answer = String::new();
    io::stdin().read_line(&mut answer).is_ok() && moving::yes(&answer)
}
