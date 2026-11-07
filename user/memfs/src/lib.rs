//! The node tree of an in-memory file system, for the temporary file
//! system: directories, files, symbolic links and sockets, their names
//! and links, and path resolution. What files hold, `C`, and what
//! sockets refer to, `S`, are the server's.

#![no_std]

extern crate alloc;

use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::vec::Vec;

/// A node's number, which is also its inode number.
pub type Id = u64;
/// The root directory.
pub const ROOT: Id = 1;
/// The longest name, in bytes.
pub const NAME_MAX: usize = 255;
/// How many symbolic links one resolution follows before it fails.
pub const LINKS_MAX: usize = 40;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    NotFound,
    Exists,
    NotDirectory,
    IsDirectory,
    NotEmpty,
    Invalid,
    AccessDenied,
    NameTooLong,
    Loop,
    NotSupported,
}

/// A node's times, in nanoseconds since 1970 in UTC.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Times {
    pub created: i64,
    pub modified: i64,
    pub accessed: i64,
    pub changed: i64,
}

impl Times {
    pub fn at(now: i64) -> Times {
        Times {
            created: now,
            modified: now,
            accessed: now,
            changed: now,
        }
    }
}

/// A directory's entries, by the cookie they got when they were added,
/// and their cookies by name. Listing follows the cookies, so that it
/// stays in order while entries come and go.
#[derive(Debug, Default)]
pub struct Directory {
    entries: BTreeMap<u64, (String, Id)>,
    names: BTreeMap<String, u64>,
    next: u64,
}

impl Directory {
    pub fn get(&self, name: &str) -> Option<Id> {
        let cookie = self.names.get(name)?;
        self.entries.get(cookie).map(|(_, id)| *id)
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    fn insert(&mut self, name: &str, id: Id) {
        let cookie = self.next;
        self.next += 1;
        self.entries.insert(cookie, (String::from(name), id));
        self.names.insert(String::from(name), cookie);
    }

    fn remove(&mut self, name: &str) -> Option<Id> {
        let cookie = self.names.remove(name)?;
        self.entries.remove(&cookie).map(|(_, id)| id)
    }
}

pub enum Kind<C, S> {
    Directory(Directory),
    File(C),
    Symlink(String),
    Socket(S),
}

pub struct Node<C, S> {
    pub kind: Kind<C, S>,
    pub mode: u32,
    pub owner: u32,
    pub times: Times,
    /// The directories that hold the node, with the names they give it.
    pub names: Vec<(Id, String)>,
}

impl<C, S> Node<C, S> {
    pub fn is_directory(&self) -> bool {
        matches!(self.kind, Kind::Directory(_))
    }

    pub fn directory(&self) -> Option<&Directory> {
        match &self.kind {
            Kind::Directory(d) => Some(d),
            _ => None,
        }
    }
}

/// Where a path leads.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Resolved {
    Node(Id),
    /// A symbolic link that is not followed, with the rest of the path.
    Redirect(String),
    /// The last component does not exist in the directory `parent`.
    Missing {
        parent: Id,
        name: String,
    },
}

/// What a removal did: the node, and whether that was its last name.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Removed {
    pub id: Id,
    pub last: bool,
}

pub struct Tree<C, S> {
    nodes: BTreeMap<Id, Node<C, S>>,
    next: Id,
}

/// Checks a name that a directory entry gets.
pub fn check_name(name: &str) -> Result<(), Error> {
    match name {
        "" | "." | ".." => Err(Error::Invalid),
        n if n.contains('/') || n.contains('\0') => Err(Error::Invalid),
        n if n.len() > NAME_MAX => Err(Error::NameTooLong),
        _ => Ok(()),
    }
}

impl<C, S> Tree<C, S> {
    /// A tree holding an empty root directory with `mode`, made at `now`.
    pub fn new(mode: u32, owner: u32, now: i64) -> Tree<C, S> {
        let root = Node {
            kind: Kind::Directory(Directory::default()),
            mode,
            owner,
            times: Times::at(now),
            names: Vec::new(),
        };
        let mut nodes = BTreeMap::new();
        nodes.insert(ROOT, root);
        Tree {
            nodes,
            next: ROOT + 1,
        }
    }

    pub fn get(&self, id: Id) -> Option<&Node<C, S>> {
        self.nodes.get(&id)
    }

    pub fn get_mut(&mut self, id: Id) -> Option<&mut Node<C, S>> {
        self.nodes.get_mut(&id)
    }

    fn dir(&self, id: Id) -> Result<&Directory, Error> {
        self.nodes
            .get(&id)
            .ok_or(Error::NotFound)?
            .directory()
            .ok_or(Error::NotDirectory)
    }

    fn dir_mut(&mut self, id: Id) -> Result<&mut Directory, Error> {
        match &mut self.nodes.get_mut(&id).ok_or(Error::NotFound)?.kind {
            Kind::Directory(d) => Ok(d),
            _ => Err(Error::NotDirectory),
        }
    }

