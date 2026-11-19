//! Extracting only into the target folder (MIKSPEC-0098.R60) and within
//! the space of its volume (MIKSPEC-0098.R61).
//!
//! An entry's name is split into parts, and refused when it is absolute
//! or has a `..` part. The parts are then resolved from the folder, one
//! at a time: a link that is there, whether it was there before or an
//! earlier entry made it, is read and followed only while what it leads
//! to stays in the folder, and refused otherwise. What is left has no
//! links in the parts that exist, so that creating the entry there
//! writes nothing outside the folder. The last part is not followed,
//! apart from folders: a file replaces what is there, a link included,
//! by removing it first and then creating the file anew, which never
//! opens a link.

use std::collections::VecDeque;
use std::ffi::{CString, c_char, c_int};
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

/// How many links one name may pass through.
const LINKS_MAX: usize = 40;
/// The C library's flag that opens a link itself, the same on Mikros
/// and on Linux.
const O_NOFOLLOW: i32 = 0o400000;
const AT_FDCWD: c_int = -100;
const AT_SYMLINK_NOFOLLOW: c_int = 0x100;
/// The C library's `UTIME_OMIT`, which leaves a time as it is.
const UTIME_OMIT: i64 = 0x3fff_fffe;

/// The C library's `struct timespec`.
#[repr(C)]
struct Timespec {
    sec: i64,
    nsec: i64,
}

unsafe extern "C" {
    fn utimensat(dirfd: c_int, path: *const c_char, times: *const Timespec, flags: c_int) -> c_int;
}

/// The parts of an entry's name, without empty and `.` parts; refused,
/// with the reason, when the name is absolute or has a `..` part.
pub fn parts(name: &str) -> Result<Vec<String>, String> {
    if name.starts_with('/') {
        return Err("its name is absolute".into());
    }
    let mut parts = Vec::new();
    for part in name.split('/') {
        match part {
            "" | "." => {}
            ".." => return Err("its name has a .. part".into()),
            part => parts.push(part.to_string()),
        }
    }
    Ok(parts)
}

/// `parts` as a name, for messages.
pub fn shown(parts: &[String]) -> String {
    match parts.is_empty() {
        true => ".".into(),
        false => parts.join("/"),
    }
}

/// The space of a volume, from `QueryFilesystem`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Volume {
    /// How messages name it.
    pub name: String,
    pub total: u64,
    pub free: u64,
}

/// What tells the space of the volume that holds a folder.
pub trait Space {
    fn volume(&mut self, folder: &Path) -> Result<Volume, String>;
}

/// Whether writing `size` bytes of `entry` on `volume` leaves at least
/// 5% of its size free, and otherwise the message that names the entry,
/// both sizes and the volume. A volume that gives no size, as one in
/// memory does, has no part to keep free.
pub fn fits(entry: &str, size: u64, volume: &Volume) -> Result<(), String> {
    if volume.total == 0 {
        return Ok(());
    }
    let enough = volume
        .free
        .checked_sub(size)
        .is_some_and(|left| u128::from(left) * 20 >= u128::from(volume.total));
    match enough {
        true => Ok(()),
        false => Err(format!(
            "{entry}: its {size} bytes would leave less than 5% of volume {} free, \
             which has {} of {} bytes free",
            volume.name, volume.free, volume.total
        )),
    }
}

/// The folder that entries are extracted into.
pub struct Target {
    root: PathBuf,
    /// The parts of its absolute path with every link resolved, against
    /// which absolute link targets are checked; none when it is not
    /// known, so that every absolute target leads out.
    real: Option<Vec<String>>,
}

impl Target {
    /// The target `root`, a folder that is there.
    pub fn new(root: &Path) -> io::Result<Target> {
        if !fs::metadata(root)?.is_dir() {
            return Err(io::Error::from(io::ErrorKind::NotADirectory));
        }
        let real = fs::canonicalize(root).ok().map(|real| {
            let text = real.as_os_str().to_string_lossy().into_owned();
            text.split('/')
                .filter(|p| !p.is_empty() && *p != ".")
                .map(str::to_string)
                .collect()
        });
        Ok(Target {
            root: root.to_path_buf(),
            real,
        })
    }

    /// The path of `parts` in the folder.
    pub fn path(&self, parts: &[String]) -> PathBuf {
        let mut path = self.root.clone();
        path.extend(parts);
        path
    }

