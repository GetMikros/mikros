//! The node tree of the boot image, and path resolution in it, and on
//! Mikros the file system server that serves it, `server`, which the
//! process manager and the test init run.
//!
//! The tree is read-only. Node 0 is the root directory; a node's inode
//! number is its index plus 1.

#![no_std]

extern crate alloc;

#[cfg(target_os = "mikros")]
pub mod server;

use alloc::collections::BTreeMap;
use alloc::vec::Vec;
use mikros_ustar::{Entry, Kind};

/// The longest path component, in bytes.
pub const NAME_MAX: usize = 255;
/// How many symbolic links one resolution follows before it fails.
pub const LINKS_MAX: usize = 40;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum NodeKind {
    /// The entries, by name, with the nodes they name.
    Directory(BTreeMap<Vec<u8>, usize>),
    /// Where the data is in the image, and its size.
    File { offset: usize, size: usize },
    /// The link's target.
    Symlink(Vec<u8>),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Node {
    pub kind: NodeKind,
    pub mode: u32,
    pub owner: u32,
    /// Seconds since the Unix epoch.
    pub mtime: u64,
    /// How many names the node has.
    pub links: u64,
}

impl Node {
    fn directory() -> Node {
        Node {
            kind: NodeKind::Directory(BTreeMap::new()),
            mode: 0o755,
            owner: 0,
            mtime: 0,
            links: 1,
        }
    }

    /// A node for `entry`, which has no names yet.
    fn from_entry(kind: NodeKind, entry: &Entry) -> Node {
        Node {
            kind,
            mode: entry.mode(),
            owner: entry.uid(),
            mtime: entry.mtime(),
            links: 0,
        }
    }

    fn set_attributes(&mut self, entry: &Entry) {
        self.mode = entry.mode();
        self.owner = entry.uid();
        self.mtime = entry.mtime();
    }
}

/// Why a path does not resolve.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    NotFound,
    NotDirectory,
    Invalid,
    AccessDenied,
    NameTooLong,
    Loop,
}

/// What a path resolves to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Resolved {
    Node(usize),
    /// A symbolic link that resolution does not follow: its target
    /// joined with the rest of the path, for the client to resolve.
    Redirect(Vec<u8>),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Tree {
    pub nodes: Vec<Node>,
    /// The members left out: their path and type flag.
    pub skipped: Vec<(Vec<u8>, u8)>,
}

fn path_of(entry: &Entry) -> Vec<u8> {
    entry.components().collect::<Vec<_>>().join(&b'/')
}

impl Tree {
    /// The tree of the archive `image`. Directories that only appear in
    /// the paths of members are created with mode 0o755, owner 0 and
    /// time 0. A later member with the same path replaces an earlier
    /// one, except that a directory member only sets the attributes of
    /// a directory that is already there.
    pub fn new(image: &[u8]) -> Tree {
        let mut tree = Tree {
            nodes: alloc::vec![Node::directory()],
            skipped: Vec::new(),
        };
        for entry in mikros_ustar::entries(image) {
            tree.add(&entry);
        }
        tree
    }

    fn add(&mut self, entry: &Entry) {
        let skip = |tree: &mut Tree, flag| tree.skipped.push((path_of(entry), flag));
        let components: Vec<&[u8]> = entry.components().collect();
        let kind = entry.kind();
        let Some((name, parents)) = components.split_last() else {
            // The root itself.
            if kind == Kind::Directory {
                self.nodes[0].set_attributes(entry);
            }
            return;
        };
        let Some(parent) = self.directories(parents) else {
            return skip(self, b'5');
        };
        let existing = self.child(parent, name);
        let node = match kind {
            Kind::Directory => {
                if let Some(dir) = existing.filter(|&d| self.is_directory(d)) {
                    return self.nodes[dir].set_attributes(entry);
                }
                self.push(Node::from_entry(
                    NodeKind::Directory(BTreeMap::new()),
                    entry,
                ))
            }
            Kind::File => self.push(Node::from_entry(
                NodeKind::File {
                    offset: entry.offset,
                    size: entry.data.len(),
                },
                entry,
            )),
            Kind::Symlink => self.push(Node::from_entry(
                NodeKind::Symlink(entry.link_name().to_vec()),
                entry,
            )),
            Kind::HardLink => {
                let target: Vec<&[u8]> = entry
                    .link_name()
                    .split(|&b| b == b'/')
                    .filter(|c| !c.is_empty() && *c != b".")
                    .collect();
                match self.lookup(&target) {
                    Some(node) if !self.is_directory(node) => node,
                    _ => return skip(self, b'1'),
                }
            }
            Kind::Other(flag) => return skip(self, flag),
        };
        if existing != Some(node) {
            if let Some(old) = existing {
                self.nodes[old].links -= 1;
            }
            self.nodes[node].links += 1;
            self.entries_mut(parent).insert(name.to_vec(), node);
        }
    }

    fn push(&mut self, node: Node) -> usize {
        self.nodes.push(node);
        self.nodes.len() - 1
    }

