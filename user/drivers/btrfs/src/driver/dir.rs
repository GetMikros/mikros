// SPDX-License-Identifier: GPL-2.0
//! Resolving paths (MIKSPEC-0049.R4, R5), also case-insensitively
//! (MIKSPEC-0071.R17), opening and creating nodes (MIKSPEC-0049.R6), and
//! listing directories (MIKSPEC-0049.R12).

use crate::fs::{self, Key, Open, error};
use crate::node::{Connection, MODE_BITS, NAME_MAX, TARGET_MAX};
use crate::watch::event;
use crate::work::blocking;
use alloc::boxed::Box;
use alloc::rc::Rc;
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec;
use alloc::vec::Vec;
use mikros_btrfs::node::{
    Attributes as BtrfsAttributes, Kind, MODE_DIRECTORY, MODE_SYMLINK, MODE_TYPE, Node as BtrfsNode,
};
use mikros_rt::exec::Reactor;
use mikros_rt::interfaces::mikros::fs::*;

/// The most entries one directory read returns, the limit of its reply.
const ENTRIES_MAX: usize = 64;
/// The most symbolic links one `Open` follows.
const LINKS_MAX: usize = 40;
/// The `DT_` types of directory entries.
const DT_DIR: u32 = 4;
const DT_LNK: u32 = 10;
const DT_SOCK: u32 = 12;
/// The shim's errors that lookups give: no such entry, and a name that
/// the dentry cache does not know.
const ENOENT: i32 = -2;
const EAGAIN: i32 = -11;

/// What a path names.
enum Resolved {
    Node(Rc<Open>),
    /// Nothing, as the last component of a path in the directory.
    Missing {
        parent: Rc<Open>,
        name: String,
    },
    /// A link to follow from elsewhere, with what remains of the path.
    Redirect(String),
}

impl Connection {
    /// Looks `name` up in the directory `dir`, as the open node that
    /// connections share. The root's `usr` is the current system
    /// version's root. A name that the dentry cache knows is found on the
    /// executor; any other on a thread of the pool, as finding it may
    /// read the disk, while the executor goes on serving the other
    /// connections.
    pub(crate) async fn lookup(
        &self,
        dir: &Rc<Open>,
        name: &str,
    ) -> Result<Option<Rc<Open>>, Error> {
        if self.fs.is_usr(dir, name)
            && let Some(usr) = &self.fs.usr
        {
            return Ok(Some(usr.clone()));
        }
        let found = match dir.node.lookup_cached(name.as_bytes()) {
            Err(EAGAIN) => {
                let node = dir.node.clone();
                let wanted = name.as_bytes().to_vec();
                blocking(&self.fs.reactor, move || node.lookup(&wanted)).await
            }
            found => found,
        };
        match found {
            Ok(node) => Ok(Some(self.fs.open(node, dir.key, name)?)),
            Err(ENOENT) => Ok(None),
            Err(e) => Err(error(e)),
        }
    }

    /// The first entry of the directory `dir`, in the order in which
    /// `ReadDirectory` lists them, whose name equals `name` under simple
    /// case folding (MIKSPEC-0071.R17), as the open node that connections
    /// share, with the entry's own name. Names that are not UTF-8 equal
    /// none.
    pub(crate) async fn lookup_folded(
        &self,
        dir: &Rc<Open>,
        name: &str,
    ) -> Result<Option<(Rc<Open>, String)>, Error> {
        let usr = dir.key == self.fs.root.key && self.fs.usr.is_some();
        if usr && mikros_casefold::equal(fs::USR, name) {
            let found = self.lookup(dir, fs::USR).await?;
            return Ok(found.map(|open| (open, String::from(fs::USR))));
        }
        let node = dir.node.clone();
        let wanted = String::from(name);
        let found = blocking(&self.fs.reactor, move || folded_entry(&node, &wanted, usr));
        match found.await.map_err(error)? {
            Some(entry) => {
                let open = self.lookup(dir, &entry).await?;
                Ok(open.map(|open| (open, entry)))
            }
            None => Ok(None),
        }
    }

    /// The entry `name` of the directory `dir` and its own name: on a
    /// case-insensitive connection, as `Open` with `CASE_INSENSITIVE`
    /// finds it, and otherwise exactly.
    pub(crate) async fn find(
        &self,
        dir: &Rc<Open>,
        name: &str,
    ) -> Result<Option<(Rc<Open>, String)>, Error> {
        match self.lookup(dir, name).await? {
            Some(open) => Ok(Some((open, String::from(name)))),
            None if self.folded => self.lookup_folded(dir, name).await,
            None => Ok(None),
        }
    }