    /// `parts`, the parts of an entry's name, with every link that is
    /// there in them resolved, the last only when `follow`; refused,
    /// with the reason, when a link leads out of the folder.
    pub fn resolve(&self, parts: &[String], follow: bool) -> Result<Vec<String>, String> {
        let mut at: Vec<String> = Vec::new();
        let mut pending: VecDeque<String> = parts.iter().cloned().collect();
        // How many parts at the front of `pending` come from links.
        let mut from_links = 0;
        let mut hops = 0;
        let mut missing = false;
        // The last link followed.
        let mut through = String::new();
        let out = |link: &str| {
            format!("its path passes through {link}, a link that leads out of the folder")
        };
        while let Some(part) = pending.pop_front() {
            let linked = from_links > 0;
            from_links -= usize::from(linked);
            let last = !linked && pending.is_empty();
            match part.as_str() {
                "" | "." => continue,
                ".." => {
                    // Only the targets of links have `..` parts.
                    if at.pop().is_none() {
                        return Err(out(&through));
                    }
                    continue;
                }
                _ => at.push(part),
            }
            if missing || (last && !follow) {
                continue;
            }
            let path = self.path(&at);
            let metadata = match fs::symlink_metadata(&path) {
                Ok(metadata) => metadata,
                Err(e) if e.kind() == io::ErrorKind::NotFound => {
                    missing = true;
                    continue;
                }
                Err(e) => return Err(format!("cannot look at {}: {e}", shown(&at))),
            };
            if !metadata.file_type().is_symlink() {
                continue;
            }
            hops += 1;
            if hops > LINKS_MAX {
                return Err(format!(
                    "its path passes through too many links at {}",
                    shown(&at)
                ));
            }
            through = shown(&at);
            let target = fs::read_link(&path).map_err(|e| format!("cannot read {through}: {e}"))?;
            let target = target.as_os_str().to_string_lossy().into_owned();
            at.pop();
            let mut next: Vec<String> = target.split('/').map(str::to_string).collect();
            if target.starts_with('/') {
                let Some(real) = &self.real else {
                    return Err(out(&through));
                };
                next.retain(|p| !p.is_empty() && p != ".");
                if next.len() < real.len() || next[..real.len()] != real[..] {
                    return Err(out(&through));
                }
                next.drain(..real.len());
                at.clear();
            }
            from_links += next.len();
            for part in next.into_iter().rev() {
                pending.push_front(part);
            }
        }
        Ok(at)
    }

