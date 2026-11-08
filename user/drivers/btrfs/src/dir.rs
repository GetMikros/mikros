// SPDX-License-Identifier: GPL-2.0
//! Resolving paths (MIKSPEC-0049.R4, R5), opening nodes
//! (MIKSPEC-0049.R6), and listing directories (MIKSPEC-0049.R12).

use crate::fs::{self, Open, error};
use crate::node::{Connection, NAME_MAX, TARGET_MAX};
use alloc::boxed::Box;
use alloc::rc::Rc;
use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;
use mikros_btrfs::node::{MODE_SYMLINK, MODE_TYPE};
use mikros_rt::interfaces::mikros::fs::*;

/// The most entries one directory read returns, the limit of its reply.
const ENTRIES_MAX: usize = 64;
/// The most symbolic links one `Open` follows.
const LINKS_MAX: usize = 40;
/// The `DT_` types of directory entries.
const DT_DIR: u32 = 4;
const DT_LNK: u32 = 10;
const DT_SOCK: u32 = 12;

/// What a path names.
enum Resolved {
    Node(Rc<Open>),
    /// Nothing, as the last component of a path in the directory.
    Missing,
    /// A link to follow from elsewhere, with what remains of the path.
    Redirect(String),
}

impl Connection {
    /// Looks `name` up in the directory `dir`, as the open node that
    /// connections share.
    pub(crate) fn lookup(&self, dir: &Rc<Open>, name: &str) -> Result<Option<Rc<Open>>, Error> {
        match dir.node.lookup(name.as_bytes()) {
            Ok(node) => Ok(Some(self.fs.open(node)?)),
            Err(-2) => Ok(None),
            Err(e) => Err(error(e)),
        }
    }

    /// Resolves `path` from this directory, following a symbolic link
    /// last in the path unless `no_follow`. Links whose relative targets
    /// stay at or below this directory are followed; for any other, the
    /// link's target joined with the rest of the path comes back as a
    /// redirect.
    fn resolve(&self, path: &str, no_follow: bool) -> Result<Resolved, Error> {
        let mut rest: Vec<String> = components(path)?.into_iter().rev().collect();
        let mut stack = vec![self.open.clone()];
        let mut followed = 0;
        while let Some(name) = rest.pop() {
            let at = stack
                .last()
                .expect("the stack holds this directory")
                .clone();
            match name.as_str() {
                "." => continue,
                // Only the targets of links that stay within get here.
                ".." => {
                    stack.pop();
                    continue;
                }
                _ => {}
            }
            if !at.is_directory()? {
                return Err(Error::NotDirectory);
            }
            let Some(node) = self.lookup(&at, &name)? else {
                if rest.is_empty() {
                    return Ok(Resolved::Missing);
                }
                return Err(Error::NotFound);
            };
            if node.attributes()?.mode & MODE_TYPE != MODE_SYMLINK || (rest.is_empty() && no_follow)
            {
                stack.push(node);
                continue;
            }
            followed += 1;
            if followed > LINKS_MAX {
                return Err(Error::Loop);
            }
            let target = link_target(&node)?;
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
            return Ok(Resolved::Redirect(redirect));
        }
        Ok(Resolved::Node(
            stack.pop().expect("the stack holds this directory"),
        ))
    }

    pub(crate) fn open(&self, q: NodeOpenRequest) -> Result<NodeOpenResponse, Error> {
        self.require(q.rights)?;
        if q.rights.contains(Rights::WRITE) && self.fs.read_only {
            return Err(Error::ReadOnly);
        }
        if q.flags.contains(OpenFlags::CASE_INSENSITIVE) {
            return Err(Error::NotSupported);
        }
        self.directory()?;
        let want_dir = q.flags.contains(OpenFlags::DIRECTORY) || q.path.ends_with('/');
        let open = match self.resolve(&q.path, q.flags.contains(OpenFlags::NO_FOLLOW))? {
            Resolved::Redirect(target) => {
                return Ok(NodeOpenResponse {
                    r#type: NodeType::Symlink,
                    redirect: Some(target),
                });
            }
            // Creating comes later.
            Resolved::Missing if q.flags.contains(OpenFlags::CREATE) => {
                return Err(Error::NotSupported);
            }
            Resolved::Missing => return Err(Error::NotFound),
            Resolved::Node(_) if q.flags.contains(OpenFlags::CREATE | OpenFlags::EXCLUSIVE) => {
                return Err(Error::Exists);
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
            return Err(Error::NotSupported);
        }
        let append = q.flags.contains(OpenFlags::APPEND);
        self.fs.serve(q.node, open, q.rights, self.user, append);
        Ok(NodeOpenResponse {
            r#type,
            redirect: None,
        })
    }

    pub(crate) fn read_directory(
        &self,
        cookie: u64,
        attributes: bool,
    ) -> Result<NodeReadDirectoryResponse, Error> {
        self.require(Rights::READ)?;
        self.directory()?;
        let mut found: Vec<(String, u64, u32)> = Vec::new();
        let mut next = cookie;
        self.open
            .node
            .read_dir(&mut next, |name, inode, kind| {
                if found.len() == ENTRIES_MAX {
                    return false;
                }
                if name != b"." && name != b".." {
                    let name = String::from_utf8_lossy(name).into_owned();
                    found.push((name, inode, kind));
                }
                true
            })
            .map_err(error)?;
        let mut entries = Vec::with_capacity(found.len());
        for (name, inode, kind) in found {
            let attrs = match attributes {
                true => match self.lookup(&self.open, &name)? {
                    Some(open) => Some(Box::new(fs::attributes(&open.attributes()?))),
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
}

/// The target of the symbolic link `open`.
pub(crate) fn link_target(open: &Open) -> Result<String, Error> {
    let mut buf = vec![0u8; TARGET_MAX];
    let len = open.node.read_link(&mut buf).map_err(error)?;
    buf.truncate(len);
    String::from_utf8(buf).map_err(|_| Error::Invalid)
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