    /// The node that `name` names in the directory `dir`.
    pub fn lookup(&self, dir: Id, name: &str) -> Result<Id, Error> {
        self.dir(dir)?.get(name).ok_or(Error::NotFound)
    }

    /// The link count of `id`: 2 plus its subdirectories for a
    /// directory, and its names otherwise.
    pub fn links(&self, id: Id) -> u64 {
        let Some(node) = self.nodes.get(&id) else {
            return 0;
        };
        match node.directory() {
            Some(d) => {
                let subdirs = d
                    .entries
                    .values()
                    .filter(|(_, c)| self.get(*c).is_some_and(Node::is_directory));
                2 + subdirs.count() as u64
            }
            None => node.names.len() as u64,
        }
    }

    /// Resolves `path`, relative to the directory `dir`, following a
    /// symbolic link last in the path unless `no_follow`. Links whose
    /// relative targets stay at or below `dir` are followed; for any
    /// other, the link's target joined with the rest of the path comes
    /// back as a redirect. An empty path, one that starts with `/` or has
    /// an empty component other than a final one fails with `Invalid`,
    /// `..` with `AccessDenied`, and a component over 255 bytes with
    /// `NameTooLong`. More than 40 links fail with `Loop`.
    pub fn resolve(&self, dir: Id, path: &str, no_follow: bool) -> Result<Resolved, Error> {
        if path.is_empty() || path.starts_with('/') {
            return Err(Error::Invalid);
        }
        let path = path.strip_suffix('/').unwrap_or(path);
        let mut components: Vec<String> = Vec::new();
        for c in path.split('/') {
            match c {
                "" => return Err(Error::Invalid),
                ".." => return Err(Error::AccessDenied),
                c if c.len() > NAME_MAX => return Err(Error::NameTooLong),
                c => components.push(String::from(c)),
            }
        }
        // The components still to resolve, the last one first.
        let mut rest: Vec<String> = components.into_iter().rev().collect();
        let mut stack = alloc::vec![dir];
        let mut followed = 0;
        while let Some(name) = rest.pop() {
            let at = *stack.last().expect("the stack holds `dir`");
            match name.as_str() {
                "." => continue,
                // Only targets that stay within `dir` get here.
                ".." => {
                    stack.pop();
                    continue;
                }
                _ => {}
            }
            let directory = self.dir(at)?;
            let Some(node) = directory.get(&name) else {
                if rest.is_empty() {
                    return Ok(Resolved::Missing { parent: at, name });
                }
                return Err(Error::NotFound);
            };
            let Some(Kind::Symlink(target)) = self.get(node).map(|n| &n.kind) else {
                stack.push(node);
                continue;
            };
            if rest.is_empty() && no_follow {
                stack.push(node);
                continue;
            }
            followed += 1;
            if followed > LINKS_MAX {
                return Err(Error::Loop);
            }
            if stays_within(stack.len() - 1, target) {
                rest.extend(
                    target
                        .split('/')
                        .filter(|c| !c.is_empty())
                        .rev()
                        .map(String::from),
                );
                continue;
            }
            let mut redirect = target.clone();
            for c in rest.iter().rev() {
                redirect.push('/');
                redirect.push_str(c);
            }
            return Ok(Resolved::Redirect(redirect));
        }
        Ok(Resolved::Node(
            *stack.last().expect("the stack holds `dir`"),
        ))
    }

    /// Adds a node of `kind` as `name` in the directory `dir`.
    pub fn create(
        &mut self,
        dir: Id,
        name: &str,
        kind: Kind<C, S>,
        mode: u32,
        owner: u32,
        now: i64,
    ) -> Result<Id, Error> {
        check_name(name)?;
        if self.dir(dir)?.get(name).is_some() {
            return Err(Error::Exists);
        }
        let id = self.next;
        self.next += 1;
        let names = alloc::vec![(dir, String::from(name))];
        self.nodes.insert(
            id,
            Node {
                kind,
                mode,
                owner,
                times: Times::at(now),
                names,
            },
        );
        self.dir_mut(dir)?.insert(name, id);
        self.touch(dir, now);
        Ok(id)
    }

    /// Marks the directory `dir` changed at `now`.
    fn touch(&mut self, dir: Id, now: i64) {
        if let Some(node) = self.nodes.get_mut(&dir) {
            node.times.modified = now;
            node.times.changed = now;
        }
    }

    /// Removes the name `name` from the directory `dir`. It must name a
    /// directory, an empty one, when `directory`, and anything else when
    /// not. A node whose last name goes stays in the tree until `forget`.
    pub fn unlink(
        &mut self,
        dir: Id,
        name: &str,
        directory: bool,
        now: i64,
    ) -> Result<Removed, Error> {
        let id = self.lookup(dir, name)?;
        let node = self.nodes.get(&id).ok_or(Error::NotFound)?;
        match (node.directory(), directory) {
            (Some(_), false) => return Err(Error::IsDirectory),
            (None, true) => return Err(Error::NotDirectory),
            (Some(d), true) if !d.is_empty() => return Err(Error::NotEmpty),
            _ => {}
        }
        self.dir_mut(dir)?.remove(name);
        self.touch(dir, now);
        let node = self.nodes.get_mut(&id).ok_or(Error::NotFound)?;
        node.names.retain(|(d, n)| !(*d == dir && n == name));
        node.times.changed = now;
        Ok(Removed {
            id,
            last: node.names.is_empty(),
        })
    }