    /// The type of what is at `parts`, a link itself and not what it
    /// leads to, if anything is.
    pub fn what(&self, parts: &[String]) -> io::Result<Option<fs::FileType>> {
        match fs::symlink_metadata(self.path(parts)) {
            Ok(metadata) => Ok(Some(metadata.file_type())),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e),
        }
    }

    /// Checks the space of the volume that receives `size` bytes at
    /// `parts`: that of the innermost folder of their path that is
    /// there.
    pub fn room(
        &self,
        space: &mut dyn Space,
        parts: &[String],
        entry: &str,
        size: u64,
    ) -> Result<(), String> {
        let mut depth = parts.len().saturating_sub(1);
        while depth > 0
            && !self
                .what(&parts[..depth])
                .is_ok_and(|t| t.is_some_and(|t| t.is_dir()))
        {
            depth -= 1;
        }
        let volume = space.volume(&self.path(&parts[..depth]))?;
        fits(entry, size, &volume)
    }

    /// Creates the folders that lead to `parts`, which `resolve` gave.
    pub fn parents(&self, parts: &[String]) -> io::Result<()> {
        match parts.len() {
            0 | 1 => Ok(()),
            n => fs::create_dir_all(self.path(&parts[..n - 1])),
        }
    }

    /// Removes what is at `parts` to make room for an entry, unless it is
    /// a folder; a link is removed itself.
    pub fn clear(&self, parts: &[String]) -> Result<(), String> {
        match self.what(parts) {
            Ok(None) => Ok(()),
            Ok(Some(t)) if t.is_dir() => Err("a folder is in its place".into()),
            Ok(Some(_)) => fs::remove_file(self.path(parts))
                .map_err(|e| format!("cannot replace what is in its place: {e}")),
            Err(e) => Err(format!("cannot look at it: {e}")),
        }
    }

    /// A new file at `parts`, where nothing is; never through a link.
    pub fn create(&self, parts: &[String]) -> io::Result<File> {
        OpenOptions::new()
            .write(true)
            .create_new(true)
            .custom_flags(O_NOFOLLOW)
            .open(self.path(parts))
    }

    /// A folder at `parts`, which `resolve` gave following the last part,
    /// unless one is there.
    pub fn folder(&self, parts: &[String]) -> Result<(), String> {
        match self.what(parts) {
            Ok(Some(t)) if t.is_dir() => Ok(()),
            Ok(Some(_)) => Err("something that is not a folder is in its place".into()),
            Ok(None) => fs::create_dir_all(self.path(parts)).map_err(|e| e.to_string()),
            Err(e) => Err(e.to_string()),
        }
    }

    /// A link at `parts`, where nothing is, to `target`, which may lead
    /// anywhere: it is never followed.
    pub fn link(&self, parts: &[String], target: &str) -> io::Result<()> {
        std::os::unix::fs::symlink(target, self.path(parts))
    }

    /// A hard link at `parts`, where nothing is, to the file at `to`,
    /// which `resolve` gave following its last part.
    pub fn hard_link(&self, parts: &[String], to: &[String]) -> io::Result<()> {
        fs::hard_link(self.path(to), self.path(parts))
    }

    /// Gives what is at `parts` the stored mode, unless it is a link.
    pub fn mode(&self, parts: &[String], mode: u32) -> io::Result<()> {
        match self.what(parts)? {
            Some(t) if !t.is_symlink() => {
                fs::set_permissions(self.path(parts), fs::Permissions::from_mode(mode & 0o7777))
            }
            _ => Ok(()),
        }
    }

    /// Gives what is at `parts`, a link itself, the modified time
    /// `seconds` since 1970.
    pub fn modified(&self, parts: &[String], seconds: i64) -> io::Result<()> {
        let path = CString::new(self.path(parts).as_os_str().as_bytes())
            .map_err(|_| io::Error::from(io::ErrorKind::InvalidInput))?;
        let times = [
            Timespec {
                sec: 0,
                nsec: UTIME_OMIT,
            },
            Timespec {
                sec: seconds,
                nsec: 0,
            },
        ];
        // SAFETY: the path is NUL-terminated and the times are two
        // timespecs.
        let done =
            unsafe { utimensat(AT_FDCWD, path.as_ptr(), times.as_ptr(), AT_SYMLINK_NOFOLLOW) };
        match done {
            0 => Ok(()),
            _ => Err(io::Error::last_os_error()),
        }
    }

    /// Removes the file at `parts`, an entry that failed.
    pub fn remove(&self, parts: &[String]) {
        let _ = fs::remove_file(self.path(parts));
    }
}

/// Copies the data of an entry from `data` to `out`, no more than the
/// `size` bytes that the archive gives for it; fails when the data is
/// shorter or longer.
pub fn copy(data: &mut dyn Read, out: &mut dyn Write, size: u64) -> Result<(), String> {
    let mut buffer = vec![0; 64 * 1024];
    let mut left = size;
    while left > 0 {
        let want = usize::try_from(left).map_or(buffer.len(), |l| l.min(buffer.len()));
        let got = match data.read(&mut buffer[..want]) {
            Ok(0) => {
                return Err(format!(
                    "its data is shorter than the {size} bytes that the archive gives"
                ));
            }
            Ok(got) => got,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(format!("cannot read its data: {e}")),
        };
        out.write_all(&buffer[..got])
            .map_err(|e| format!("cannot write it: {e}"))?;
        left -= got as u64;
    }
    loop {
        match data.read(&mut buffer[..1]) {
            Ok(0) => return Ok(()),
            Ok(_) => {
                return Err(format!(
                    "its data is longer than the {size} bytes that the archive gives"
                ));
            }
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => return Err(format!("cannot read its data: {e}")),
        }
    }
}

/// What the tests of the commands that extract use: scratch folders and
/// volumes of a fixed size.
#[cfg(any(test, feature = "testing"))]
pub mod testing {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// A new, empty folder for one test, removed when it is dropped.
    pub struct Scratch(pub PathBuf);