    /// Fails with `EXISTS` on a case-insensitive connection when the
    /// directory `dir` holds an entry equal to the new name `name` under
    /// case folding.
    pub(crate) async fn free_folded(&self, dir: &Rc<Open>, name: &str) -> Result<(), Error> {
        match self.folded && self.lookup_folded(dir, name).await?.is_some() {
            true => Err(Error::Exists),
            false => Ok(()),
        }
    }

    /// Resolves `path` from this directory with the flags `flags` of
    /// `Open`: following a symbolic link last in the path unless
    /// `NO_FOLLOW`. Links whose relative targets stay at or below this
    /// directory are followed; for any other, the link's target joined
    /// with the rest of the path comes back as a redirect. With
    /// `CASE_INSENSITIVE`, a component that no entry has exactly is
    /// looked up under case folding, and with `CREATE` too, a last
    /// component found that way fails with `EXISTS`. The walk goes from
    /// the dentry cache on the executor; when a name is not in it, or a
    /// link or a case-folded name needs reading, the whole walk goes again
    /// in one call on a thread of the pool.
    async fn resolve(&self, path: &str, flags: OpenFlags) -> Result<Resolved, Error> {
        let reached = |open: &Rc<Open>| Reached {
            node: open.node.clone(),
            key: open.key,
            found: None,
        };
        let walk = Walk {
            start: reached(&self.open),
            root: self.fs.root.key,
            usr: self.fs.usr.as_ref().map(reached),
            path: String::from(path),
            flags,
        };
        let (walked, named) = match walk.run(true)? {
            (Walked::Uncached, _) => blocking(&self.fs.reactor, move || walk.run(false)).await?,
            done => done,
        };
        for (key, dir, name) in &named {
            self.fs.note_name(*key, *dir, name);
        }
        Ok(match walked {
            Walked::Node(node) => Resolved::Node(self.reopen(node)),
            Walked::Missing { parent, name } => Resolved::Missing {
                parent: self.reopen(parent),
                name,
            },
            Walked::Redirect(target) => Resolved::Redirect(target),
            // A walk from the disk does not end so.
            Walked::Uncached => return Err(Error::Io),
        })
    }

    /// The open node of a node that a walk reached: shared with the
    /// connections that have it open, or this directory or the root's
    /// `usr`, where the walk started or which it gave.
    fn reopen(&self, reached: Reached) -> Rc<Open> {
        match reached.found {
            Some((dir, name)) => self.fs.open_shared(reached.node, reached.key, dir, &name),
            None => match &self.fs.usr {
                Some(usr) if usr.key == reached.key => usr.clone(),
                _ => self.open.clone(),
            },
        }
    }

