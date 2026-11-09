//! One connection to a directory of a view.

use crate::change::check_name;
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
    Missing {
        parent: Dir,
        name: String,
    },
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
            R::SetAttributes(q, r) => r.reply(s, self.set_attributes(q).await),
            R::Unlink(q, r) => r.reply(s, self.unlink(&q.name, q.directory).await),
            R::GetToken(_, r) => r.reply(s, self.token()),
            R::Rename(q, r) => r.reply(s, self.rename(q).await),
            R::Link(q, r) => r.reply(s, self.link(q).await),
            R::CreateSymlink(q, r) => r.reply(s, self.create_symlink(q).await),
            R::BindSocket(q, r) => r.reply(s, self.bind_socket(q).await),
            R::ConnectSocket(q, r) => r.reply(s, self.connect_socket(q).await),
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

    /// Checks that the connection may change the view: it needs `WRITE`,
    /// and the view a writable layer.
    fn may_change(&self) -> Result<(), Error> {
        self.require(Rights::WRITE)?;
        match self.dir.view.writable {
            true => Ok(()),
            false => Err(Error::ReadOnly),
        }
    }

    async fn set_attributes(
        &self,
        q: NodeSetAttributesRequest,
    ) -> Result<NodeSetAttributesResponse, Error> {
        self.may_change()?;
        let upper = self.dir.upper().await?;
        flat(
            upper
                .set_attributes(q.mask, q.mode, q.accessed, q.modified)
                .await,
        )
    }

    async fn unlink(&self, name: &str, directory: bool) -> Result<NodeUnlinkResponse, Error> {
        self.may_change()?;
        self.dir.remove(name, directory).await?;
        Ok(NodeUnlinkResponse {})
    }

    fn token(&self) -> Result<NodeGetTokenResponse, Error> {
        self.require(Rights::WRITE)?;
        Ok(NodeGetTokenResponse {
            token: self.dir.token()?,
        })
    }

    async fn rename(&self, q: NodeRenameRequest) -> Result<NodeRenameResponse, Error> {
        self.may_change()?;
        let to = self.dir.token_dir(&q.target).ok_or(Error::CrossDevice)?;
        self.dir.rename(&q.name, &to, &q.new_name).await?;
        Ok(NodeRenameResponse {})
    }

    async fn link(&self, q: NodeLinkRequest) -> Result<NodeLinkResponse, Error> {
        self.may_change()?;
        let to = self.dir.token_dir(&q.target).ok_or(Error::CrossDevice)?;
        self.dir.link(&q.name, &to, &q.new_name).await?;
        Ok(NodeLinkResponse {})
    }

    async fn create_symlink(
        &self,
        q: NodeCreateSymlinkRequest,
    ) -> Result<NodeCreateSymlinkResponse, Error> {
        self.may_change()?;
        check_name(&q.name)?;
        if self.dir.shows(&q.name).await? {
            return Err(Error::Exists);
        }
        let upper = self.dir.upper().await?;
        self.dir.unwhite(&upper, &q.name).await?;
        flat(upper.create_symlink(q.name, q.target).await)
    }

    async fn bind_socket(&self, q: NodeBindSocketRequest) -> Result<NodeBindSocketResponse, Error> {
        self.may_change()?;
        check_name(&q.name)?;
        if self.dir.shows(&q.name).await? {
            return Err(Error::Exists);
        }
        let upper = self.dir.upper().await?;
        self.dir.unwhite(&upper, &q.name).await?;
        flat(upper.bind_socket(q.name, q.mode, q.listener).await)
    }

    /// Connects to a socket of the writable layer; those of other layers
    /// cannot be reached.
    async fn connect_socket(
        &self,
        q: NodeConnectSocketRequest,
    ) -> Result<NodeConnectSocketResponse, Error> {
        self.may_change()?;
        match self.dir.lookup(&q.name).await? {
            Found::Missing => Err(Error::NotFound),
            Found::Other {
                layer: 0,
                kind: NodeType::Socket,
            } => {
                let upper = self.dir.upper().await?;
                flat(upper.connect_socket(q.name, q.r#type, q.connection).await)
            }
            Found::Other {
                kind: NodeType::Socket,
                ..
            } => Err(Error::NotSupported),
            _ => Err(Error::Invalid),
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
                Found::Missing if rest.is_empty() => {
                    return Ok(Resolved::Missing { parent: at, name });
                }
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
                // A file of a lower layer is copied up before it changes,
                // and the client then talks to the writable layer's server.
                let rights = Rights(q.rights.0 & self.rights.0);
                let opened = if changes {
                    let upper = parent.copy_up(&name, layer, kind).await?;
                    flat(upper.open(name, flags, q.mode, rights, q.node).await)?
                } else {
                    // The client talks to the layer's server itself.
                    let holder = parent.layers[layer].as_ref().ok_or(Error::Io)?;
                    flat(holder.open(name, flags, q.mode, rights, q.node).await)?
                };
                Ok(NodeOpenResponse {
                    r#type: if opened.redirect.is_some() {
                        opened.r#type
                    } else {
                        kind
                    },
                    redirect: opened.redirect,
                })
            }
            Resolved::Missing { parent, name } if flags.contains(OpenFlags::CREATE) => {
                check_name(&name)?;
                let rights = Rights(q.rights.0 & self.rights.0);
                if flags.contains(OpenFlags::DIRECTORY) {
                    parent.make_directory(&name, q.mode).await?;
                    let Found::Dir(made) = parent.lookup(&name).await? else {
                        return Err(Error::Io);
                    };
                    self.dir.view.spawner.spawn(serve(made, q.node, rights));
                    return Ok(NodeOpenResponse {
                        r#type: NodeType::Directory,
                        redirect: None,
                    });
                }
                let upper = parent.upper().await?;
                parent.unwhite(&upper, &name).await?;
                let opened = flat(upper.open(name, flags, q.mode, rights, q.node).await)?;
                Ok(NodeOpenResponse {
                    r#type: opened.r#type,
                    redirect: opened.redirect,
                })
            }
            Resolved::Missing { .. } => Err(Error::NotFound),
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
