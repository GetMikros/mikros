//! Choosing the system version to boot from `@system`.

use crate::btrfs::{self, Kind, Node, Volume};
use crate::cmdline;
use crate::disk::Disk;
use alloc::string::{String, ToString};
use core::fmt;

/// Why no version could be chosen.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Error {
    Fs(btrfs::Error),
    /// The top-level subvolume has no `@system` directory.
    NoSystem,
    /// `@system` has no link of this name.
    NoLink(&'static str),
    /// The link of this name is not a symbolic link.
    NotALink(&'static str),
    /// The link of this name has a target that names no version.
    BadTarget(&'static str, String),
    /// `@system` holds no version of this name.
    NoVersion(String),
}

impl From<btrfs::Error> for Error {
    fn from(e: btrfs::Error) -> Error {
        Error::Fs(e)
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Fs(e) => write!(f, "{e}"),
            Error::NoSystem => write!(f, "the system partition has no @system"),
            Error::NoLink(name) => write!(f, "@system/{name} does not exist"),
            Error::NotALink(name) => write!(f, "@system/{name} is not a symbolic link"),
            Error::BadTarget(name, target) => {
                write!(f, "@system/{name} leads to {target:?}, which is no version")
            }
            Error::NoVersion(name) => write!(f, "@system has no version {name:?}"),
        }
    }
}

/// The version that the command line asks for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Request<'a> {
    Current,
    Previous,
    Named(&'a str),
}

/// The version that the last `system=` option of `cmdline` asks for.
pub fn request(cmdline: &str) -> Request<'_> {
    match cmdline::option(cmdline, "system") {
        None | Some("current") => Request::Current,
        Some("previous") => Request::Previous,
        Some(name) => Request::Named(name),
    }
}

/// Whether `name` may be the name of a version.
pub fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name != "current"
        && name != "previous"
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b".-_".contains(&b))
}

/// A version: its name and the root directory of its subvolume.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Version {
    pub name: String,
    pub root: Node,
}

struct System<'v, D> {
    volume: &'v mut Volume<D>,
    dir: Node,
}

impl<D: Disk> System<'_, D> {
    /// The version `name`: a subvolume of that name directly in `@system`.
    fn version(&mut self, name: &str) -> Result<Option<Version>, Error> {
        if !valid_name(name) {
            return Ok(None);
        }
        let Some(root) = self.volume.lookup(self.dir, name.as_bytes())? else {
            return Ok(None);
        };
        let subvolume = root.subvolume != self.dir.subvolume
            && self.volume.is_subvolume_root(root)?
            && self.volume.inode(root)?.kind == Kind::Directory;
        Ok(subvolume.then(|| Version {
            name: name.to_string(),
            root,
        }))
    }

    /// The version that the link `name` in `@system` names.
    fn link(&mut self, name: &'static str) -> Result<Version, Error> {
        let node = self
            .volume
            .lookup(self.dir, name.as_bytes())?
            .ok_or(Error::NoLink(name))?;
        if self.volume.inode(node)?.kind != Kind::Symlink {
            return Err(Error::NotALink(name));
        }
        let target = self.volume.read_link(node)?;
        let target = String::from_utf8_lossy(&target).into_owned();
        self.version(&target)?.ok_or(Error::BadTarget(name, target))
    }
}