    pub(crate) async fn open(&self, q: NodeOpenRequest) -> Result<NodeOpenResponse, Error> {
        self.require(q.rights)?;
        if q.rights.contains(Rights::WRITE) && self.fs.read_only {
            return Err(Error::ReadOnly);
        }
        self.directory()?;
        let want_dir = q.flags.contains(OpenFlags::DIRECTORY) || q.path.ends_with('/');
        let open = match self.resolve(&q.path, q.flags).await? {
            Resolved::Redirect(target) => {
                return Ok(NodeOpenResponse {
                    r#type: NodeType::Symlink,
                    redirect: Some(target),
                });
            }
            Resolved::Missing { parent, name } if q.flags.contains(OpenFlags::CREATE) => {
                self.require_write()?;
                if self.fs.is_usr(&parent, &name) {
                    return Err(Error::AccessDenied);
                }
                let kind = match q.flags.contains(OpenFlags::DIRECTORY) {
                    true => Kind::Directory,
                    false => Kind::File,
                };
                let node = parent
                    .node
                    .create(name.as_bytes(), kind, q.mode & MODE_BITS)
                    .map_err(error)?;
                self.own(&node)?;
                self.fs
                    .watches
                    .notify(parent.key, &[event(WatchKind::Created, 0, &name)]);
                self.fs.open(node, parent.key, &name)?
            }
            Resolved::Missing { .. } => return Err(Error::NotFound),
            Resolved::Node(open) if q.flags.contains(OpenFlags::CREATE | OpenFlags::EXCLUSIVE) => {
                let usr = self.fs.usr.as_ref().is_some_and(|u| u.key == open.key);
                return Err(if usr {
                    Error::AccessDenied
                } else {
                    Error::Exists
                });
            }
            Resolved::Node(open) => open,
        };
        let r#type = fs::node_type(open.attributes()?.mode);
        if want_dir && r#type != NodeType::Directory {
            return Err(Error::NotDirectory);
        }
        if r#type == NodeType::Socket {
            return Err(Error::Invalid);
        }
        if q.flags.contains(OpenFlags::TRUNCATE) && r#type == NodeType::File {
            self.require_write()?;
            self.fs.resized(&open, Some(0));
            open.node.truncate(0).map_err(error)?;
            self.fs.changed(&open, WatchKind::Modified);
        }
        let append = q.flags.contains(OpenFlags::APPEND);
        let folded = q.flags.contains(OpenFlags::CASE_INSENSITIVE);
        self.fs
            .serve(q.node, open, q.rights, self.user, append, folded);
        Ok(NodeOpenResponse {
            r#type,
            redirect: None,
        })
    }

    /// The entries of this directory from `cookie` on, read on a thread
    /// of the pool, with their attributes when `attributes`.
    pub(crate) async fn read_directory(
        &self,
        cookie: u64,
        attributes: bool,
    ) -> Result<NodeReadDirectoryResponse, Error> {
        self.require(Rights::READ)?;
        self.directory()?;
        let mut found: Vec<(String, u64, u32)> = Vec::new();
        let root = self.open.key == self.fs.root.key;
        // The root's `usr` comes first, and hides any of the subvolume's.
        if root
            && cookie == 0
            && let Some(usr) = &self.fs.usr
        {
            found.push((String::from(fs::USR), usr.key.1, DT_DIR));
        }
        let hide_usr = root && self.fs.usr.is_some();
        let node = self.open.node.clone();
        let read = blocking(&self.fs.reactor, move || {
            let mut next = cookie;
            node.read_dir(&mut next, |name, inode, kind| {
                if found.len() == ENTRIES_MAX {
                    return false;
                }
                let hidden = hide_usr && name == fs::USR.as_bytes();
                if name != b"." && name != b".." && !hidden {
                    let name = String::from_utf8_lossy(name).into_owned();
                    found.push((name, inode, kind));
                }
                true
            })
            .map(|_| (found, next))
        });
        let (found, next) = read.await.map_err(error)?;
        let looked = match attributes {
            true => self.entry_attributes(&found).await?,
            false => Vec::new(),
        };
        let mut entries = Vec::with_capacity(found.len());
        for (at, (name, inode, kind)) in found.into_iter().enumerate() {
            let attrs = match attributes {
                true => match &looked[at] {
                    Some(attributes) => Some(Box::new(fs::attributes(attributes))),
                    // Removed since it was listed.
                    None => continue,
                },
                false => None,
            };
            entries.push(DirEntry {
                name,
                r#type: match kind {
                    DT_DIR => NodeType::Directory,
                    DT_LNK => NodeType::Symlink,
                    DT_SOCK => NodeType::Socket,
                    _ => NodeType::File,
                },
                inode,
                attributes: attrs,
            });
        }
        Ok(NodeReadDirectoryResponse { entries, next })
    }

    /// The attributes of the entries `found` of this directory, none for
    /// one removed since it was listed: from the dentry cache where it
    /// knows the names, and the others in one call on a thread of the
    /// pool.
    async fn entry_attributes(
        &self,
        found: &[(String, u64, u32)],
    ) -> Result<Vec<Option<BtrfsAttributes>>, Error> {
        let root = self.open.key == self.fs.root.key;
        let mut looked: Vec<Option<BtrfsAttributes>> = found.iter().map(|_| None).collect();
        let mut missed = Vec::new();
        for (at, (name, _, _)) in found.iter().enumerate() {
            if root
                && name == fs::USR
                && let Some(usr) = &self.fs.usr
            {
                looked[at] = Some(usr.attributes()?);
                continue;
            }
            match self.open.node.lookup_cached(name.as_bytes()) {
                Ok(node) => looked[at] = Some(node.attributes().map_err(error)?),
                Err(ENOENT) => {}
                Err(EAGAIN) => missed.push((at, name.clone())),
                Err(e) => return Err(error(e)),
            }
        }
        if missed.is_empty() {
            return Ok(looked);
        }
        let node = self.open.node.clone();
        let read = blocking(&self.fs.reactor, move || {
            let mut read = Vec::with_capacity(missed.len());
            for (at, name) in missed {
                let attributes = node.lookup(name.as_bytes()).and_then(|n| n.attributes());
                read.push((at, attributes));
            }
            read
        });
        for (at, attributes) in read.await {
            match attributes {
                Ok(attributes) => looked[at] = Some(attributes),
                Err(ENOENT) => {}
                Err(e) => return Err(error(e)),
            }
        }
        Ok(looked)
    }
}

