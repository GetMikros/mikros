//! One connection to a directory of a view.

use crate::view::{Dir, Found, flat, open_in};
use alloc::string::String;
use alloc::vec::Vec;
use mikros_rt::interfaces::__midl::ServerEnd;
use mikros_rt::interfaces::mikros::fs::*;

/// The most entries one directory read returns, the limit of its reply.
const ENTRIES_MAX: usize = 64;
/// The longest path component, in bytes, and how many links one `Open`
/// follows.
const NAME_MAX: usize = 255;
const LINKS_MAX: usize = 40;

struct Connection {
    dir: Dir,
    rights: Rights,
}

/// Serves the connection on `end` to `dir` with `rights`, at most the
/// view's, until the client closes it.
pub async fn serve(dir: Dir, end: ServerEnd<Node>, rights: Rights) {
    let rights = Rights(rights.0 & dir.view.rights.0);
    let server = NodeAsyncServer::new(end, dir.view.reactor.clone());
    let connection = Connection { dir, rights };
    while let Ok(request) = server.next().await {
        connection.handle(request, server.server()).await;
    }
}

/// Where a path leads in a view.
enum Resolved {
    Dir(Dir),
    /// A name in `parent` that a layer holds, as something other than a
    /// directory.
    Other {
        parent: Dir,
        name: String,
        layer: usize,
        kind: NodeType,
    },
    Missing,
    Redirect(String),
}

impl Connection {
    async fn handle(&self, request: NodeRequest, s: &NodeServer) {
        use NodeRequest as R;
        // A reply that cannot be sent means that the client went away,
        // which the next read finds.
        let _ = match request {
            R::GetAttributes(_, r) => r.reply(s, self.attributes().await),
            R::Clone(q, r) => r.reply(s, self.clone(q)),
            R::Sync(_, r) => r.reply(s, Ok(NodeSyncResponse {})),
            R::QueryFilesystem(_, r) => r.reply(s, self.filesystem().await),
            R::ReadDirectory(q, r) => r.reply(s, self.read_directory(q.cookie, q.attributes).await),
            R::Open(q, r) => r.reply(s, self.open(q).await),
            R::Read(_, r) => r.reply(s, Err(Error::IsDirectory)),
            R::ReadNext(_, r) => r.reply(s, Err(Error::IsDirectory)),
            R::Seek(_, r) => r.reply(s, Err(Error::Invalid)),
            R::ReadLink(_, r) => r.reply(s, Err(Error::Invalid)),
            R::Write(_, r) => r.reply(s, Err(Error::IsDirectory)),
            R::Append(_, r) => r.reply(s, Err(Error::IsDirectory)),
            R::WriteNext(_, r) => r.reply(s, Err(Error::IsDirectory)),
            R::Truncate(_, r) => r.reply(s, Err(Error::IsDirectory)),
            R::GetMemoryObject(_, r) => r.reply(s, Err(Error::IsDirectory)),
            R::GetRevocableMemoryObject(_, r) => r.reply(s, Err(Error::IsDirectory)),
            R::Lock(_, r) => r.reply(s, Err(Error::IsDirectory)),
            R::Unlock(_, r) => r.reply(s, Err(Error::IsDirectory)),
            R::CloneRange(_, r) => r.reply(s, Err(Error::IsDirectory)),
            // Changes to directories, which views do not make yet.
            R::SetAttributes(_, r) => r.reply(s, Err(self.change_denied())),
            R::Unlink(_, r) => r.reply(s, Err(self.change_denied())),
            R::GetToken(_, r) => r.reply(s, Err(self.change_denied())),
            R::Rename(_, r) => r.reply(s, Err(self.change_denied())),
            R::Link(_, r) => r.reply(s, Err(self.change_denied())),
            R::CreateSymlink(_, r) => r.reply(s, Err(self.change_denied())),
            R::BindSocket(_, r) => r.reply(s, Err(self.change_denied())),
            R::ConnectSocket(_, r) => r.reply(s, Err(self.change_denied())),
            R::Watch(_, r) => r.reply(s, Err(Error::NotSupported)),
            R::Unwatch(_, r) => r.reply(s, Err(Error::NotSupported)),
        };
    }

    fn require(&self, rights: Rights) -> Result<(), Error> {
        match self.rights.contains(rights) {
            true => Ok(()),
            false => Err(Error::AccessDenied),
        }
    }

    /// The error of a change: without `WRITE` the right is missing, a
    /// view without a writable layer is read-only, and views do not
    /// change directories yet.
    fn change_denied(&self) -> Error {
        if !self.rights.contains(Rights::WRITE) {
            Error::AccessDenied
        } else if !self.dir.view.writable {
            Error::ReadOnly
        } else {
            Error::NotSupported
        }
    }

    async fn attributes(&self) -> Result<NodeGetAttributesResponse, Error> {
        let (_, first) = self.dir.first().ok_or(Error::NotFound)?;
        flat(first.get_attributes().await)
    }

    async fn filesystem(&self) -> Result<NodeQueryFilesystemResponse, Error> {
        let (_, first) = self.dir.first().ok_or(Error::NotFound)?;
        let mut info = flat(first.query_filesystem().await)?;
        info.info.read_only = !self.dir.view.writable;
        Ok(info)
    }

    fn clone(&self, q: NodeCloneRequest) -> Result<NodeCloneResponse, Error> {
        self.require(q.rights)?;
        let task = serve(self.dir.clone(), q.node, q.rights);
        self.dir.view.spawner.spawn(task);
        Ok(NodeCloneResponse {})
    }

