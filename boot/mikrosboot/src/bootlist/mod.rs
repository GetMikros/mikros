//! The boot list of a system version and the boot modules it names.

use crate::btrfs::{self, Kind, Node, Volume};
use crate::disk::Disk;
use alloc::collections::BTreeSet;
use alloc::string::{String, ToString};
use alloc::vec;
use alloc::vec::Vec;
use core::fmt;

#[cfg(test)]
mod tests;

/// The most modules the bootloader loads.
pub const MODULES_MAX: usize = 4096;
/// The longest path of a line, and of a module.
pub const PATH_MAX: usize = 1024;
/// The most symbolic links through which a path may lead.
pub const LINKS_MAX: u32 = 40;

/// Why the boot list could not be followed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Error {
    Fs(btrfs::Error),
    /// The list is not valid UTF-8.
    NotUtf8,
    /// The line of this number is neither empty, a comment nor a path.
    Line(usize),
    /// Nothing exists at the path.
    Missing(String),
    /// The path names a file but leads to something else.
    NotAFile(String),
    /// The path names a directory, or leads through one, but leads to
    /// something else.
    NotADirectory(String),
    /// A symbolic link on the path has an absolute target.
    AbsoluteLink(String),
    /// A symbolic link on the path leads out of the system version.
    Outside(String),
    /// The path leads through more than `LINKS_MAX` symbolic links.
    Links(String),
    /// A file below a directory has a name that is not valid UTF-8, or a
    /// path longer than `PATH_MAX` bytes.
    BadPath(String),
    /// There would be more than `MODULES_MAX` modules.
    TooMany,
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
            Error::NotUtf8 => write!(f, "boot/modules is not valid UTF-8"),
            Error::Line(n) => write!(f, "line {n} of boot/modules is not valid"),
            Error::Missing(p) => write!(f, "{p} does not exist"),
            Error::NotAFile(p) => write!(f, "{p} is not a regular file"),
            Error::NotADirectory(p) => write!(f, "{p} is not a directory"),
            Error::AbsoluteLink(p) => write!(f, "{p} leads through a link with an absolute target"),
            Error::Outside(p) => write!(f, "{p} leads out of the system version"),
            Error::Links(p) => write!(f, "{p} leads through more than {LINKS_MAX} links"),
            Error::BadPath(p) => write!(f, "{p} has no valid module path"),
            Error::TooMany => write!(f, "boot/modules names more than {MODULES_MAX} files"),
        }
    }
}

/// A path of the list, and whether it names a directory.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Line<'a> {
    pub path: &'a str,
    pub directory: bool,
}

/// Whether `path` is a path relative to the root of the system version.
fn valid(path: &str) -> bool {
    !path.is_empty()
        && path.len() <= PATH_MAX
        && path
            .split('/')
            .all(|c| !c.is_empty() && c != "." && c != "..")
}

/// The paths of the boot list `list`, in order.
pub fn parse(list: &[u8]) -> Result<Vec<Line<'_>>, Error> {
    let text = core::str::from_utf8(list).map_err(|_| Error::NotUtf8)?;
    let mut lines = Vec::new();
    for (i, line) in text.split('\n').enumerate() {
        let line = line.trim_matches([' ', '\t']);
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let (path, directory) = match line.strip_suffix('/') {
            Some(path) => (path, true),
            None => (line, false),
        };
        if !valid(path) {
            return Err(Error::Line(i + 1));
        }
        lines.push(Line { path, directory });
    }
    Ok(lines)
}

/// A file to load: its path within the system version, and its inode
/// and size.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Module {
    pub path: String,
    pub node: Node,
    pub size: u64,
}

/// Where a path leads: the directories from the version's root to the
/// last one it went through, and the node it ends at.
struct Resolved {
    dirs: Vec<Node>,
    node: Node,
    kind: Kind,
}

struct Walk<'v, D> {
    volume: &'v mut Volume<D>,
    modules: Vec<Module>,
    seen: BTreeSet<String>,
}