    fn is_directory(&self, node: usize) -> bool {
        matches!(self.nodes[node].kind, NodeKind::Directory(_))
    }

    fn entries_mut(&mut self, dir: usize) -> &mut BTreeMap<Vec<u8>, usize> {
        match &mut self.nodes[dir].kind {
            NodeKind::Directory(entries) => entries,
            _ => unreachable!("only directories have entries"),
        }
    }

    /// The node that `name` names in `dir`.
    pub fn child(&self, dir: usize, name: &[u8]) -> Option<usize> {
        match &self.nodes[dir].kind {
            NodeKind::Directory(entries) => entries.get(name).copied(),
            _ => None,
        }
    }

    /// The node at `path` from the root, following no links.
    fn lookup(&self, path: &[&[u8]]) -> Option<usize> {
        path.iter().try_fold(0, |dir, name| self.child(dir, name))
    }

    /// The directory at `path` from the root, created where missing, or
    /// `None` when something else is in the way.
    fn directories(&mut self, path: &[&[u8]]) -> Option<usize> {
        let mut dir = 0;
        for name in path {
            dir = match self.child(dir, name) {
                Some(d) if self.is_directory(d) => d,
                Some(_) => return None,
                None => {
                    let d = self.push(Node::directory());
                    self.entries_mut(dir).insert(name.to_vec(), d);
                    d
                }
            };
        }
        Some(dir)
    }

    /// Resolves `path`, relative to the directory `dir`. A symbolic link
    /// is followed when its target is relative and stays within `dir`;
    /// any other link ends resolution with a redirect. With `no_follow`,
    /// a link as the last component is the result itself.
    /// Whether `node` is `dir` or lies below it, reached through
    /// directory entries only, without following links.
    pub fn contains(&self, dir: usize, node: usize) -> bool {
        let mut pending = alloc::vec![dir];
        let mut seen = alloc::collections::BTreeSet::new();
        while let Some(at) = pending.pop() {
            if at == node {
                return true;
            }
            if !seen.insert(at) {
                continue;
            }
            if let NodeKind::Directory(entries) = &self.nodes[at].kind {
                pending.extend(entries.values().copied());
            }
        }
        false
    }

    pub fn resolve(&self, dir: usize, path: &[u8], no_follow: bool) -> Result<Resolved, Error> {
        if path.is_empty() || path[0] == b'/' {
            return Err(Error::Invalid);
        }
        let mut components: Vec<&[u8]> = path.split(|&b| b == b'/').collect();
        if components.last() == Some(&&b""[..]) {
            components.pop();
        }
        for c in &components {
            match *c {
                b"" => return Err(Error::Invalid),
                b".." => return Err(Error::AccessDenied),
                c if c.len() > NAME_MAX => return Err(Error::NameTooLong),
                _ => {}
            }
        }
        // The components still to resolve, the last one first.
        let mut rest: Vec<&[u8]> = components.into_iter().rev().collect();
        let mut stack = alloc::vec![dir];
        let mut followed = 0;
        while let Some(name) = rest.pop() {
            let at = *stack.last().expect("the stack holds `dir`");
            match name {
                b"" | b"." => continue,
                b".." => {
                    // Only targets that stay within `dir` are followed.
                    stack.pop();
                    continue;
                }
                _ => {}
            }
            if !self.is_directory(at) {
                return Err(Error::NotDirectory);
            }
            let node = self.child(at, name).ok_or(Error::NotFound)?;
            let NodeKind::Symlink(target) = &self.nodes[node].kind else {
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
                rest.extend(target.split(|&b| b == b'/').rev());
                continue;
            }
            let mut redirect = target.clone();
            for c in rest.iter().rev() {
                redirect.push(b'/');
                redirect.extend_from_slice(c);
            }
            return Ok(Resolved::Redirect(redirect));
        }
        Ok(Resolved::Node(
            *stack.last().expect("the stack holds `dir`"),
        ))
    }

    /// Up to `count` entries of the directory `dir` from position
    /// `cookie`, in name order, and the position after them.
    pub fn read_directory(
        &self,
        dir: usize,
        cookie: u64,
        count: usize,
    ) -> (Vec<(&[u8], usize)>, u64) {
        let NodeKind::Directory(entries) = &self.nodes[dir].kind else {
            return (Vec::new(), cookie);
        };
        let skip = usize::try_from(cookie).unwrap_or(usize::MAX);
        let out: Vec<(&[u8], usize)> = entries
            .iter()
            .skip(skip)
            .take(count)
            .map(|(name, &node)| (name.as_slice(), node))
            .collect();
        let next = cookie + out.len() as u64;
        (out, next)
    }
}

/// Whether the relative `target` of a link `depth` levels below the
/// opened directory leads to it or below it.
fn stays_within(depth: usize, target: &[u8]) -> bool {
    if target.first() == Some(&b'/') || target.is_empty() {
        return false;
    }
    let mut depth = depth as isize;
    for c in target.split(|&b| b == b'/') {
        match c {
            b"" | b"." => {}
            b".." => {
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