    async fn read_directory(
        &self,
        cookie: u64,
        attributes: bool,
    ) -> Result<NodeReadDirectoryResponse, Error> {
        self.require(Rights::READ)?;
        let entries = self.dir.entries(attributes).await?;
        let from = usize::try_from(cookie)
            .unwrap_or(usize::MAX)
            .min(entries.len());
        let page: Vec<DirEntry> = entries.into_iter().skip(from).take(ENTRIES_MAX).collect();
        let next = (from + page.len()) as u64;
        Ok(NodeReadDirectoryResponse {
            entries: page,
            next,
        })
    }

    /// Resolves `path` from this directory as 0049 asks of a server:
    /// links whose relative targets stay at or below it are followed,
    /// found in whichever layer holds them, and any other comes back as
    /// a redirect.
    async fn resolve(&self, path: &str, no_follow: bool) -> Result<Resolved, Error> {
        if path.is_empty() || path.starts_with('/') {
            return Err(Error::Invalid);
        }
        let path = path.strip_suffix('/').unwrap_or(path);
        let mut rest: Vec<String> = Vec::new();
        for c in path.split('/') {
            match c {
                "" => return Err(Error::Invalid),
                ".." => return Err(Error::AccessDenied),
                c if c.len() > NAME_MAX => return Err(Error::NameTooLong),
                c => rest.push(String::from(c)),
            }
        }
        rest.reverse();
        let mut stack: Vec<Dir> = alloc::vec![self.dir.clone()];
        let mut followed = 0;
        while let Some(name) = rest.pop() {
            match name.as_str() {
                "." => continue,
                // Only targets that stay within the directory get here.
                ".." => {
                    stack.pop();
                    continue;
                }
                _ => {}
            }
            let at = stack.last().expect("the stack holds the directory").clone();
            match at.lookup(&name).await? {
                Found::Dir(dir) => stack.push(dir),
                Found::Missing if rest.is_empty() => return Ok(Resolved::Missing),
                Found::Missing => return Err(Error::NotFound),
                Found::Other { layer, kind }
                    if kind != NodeType::Symlink || (rest.is_empty() && no_follow) =>
                {
                    if !rest.is_empty() {
                        return Err(Error::NotDirectory);
                    }
                    return Ok(Resolved::Other {
                        parent: at,
                        name,
                        layer,
                        kind,
                    });
                }
                Found::Other { layer, .. } => {
                    followed += 1;
                    if followed > LINKS_MAX {
                        return Err(Error::Loop);
                    }
                    let holder = at.layers[layer].as_ref().ok_or(Error::Io)?;
                    let (link, _) = open_in(holder, &name, &at.view.reactor).await?;
                    let target = flat(link.read_link().await)?.target;
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
            }
        }
        Ok(Resolved::Dir(
            stack.pop().expect("the stack holds the directory"),
        ))
    }

    async fn open(&self, q: NodeOpenRequest) -> Result<NodeOpenResponse, Error> {
        self.require(q.rights)?;
        let flags = q.flags;
        let changes = q.rights.contains(Rights::WRITE)
            || flags.contains(OpenFlags::CREATE)
            || flags.contains(OpenFlags::TRUNCATE)
            || flags.contains(OpenFlags::APPEND);
        if changes && !self.dir.view.writable {
            return Err(Error::ReadOnly);
        }
        let want_dir = flags.contains(OpenFlags::DIRECTORY) || q.path.ends_with('/');
        let exclusive = flags.contains(OpenFlags::CREATE) && flags.contains(OpenFlags::EXCLUSIVE);
        match self
            .resolve(&q.path, flags.contains(OpenFlags::NO_FOLLOW))
            .await?
        {
            Resolved::Redirect(target) => Ok(NodeOpenResponse {
                r#type: NodeType::Symlink,
                redirect: Some(target),
            }),
            Resolved::Dir(_) if exclusive => Err(Error::Exists),
            Resolved::Dir(dir) => {
                if flags.contains(OpenFlags::TRUNCATE) {
                    return Err(Error::IsDirectory);
                }
                self.dir.view.spawner.spawn(serve(dir, q.node, q.rights));
                Ok(NodeOpenResponse {
                    r#type: NodeType::Directory,
                    redirect: None,
                })
            }
            Resolved::Other { .. } if exclusive => Err(Error::Exists),
            Resolved::Other { .. } if want_dir => Err(Error::NotDirectory),
            Resolved::Other {
                parent,
                name,
                layer,
                kind,
            } => {
                // A file of a lower layer must be copied up before it
                // changes, which views do not do yet.
                if changes && layer != 0 {
                    return Err(Error::NotSupported);
                }
                // The client talks to the layer's server itself.
                let holder = parent.layers[layer].as_ref().ok_or(Error::Io)?;
                let rights = Rights(q.rights.0 & self.rights.0);
                let opened = flat(holder.open(name, flags, q.mode, rights, q.node).await)?;
                Ok(NodeOpenResponse {
                    r#type: if opened.redirect.is_some() {
                        opened.r#type
                    } else {
                        kind
                    },
                    redirect: opened.redirect,
                })
            }
            Resolved::Missing if flags.contains(OpenFlags::CREATE) => Err(Error::NotSupported),
            Resolved::Missing => Err(Error::NotFound),
        }
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