/// The target of the symbolic link `open`, read on a thread of the pool
/// of `reactor`'s executor.
pub(crate) async fn link_target(open: &Open, reactor: &Reactor) -> Result<String, Error> {
    let node = open.node.clone();
    blocking(reactor, move || read_target(&node)).await
}

/// The target of the symbolic link `node`, which may read the disk.
fn read_target(node: &BtrfsNode<'static>) -> Result<String, Error> {
    let mut buf = vec![0u8; TARGET_MAX];
    let len = node.read_link(&mut buf).map_err(error)?;
    buf.truncate(len);
    String::from_utf8(buf).map_err(|_| Error::Invalid)
}

/// A node that a walk reached: the shim's node, its key, and the
/// directory and name it was found as, none for where the walk started and
/// for the root's `usr`.
#[derive(Clone)]
struct Reached {
    node: Arc<BtrfsNode<'static>>,
    key: Key,
    found: Option<(Key, String)>,
}

/// What resolving a path takes, which a thread of the pool takes as it
/// is: where it starts, the root's key and `usr`, the path and the flags
/// of `Open`.
struct Walk {
    start: Reached,
    root: Key,
    usr: Option<Reached>,
    path: String,
    flags: OpenFlags,
}

/// Where a walk ended, as `Resolved` but with the shim's nodes.
enum Walked {
    Node(Reached),
    Missing {
        parent: Reached,
        name: String,
    },
    Redirect(String),
    /// A walk from the dentry cache alone met a name that the cache does
    /// not know, a link to follow or a name to find under case folding,
    /// all of which may read the disk.
    Uncached,
}

/// A node a walk went through: its key, and the directory and name it was
/// found as.
type Named = (Key, Key, String);

/// What looking a name up in a directory gave.
enum Step {
    Found(Reached),
    Missing,
    Uncached,
}

impl Walk {
    /// Walks the path as `Connection::resolve` describes it: from the
    /// dentry cache alone when `cached`, which never waits for the disk,
    /// and otherwise from the file system. Also gives the key, directory
    /// and name of each node it went through, for the open ones to note.
    fn run(&self, cached: bool) -> Result<(Walked, Vec<Named>), Error> {
        let no_follow = self.flags.contains(OpenFlags::NO_FOLLOW);
        let folded = self.flags.contains(OpenFlags::CASE_INSENSITIVE);
        let mut rest: Vec<String> = components(&self.path)?.into_iter().rev().collect();
        let mut stack = vec![self.start.clone()];
        let mut named = Vec::new();
        let mut followed = 0;
        while let Some(name) = rest.pop() {
            let at = stack.last().expect("the stack holds the start").clone();
            match name.as_str() {
                "." => continue,
                // Only the targets of links that stay within get here.
                ".." => {
                    stack.pop();
                    continue;
                }
                _ => {}
            }
            if at.node.attributes().map_err(error)?.mode & MODE_TYPE != MODE_DIRECTORY {
                return Err(Error::NotDirectory);
            }
            let found = match self.child(&at, &name, cached)? {
                Step::Found(node) => Some(node),
                Step::Uncached => return Ok((Walked::Uncached, named)),
                Step::Missing if folded && cached => return Ok((Walked::Uncached, named)),
                Step::Missing if folded => match self.folded_child(&at, &name)? {
                    Some(_) if rest.is_empty() && self.flags.contains(OpenFlags::CREATE) => {
                        return Err(Error::Exists);
                    }
                    found => found,
                },
                Step::Missing => None,
            };
            let Some(node) = found else {
                if rest.is_empty() {
                    return Ok((Walked::Missing { parent: at, name }, named));
                }
                return Err(Error::NotFound);
            };
            if let Some((dir, found_as)) = &node.found {
                named.push((node.key, *dir, found_as.clone()));
            }
            let mode = node.node.attributes().map_err(error)?.mode;
            if mode & MODE_TYPE != MODE_SYMLINK || (rest.is_empty() && no_follow) {
                stack.push(node);
                continue;
            }
            if cached {
                return Ok((Walked::Uncached, named));
            }
            followed += 1;
            if followed > LINKS_MAX {
                return Err(Error::Loop);
            }
            let target = read_target(&node.node)?;
            if stays_within(stack.len() - 1, &target) {
                rest.extend(
                    target
                        .split('/')
                        .filter(|c| !c.is_empty())
                        .rev()
                        .map(String::from),
                );
                continue;
            }
            let mut redirect = target;
            for c in rest.iter().rev() {
                redirect.push('/');
                redirect.push_str(c);
            }
            return Ok((Walked::Redirect(redirect), named));
        }
        let end = stack.pop().expect("the stack holds the start");
        Ok((Walked::Node(end), named))
    }