/// The version of the system partition `volume` that `cmdline` asks for.
/// When that is the one `current` names, `count` gives the name of the
/// version to boot instead, from the names of the current version and of
/// the one that `previous` names, if that is a version.
pub fn choose<D: Disk>(
    volume: &mut Volume<D>,
    cmdline: &str,
    count: impl FnOnce(&str, Option<&str>) -> String,
) -> Result<Version, Error> {
    let top = volume.top()?;
    let dir = volume.lookup(top, b"@system")?.ok_or(Error::NoSystem)?;
    if volume.inode(dir)?.kind != Kind::Directory {
        return Err(Error::NoSystem);
    }
    let mut system = System { volume, dir };
    match request(cmdline) {
        Request::Named(name) => system
            .version(name)?
            .ok_or_else(|| Error::NoVersion(name.to_string())),
        Request::Previous => system.link("previous"),
        Request::Current => {
            let current = system.link("current")?;
            let previous = system.link("previous").ok();
            let name = count(&current.name, previous.as_ref().map(|v| v.name.as_str()));
            if name == current.name {
                return Ok(current);
            }
            system.version(&name)?.ok_or(Error::NoVersion(name))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::btrfs::tests::{mkfs, write};
    use std::os::unix::fs::symlink;
    use std::path::Path;

    const VERSIONS: [&str; 8] = [
        "--subvol",
        "@system",
        "--subvol",
        "ro:@system/1.0",
        "--subvol",
        "ro:@system/2.0",
        "--subvol",
        "other",
    ];

    fn volume(links: impl FnOnce(&Path)) -> Volume<Vec<u8>> {
        let image = mkfs("versions", &VERSIONS, |t| {
            write(t, "@system/1.0/mikros", b"one");
            write(t, "@system/2.0/mikros", b"two");
            write(t, "@system/plain/mikros", b"plain");
            write(t, "other/x", b"x");
            links(&t.join("@system"));
        });
        Volume::open(image).unwrap()
    }

    fn both(system: &Path) {
        symlink("2.0", system.join("current")).unwrap();
        symlink("1.0", system.join("previous")).unwrap();
    }

    fn uncounted(_: &str, _: Option<&str>) -> String {
        panic!("not counted")
    }

    #[test]
    fn reads_the_request() {
        assert_eq!(request(""), Request::Current);
        assert_eq!(request("system=previous system=current"), Request::Current);
        assert_eq!(request("system=current system=previous"), Request::Previous);
        assert_eq!(request("system=1.0"), Request::Named("1.0"));
    }

    #[test]
    fn checks_names() {
        assert!(valid_name("0.1.0-debug_x"));
        for name in ["", "current", "previous", "a/b", "a b", "é"] {
            assert!(!valid_name(name), "{name}");
        }
    }

    #[test]
    fn boots_the_current_version_as_counted() {
        let mut v = volume(both);
        let mut asked = None;
        let chosen = choose(&mut v, "quiet", |current, previous| {
            asked = Some((current.to_string(), previous.map(str::to_string)));
            current.to_string()
        })
        .unwrap();
        assert_eq!(chosen.name, "2.0");
        assert_eq!(asked, Some(("2.0".into(), Some("1.0".into()))));
        let fallen = choose(&mut v, "system=current", |_, p| p.unwrap().into()).unwrap();
        assert_eq!(fallen.name, "1.0");
        assert_ne!(fallen.root, chosen.root);
    }

    #[test]
    fn boots_other_versions_without_counting() {
        let mut v = volume(both);
        assert_eq!(
            choose(&mut v, "system=previous", uncounted).unwrap().name,
            "1.0"
        );
        assert_eq!(choose(&mut v, "system=2.0", uncounted).unwrap().name, "2.0");
        let err = |v: &mut Volume<Vec<u8>>, line: &str| choose(v, line, uncounted).unwrap_err();
        assert_eq!(err(&mut v, "system=3.0"), Error::NoVersion("3.0".into()));
        assert_eq!(
            err(&mut v, "system=plain"),
            Error::NoVersion("plain".into())
        );
        assert_eq!(err(&mut v, "system=a/b"), Error::NoVersion("a/b".into()));
    }

    #[test]
    fn checks_the_links() {
        let mut v = volume(|s| symlink("2.0", s.join("current")).unwrap());
        let chosen = choose(&mut v, "", |c, p| {
            assert_eq!(p, None);
            c.into()
        });
        assert_eq!(chosen.unwrap().name, "2.0");
        assert_eq!(
            choose(&mut v, "system=previous", uncounted),
            Err(Error::NoLink("previous"))
        );
        let mut v = volume(|s| {
            symlink("3.0", s.join("current")).unwrap();
            symlink("../other", s.join("previous")).unwrap();
        });
        assert_eq!(
            choose(&mut v, "", uncounted),
            Err(Error::BadTarget("current", "3.0".into()))
        );
        assert_eq!(
            choose(&mut v, "system=previous", uncounted),
            Err(Error::BadTarget("previous", "../other".into()))
        );
        let mut v = volume(|s| write(s, "current", b"2.0"));
        assert_eq!(
            choose(&mut v, "", uncounted),
            Err(Error::NotALink("current"))
        );
        let mut v = Volume::open(mkfs("nosystem", &[], |t| write(t, "x", b""))).unwrap();
        assert_eq!(choose(&mut v, "", uncounted), Err(Error::NoSystem));
    }
}