    impl Scratch {
        pub fn new(name: &str) -> Scratch {
            static COUNT: AtomicUsize = AtomicUsize::new(0);
            let n = COUNT.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "mikros-{}-{name}-{}-{n}",
                env!("CARGO_PKG_NAME"),
                std::process::id()
            ));
            let _ = fs::remove_dir_all(&path);
            fs::create_dir_all(&path).unwrap();
            Scratch(path)
        }

        pub fn join(&self, path: &str) -> PathBuf {
            self.0.join(path)
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    /// A volume of `total` bytes with `free` of them free.
    pub struct Fixed(pub u64, pub u64);

    impl Space for Fixed {
        fn volume(&mut self, _: &Path) -> Result<Volume, String> {
            Ok(Volume {
                name: "test".into(),
                total: self.0,
                free: self.1,
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::testing::*;
    use super::*;
    use std::os::unix::fs::symlink;

    fn p(parts: &[&str]) -> Vec<String> {
        parts.iter().map(|p| p.to_string()).collect()
    }

    #[test]
    fn refuses_absolute_names_and_parent_parts() {
        assert_eq!(parts("a/./b//c/").unwrap(), p(&["a", "b", "c"]));
        assert_eq!(parts("./").unwrap(), p(&[]));
        assert!(parts("/etc/passwd").is_err());
        assert!(parts("//x").is_err());
        assert!(parts("../x").is_err());
        assert!(parts("a/../../x").is_err());
        assert!(parts("a/..").is_err());
        assert_eq!(parts("a/..b/c..").unwrap(), p(&["a", "..b", "c.."]));
    }

    #[test]
    fn follows_links_that_stay_inside() {
        let s = Scratch::new("inside");
        let root = s.join("root");
        fs::create_dir_all(root.join("sub/deep")).unwrap();
        symlink("sub", root.join("l")).unwrap();
        symlink("../sub/deep", root.join("sub/up")).unwrap();
        symlink("l", root.join("chain")).unwrap();
        symlink(s.join("root/sub"), root.join("abs")).unwrap();
        let t = Target::new(&root).unwrap();
        assert_eq!(t.resolve(&p(&["l", "f"]), false).unwrap(), p(&["sub", "f"]));
        assert_eq!(
            t.resolve(&p(&["chain", "up", "f"]), false).unwrap(),
            p(&["sub", "deep", "f"])
        );
        assert_eq!(
            t.resolve(&p(&["abs", "f"]), false).unwrap(),
            p(&["sub", "f"])
        );
        // The last part is not followed unless asked.
        assert_eq!(t.resolve(&p(&["l"]), false).unwrap(), p(&["l"]));
        assert_eq!(t.resolve(&p(&["l"]), true).unwrap(), p(&["sub"]));
        // Parts that are not there yet stay as they are.
        assert_eq!(
            t.resolve(&p(&["new", "x", "y"]), true).unwrap(),
            p(&["new", "x", "y"])
        );
    }

    #[test]
    fn refuses_links_that_lead_out() {
        let s = Scratch::new("out");
        let root = s.join("root");
        fs::create_dir_all(root.join("sub")).unwrap();
        fs::create_dir_all(s.join("outside")).unwrap();
        symlink("..", root.join("up")).unwrap();
        symlink("../outside", root.join("rel")).unwrap();
        symlink(s.join("outside"), root.join("abs")).unwrap();
        symlink("/", root.join("slash")).unwrap();
        symlink("../../outside", root.join("sub/deeper")).unwrap();
        symlink("sub/../..", root.join("sneaky")).unwrap();
        symlink("up", root.join("via")).unwrap();
        symlink("loop", root.join("loop")).unwrap();
        let t = Target::new(&root).unwrap();
        for name in ["up", "rel", "abs", "slash", "sneaky", "via"] {
            let why = t.resolve(&p(&[name, "f"]), false).unwrap_err();
            assert!(why.contains("leads out"), "{name}: {why}");
            assert!(t.resolve(&p(&[name]), true).is_err(), "{name}");
        }
        assert!(t.resolve(&p(&["sub", "deeper", "f"]), false).is_err());
        assert!(
            t.resolve(&p(&["loop", "f"]), false)
                .unwrap_err()
                .contains("too many")
        );
        // A link that leads out is fine as the last part, not followed.
        assert_eq!(t.resolve(&p(&["abs"]), false).unwrap(), p(&["abs"]));
    }

    #[test]
    fn replaces_links_without_following_them() {
        let s = Scratch::new("replace");
        let root = s.join("root");
        fs::create_dir_all(&root).unwrap();
        fs::write(s.join("victim"), b"keep").unwrap();
        symlink(s.join("victim"), root.join("f")).unwrap();
        let t = Target::new(&root).unwrap();
        let at = t.resolve(&p(&["f"]), false).unwrap();
        // A file is never opened through the link.
        assert!(t.create(&at).is_err());
        t.clear(&at).unwrap();
        t.create(&at).unwrap().write_all(b"new").unwrap();
        assert_eq!(fs::read(s.join("victim")).unwrap(), b"keep");
        assert_eq!(fs::read(root.join("f")).unwrap(), b"new");
        fs::create_dir(root.join("d")).unwrap();
        assert!(t.clear(&p(&["d"])).is_err());
    }

    #[test]
    fn sets_modes_and_times_but_not_through_links() {
        let s = Scratch::new("attributes");
        let root = s.join("root");
        fs::create_dir_all(&root).unwrap();
        fs::write(s.join("victim"), b"").unwrap();
        fs::set_permissions(s.join("victim"), fs::Permissions::from_mode(0o600)).unwrap();
        symlink(s.join("victim"), root.join("l")).unwrap();
        fs::write(root.join("f"), b"").unwrap();
        let t = Target::new(&root).unwrap();
        t.mode(&p(&["f"]), 0o751).unwrap();
        t.modified(&p(&["f"]), 1_000_000).unwrap();
        t.mode(&p(&["l"]), 0o777).unwrap();
        t.modified(&p(&["l"]), 2_000_000).unwrap();
        use std::os::unix::fs::MetadataExt;
        let f = fs::metadata(root.join("f")).unwrap();
        assert_eq!((f.mode() & 0o7777, f.mtime()), (0o751, 1_000_000));
        let victim = fs::metadata(s.join("victim")).unwrap();
        assert_eq!(victim.mode() & 0o7777, 0o600);
        assert_ne!(victim.mtime(), 2_000_000);
        assert_eq!(
            fs::symlink_metadata(root.join("l")).unwrap().mtime(),
            2_000_000
        );
    }

    #[test]
    fn keeps_five_percent_free() {
        let volume = |total, free| Volume {
            name: "disk".into(),
            total,
            free,
        };
        assert!(fits("a", 50, &volume(1000, 100)).is_ok());
        let why = fits("a", 51, &volume(1000, 100)).unwrap_err();
        assert!(
            why.starts_with("a: its 51 bytes") && why.contains("volume disk"),
            "{why}"
        );
        assert!(why.contains("100 of 1000 bytes free"), "{why}");
        assert!(fits("a", 200, &volume(1000, 100)).is_err());
        assert!(fits("a", 0, &volume(1000, 10)).is_err());
        assert!(fits("a", u64::MAX, &volume(u64::MAX, u64::MAX)).is_err());
        assert!(fits("a", 1 << 40, &volume(0, 0)).is_ok());
    }

    #[test]
    fn checks_the_volume_of_the_innermost_folder_there() {
        struct Asked(Vec<PathBuf>);
        impl Space for Asked {
            fn volume(&mut self, folder: &Path) -> Result<Volume, String> {
                self.0.push(folder.to_path_buf());
                Ok(Volume {
                    name: "v".into(),
                    total: 100,
                    free: 10,
                })
            }
        }
        let s = Scratch::new("room");
        fs::create_dir_all(s.join("a/b")).unwrap();
        let t = Target::new(&s.0).unwrap();
        let mut asked = Asked(Vec::new());
        assert!(
            t.room(&mut asked, &p(&["a", "b", "c", "d"]), "e", 5)
                .is_ok()
        );
        assert!(t.room(&mut asked, &p(&["f"]), "f", 6).is_err());
        assert_eq!(asked.0, [s.join("a/b"), s.0.clone()]);
    }

    #[test]
    fn copies_no_more_and_no_less_than_the_size() {
        let mut out = Vec::new();
        copy(&mut &b"hello"[..], &mut out, 5).unwrap();
        assert_eq!(out, b"hello");
        let mut out = Vec::new();
        let why = copy(&mut &b"hello!"[..], &mut out, 5).unwrap_err();
        assert!(why.contains("longer"), "{why}");
        assert_eq!(out, b"hello");
        let why = copy(&mut &b"hell"[..], &mut Vec::new(), 5).unwrap_err();
        assert!(why.contains("shorter"), "{why}");
    }
}