impl<D: Disk> Walk<'_, D> {
    /// Follows `components` from the last of `dirs`, resolving symbolic
    /// links relative to their directory. `links` counts the links that
    /// `path` led through so far.
    fn resolve(
        &mut self,
        mut dirs: Vec<Node>,
        components: &[&[u8]],
        links: &mut u32,
        path: &str,
    ) -> Result<Resolved, Error> {
        let mut queue: Vec<Vec<u8>> = components.iter().rev().map(|c| c.to_vec()).collect();
        while let Some(name) = queue.pop() {
            match &name[..] {
                b"" | b"." => continue,
                b".." if dirs.len() == 1 => return Err(Error::Outside(path.to_string())),
                b".." => {
                    dirs.pop();
                    continue;
                }
                _ => {}
            }
            let dir = *dirs.last().expect("the root is never left");
            let node = self
                .volume
                .lookup(dir, &name)?
                .ok_or_else(|| Error::Missing(path.to_string()))?;
            match self.volume.inode(node)?.kind {
                Kind::Directory => dirs.push(node),
                Kind::Symlink => {
                    *links += 1;
                    if *links > LINKS_MAX {
                        return Err(Error::Links(path.to_string()));
                    }
                    let target = self.volume.read_link(node)?;
                    if target.starts_with(b"/") {
                        return Err(Error::AbsoluteLink(path.to_string()));
                    }
                    queue.extend(target.split(|&b| b == b'/').rev().map(<[u8]>::to_vec));
                }
                kind if queue.is_empty() => return Ok(Resolved { dirs, node, kind }),
                _ => return Err(Error::NotADirectory(path.to_string())),
            }
        }
        let node = *dirs.last().expect("the root is never left");
        Ok(Resolved {
            dirs,
            node,
            kind: Kind::Directory,
        })
    }

    /// Adds `node`, a regular file, as the module `path` unless a module
    /// has that path already.
    fn add(&mut self, path: String, node: Node) -> Result<(), Error> {
        if self.seen.contains(&path) {
            return Ok(());
        }
        if self.modules.len() == MODULES_MAX {
            return Err(Error::TooMany);
        }
        let size = self.volume.inode(node)?.size;
        self.seen.insert(path.clone());
        self.modules.push(Module { path, node, size });
        Ok(())
    }

    /// Collects the regular files below the last of `dirs`, which has the
    /// path `prefix`, and those that the links below it lead to.
    fn below(
        &mut self,
        dirs: &[Node],
        prefix: &str,
        links: u32,
        found: &mut Vec<(String, Node)>,
    ) -> Result<(), Error> {
        let dir = *dirs.last().expect("a directory");
        for (name, _) in self.volume.entries(dir)? {
            let path = match core::str::from_utf8(&name) {
                Ok(name) => alloc::format!("{prefix}/{name}"),
                Err(_) => return Err(Error::BadPath(alloc::format!("{prefix}/…"))),
            };
            // Resolving the entry by its name follows it if it is a link.
            let mut links = links;
            let r = self.resolve(dirs.to_vec(), &[&name], &mut links, &path)?;
            match r.kind {
                Kind::File if path.len() > PATH_MAX => return Err(Error::BadPath(path)),
                Kind::File => found.push((path, r.node)),
                Kind::Directory => self.below(&r.dirs, &path, links, found)?,
                _ => {}
            }
            if found.len() > MODULES_MAX {
                return Err(Error::TooMany);
            }
        }
        Ok(())
    }
}

/// The modules that the boot list `list` names in the system version whose
/// root directory is `root`, in the order to load them.
pub fn modules<D: Disk>(
    volume: &mut Volume<D>,
    root: Node,
    list: &[u8],
) -> Result<Vec<Module>, Error> {
    let lines = parse(list)?;
    let mut walk = Walk {
        volume,
        modules: Vec::new(),
        seen: BTreeSet::new(),
    };
    for line in lines {
        let components: Vec<&[u8]> = line.path.split('/').map(str::as_bytes).collect();
        let mut links = 0;
        let r = walk.resolve(vec![root], &components, &mut links, line.path)?;
        if !line.directory {
            if r.kind != Kind::File {
                return Err(Error::NotAFile(line.path.to_string()));
            }
            walk.add(line.path.to_string(), r.node)?;
            continue;
        }
        if r.kind != Kind::Directory {
            return Err(Error::NotADirectory(alloc::format!("{}/", line.path)));
        }
        let mut found = Vec::new();
        walk.below(&r.dirs, line.path, links, &mut found)?;
        found.sort_by(|a, b| a.0.as_bytes().cmp(b.0.as_bytes()));
        for (path, node) in found {
            walk.add(path, node)?;
        }
    }
    Ok(walk.modules)
}

/// The file at `path` in the system version whose root directory is
/// `root`, following symbolic links as the boot list does.
pub fn file<D: Disk>(volume: &mut Volume<D>, root: Node, path: &str) -> Result<Module, Error> {
    let mut walk = Walk {
        volume,
        modules: Vec::new(),
        seen: BTreeSet::new(),
    };
    let components: Vec<&[u8]> = path.split('/').map(str::as_bytes).collect();
    let r = walk.resolve(vec![root], &components, &mut 0, path)?;
    if r.kind != Kind::File {
        return Err(Error::NotAFile(path.to_string()));
    }
    let size = walk.volume.inode(r.node)?.size;
    Ok(Module {
        path: path.to_string(),
        node: r.node,
        size,
    })
}