    /// Drops the node `id`, which has no name left.
    pub fn forget(&mut self, id: Id) -> Option<Node<C, S>> {
        match self.nodes.get(&id) {
            Some(node) if node.names.is_empty() && id != ROOT => self.nodes.remove(&id),
            _ => None,
        }
    }

    /// The directories above `id`, nearest first, by the first name of
    /// each: its directory, that directory's, and so on to the root.
    pub fn ancestors(&self, id: Id) -> Vec<Id> {
        let mut out = Vec::new();
        let mut current = id;
        for _ in 0..self.nodes.len() {
            match self.nodes.get(&current).and_then(|n| n.names.first()) {
                Some((parent, _)) => {
                    out.push(*parent);
                    current = *parent;
                }
                None => break,
            }
        }
        out
    }

    /// Whether the directory `dir` is `ancestor` or lies below it.
    fn is_within(&self, dir: Id, ancestor: Id) -> bool {
        let mut current = dir;
        for _ in 0..self.nodes.len() {
            if current == ancestor {
                return true;
            }
            match self.nodes.get(&current).and_then(|n| n.names.first()) {
                Some((parent, _)) => current = *parent,
                None => return false,
            }
        }
        false
    }

    /// Moves the entry `name` of `from` to `new_name` in `to`, as POSIX
    /// `rename` does: an existing target is replaced, a file by a file
    /// and an empty directory by a directory. Returns what was replaced.
    pub fn rename(
        &mut self,
        from: Id,
        name: &str,
        to: Id,
        new_name: &str,
        now: i64,
    ) -> Result<Option<Removed>, Error> {
        check_name(new_name)?;
        let id = self.lookup(from, name)?;
        let moving_dir = self.get(id).is_some_and(Node::is_directory);
        self.dir(to)?;
        if moving_dir && self.is_within(to, id) {
            return Err(Error::Invalid);
        }
        let replaced = match self.dir(to)?.get(new_name) {
            // Two names of the same node: POSIX leaves both.
            Some(existing) if existing == id => return Ok(None),
            Some(existing) => {
                let target_dir = self.get(existing).is_some_and(Node::is_directory);
                match (moving_dir, target_dir) {
                    (false, true) => return Err(Error::IsDirectory),
                    (true, false) => return Err(Error::NotDirectory),
                    _ => {}
                }
                Some(self.unlink(to, new_name, target_dir, now)?)
            }
            None => None,
        };
        self.dir_mut(from)?.remove(name);
        self.dir_mut(to)?.insert(new_name, id);
        self.touch(from, now);
        self.touch(to, now);
        let node = self.nodes.get_mut(&id).ok_or(Error::NotFound)?;
        for (d, n) in node.names.iter_mut() {
            if *d == from && n == name {
                *d = to;
                *n = String::from(new_name);
            }
        }
        node.times.changed = now;
        Ok(replaced)
    }

    /// Gives the file `name` of `from` the further name `new_name` in
    /// `to`. Directories cannot be linked.
    pub fn link(
        &mut self,
        from: Id,
        name: &str,
        to: Id,
        new_name: &str,
        now: i64,
    ) -> Result<Id, Error> {
        check_name(new_name)?;
        let id = self.lookup(from, name)?;
        if self.get(id).is_some_and(Node::is_directory) {
            return Err(Error::IsDirectory);
        }
        if self.dir(to)?.get(new_name).is_some() {
            return Err(Error::Exists);
        }
        self.dir_mut(to)?.insert(new_name, id);
        self.touch(to, now);
        let node = self.nodes.get_mut(&id).ok_or(Error::NotFound)?;
        node.names.push((to, String::from(new_name)));
        node.times.changed = now;
        Ok(id)
    }

    /// The entries of the directory `dir` from position `cookie`, at most
    /// `max`, and the position to continue from.
    pub fn list(
        &self,
        dir: Id,
        cookie: u64,
        max: usize,
    ) -> Result<(Vec<(String, Id)>, u64), Error> {
        let directory = self.dir(dir)?;
        let mut next = cookie;
        let mut out = Vec::new();
        for (&at, (name, id)) in directory.entries.range(cookie..).take(max) {
            out.push((name.clone(), *id));
            next = at + 1;
        }
        Ok((out, next))
    }
}

/// Whether the relative `target` of a link `depth` levels below the
/// opened directory leads to it or below it.
fn stays_within(depth: usize, target: &str) -> bool {
    if target.is_empty() || target.starts_with('/') {
        return false;
    }
    let mut depth = depth as isize;
    for c in target.split('/') {
        match c {
            "" | "." => {}
            ".." => {
                depth -= 1;
                if depth < 0 {
                    return false;
                }
            }
            _ => depth += 1,
        }
    }
    true
}

#[cfg(test)]
mod tests;