    /// The node `name` of the directory `at`: the root's `usr` there, and
    /// otherwise from the dentry cache alone when `cached`.
    fn child(&self, at: &Reached, name: &str, cached: bool) -> Result<Step, Error> {
        if let Some(usr) = &self.usr
            && at.key == self.root
            && name == fs::USR
        {
            return Ok(Step::Found(usr.clone()));
        }
        let found = match cached {
            true => at.node.lookup_cached(name.as_bytes()),
            false => at.node.lookup(name.as_bytes()),
        };
        match found {
            Ok(node) => {
                let key = fs::key(&node)?;
                Ok(Step::Found(Reached {
                    node: Arc::new(node),
                    key,
                    found: Some((at.key, String::from(name))),
                }))
            }
            Err(ENOENT) => Ok(Step::Missing),
            Err(EAGAIN) => Ok(Step::Uncached),
            Err(e) => Err(error(e)),
        }
    }

    /// The first entry of the directory `at` whose name equals `name`
    /// under simple case folding, as `Connection::lookup_folded` finds it.
    fn folded_child(&self, at: &Reached, name: &str) -> Result<Option<Reached>, Error> {
        let usr = at.key == self.root && self.usr.is_some();
        let entry = match usr && mikros_casefold::equal(fs::USR, name) {
            true => Some(String::from(fs::USR)),
            false => folded_entry(&at.node, name, usr).map_err(error)?,
        };
        let Some(entry) = entry else {
            return Ok(None);
        };
        match self.child(at, &entry, false)? {
            Step::Found(node) => Ok(Some(node)),
            _ => Ok(None),
        }
    }
}

/// The first entry of the directory `node`, in the order in which
/// `ReadDirectory` lists them, whose name equals `name` under simple case
/// folding, leaving out the subvolume's own `usr` when `usr`. Names that
/// are not UTF-8 equal none.
fn folded_entry(node: &BtrfsNode<'static>, name: &str, usr: bool) -> Result<Option<String>, i32> {
    let mut cookie = 0;
    loop {
        let mut found = None;
        let mut seen = false;
        node.read_dir(&mut cookie, |entry, _, _| {
            seen = true;
            let hidden = usr && entry == fs::USR.as_bytes();
            if entry != b"."
                && entry != b".."
                && !hidden
                && let Ok(entry) = core::str::from_utf8(entry)
                && mikros_casefold::equal(entry, name)
            {
                found = Some(String::from(entry));
                return false;
            }
            true
        })?;
        if found.is_some() || !seen {
            return Ok(found);
        }
    }
}

/// Whether the relative `target` of a link at `depth` below the directory
/// that `Open` was called on stays at or below it.
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

/// The components of the path of `Open` (MIKSPEC-0049.R4).
fn components(path: &str) -> Result<Vec<String>, Error> {
    if path.is_empty() || path.starts_with('/') {
        return Err(Error::Invalid);
    }
    let path = path.strip_suffix('/').unwrap_or(path);
    let mut parts = Vec::new();
    for part in path.split('/') {
        match part {
            "" => return Err(Error::Invalid),
            ".." => return Err(Error::AccessDenied),
            p if p.len() > NAME_MAX => return Err(Error::NameTooLong),
            p => parts.push(String::from(p)),
        }
    }
    Ok(parts)
}
