//! `CommandOwner` and `Commands` (MIKSPEC-0101.R43): the commands of
//! installed packages, found in their manifests, and the folder of
//! MIKSPEC-0101.R44, which lists those that a connection's user sees and
//! opens the package's file itself for a name, bound to the connection's
//! subject once it has one.

use super::files::{Root, Txn, user_name};
use super::{FOREVER, Shared};
use crate::commands::{self, Command, Provided};
use crate::place::{self, Scope};
use crate::records::Owner;
use mikros_mika::format::MANIFEST;
use mikros_mika::manifest::Manifest;
use mikros_rt::interfaces::__midl::{ServerEnd, endpoints};
use mikros_rt::interfaces::mikros::base::Status;
use mikros_rt::interfaces::mikros::fs::{
    Attributes, DirEntry, Error, FilesystemInfo, Node, NodeClient, NodeCloneResponse,
    NodeGetAttributesResponse, NodeOpenRequest, NodeOpenResponse, NodeQueryFilesystemResponse,
    NodeReadDirectoryResponse, NodeRequest, NodeRestrictResponse, NodeServer, NodeSyncResponse,
    NodeType, OpenFlags, Rights,
};
use mikros_rt::interfaces::mikros::service::ClientIdentity;
use std::collections::BTreeMap;
use std::sync::Arc;

/// The only caller of `CommandOwner` and `Commands`.
const PROCESS_MANAGER: &str = "system.processmgr";
/// What the folder may be read, searched and run with.
const RX: Rights = Rights(Rights::READ.0 | Rights::EXECUTE.0);
/// The most entries that one `ReadDirectory` returns.
const ENTRIES_MAX: usize = 32;

/// The process that a connection is bound to.
#[derive(Clone, Copy)]
struct Subject {
    user: u32,
    app: u32,
    pid: u32,
    koid: u64,
    admin: u32,
}

/// The commands that the packages of `scope` have.
pub fn provided(root: &Root, scope: &Scope) -> Result<Provided, Status> {
    let mut found = Provided::new();
    for (folder, kind, _) in place::folders(scope) {
        let ids = match root.folders(&folder) {
            Ok(ids) => ids,
            Err(Status::NotFound) => continue,
            Err(e) => return Err(e),
        };
        for id in ids.into_iter().filter(|id| mikros_mika::id::is_valid(id)) {
            let Some(at) = place::subvolume(kind, &id, scope) else {
                continue;
            };
            let Ok(bytes) = root.read(&format!("{at}/{MANIFEST}")) else {
                continue;
            };
            let Ok(manifest) = Manifest::parse(&bytes) else {
                continue;
            };
            for bin in manifest.bins {
                found.entry(bin.name).or_default().push(Command {
                    id: id.clone(),
                    file: format!("{at}/{}", bin.path),
                });
            }
        }
    }
    Ok(found)
}

/// The commands that `user` sees, or, for `None`, those of every user.
fn visible(shared: &Shared, user: Option<u32>) -> Result<BTreeMap<String, Command>, Status> {
    let root = shared.root()?;
    let owners = root.owners()?;
    let global = owners.resolve(Owner::Global, &provided(&root, &Scope::Global)?);
    let local = match user.filter(|u| *u != 0) {
        Some(user) => {
            let directory = shared.directory.lock().map_err(|_| Status::Internal)?;
            let name = user_name(&directory, user)?;
            drop(directory);
            owners.resolve(Owner::User(user), &provided(&root, &Scope::Local(name))?)
        }
        None => BTreeMap::new(),
    };
    Ok(commands::visible(global, local))
}

/// Records who keeps the commands of `manifest_id`, installed in `scope`
/// for `owner`, that other packages there have: the package itself for
/// those in `taken`. `others` are the commands of the scope's other
/// packages before the install.
pub fn installed(
    txn: &Txn<'_>,
    owner: Owner,
    id: &str,
    names: &[String],
    taken: &[String],
    others: &Provided,
) -> Result<(), Status> {
    txn.update_owners(|owners| owners.install(owner, id, names, taken, others))
}

/// The commands of `scope` but those of the package `id`.
pub fn others(root: &Root, scope: &Scope, id: &str) -> Result<Provided, Status> {
    let mut found = provided(root, scope)?;
    for providers in found.values_mut() {
        providers.retain(|c| c.id != id);
    }
    Ok(found)
}

/// Forgets the commands that the package `id` of `owner` owned.
pub fn removed(txn: &Txn<'_>, owner: Owner, id: &str) -> Result<(), Status> {
    txn.update_owners(|owners| owners.forget(owner, id))
}

/// The ID of the package whose command `name` the user `user` sees, for
/// the process manager only.
pub fn command_owner(
    shared: &Shared,
    caller: &ClientIdentity,
    name: &str,
    user: u32,
) -> Result<String, Status> {
    if caller.app_id != PROCESS_MANAGER {
        return Err(Status::AccessDenied);
    }
    visible(shared, Some(user))?
        .remove(name)
        .map(|c| c.id)
        .ok_or(Status::NotFound)
}

/// Serves the folder on `server`, for the process manager only.
pub fn commands(
    shared: &Arc<Shared>,
    caller: &ClientIdentity,
    server: ServerEnd<Node>,
) -> Result<(), Status> {
    if caller.app_id != PROCESS_MANAGER {
        return Err(Status::AccessDenied);
    }
    spawn(shared.clone(), server, RX, None);
    Ok(())
}

/// Serves a connection to the folder with `rights` for `subject` on a
/// thread of its own.
fn spawn(shared: Arc<Shared>, end: ServerEnd<Node>, rights: Rights, subject: Option<Subject>) {
    std::thread::spawn(move || serve(&shared, end, rights, subject));
}

fn folder_attributes() -> Attributes {
    Attributes {
        r#type: NodeType::Directory,
        mode: 0o555,
        owner: 0,
        size: 0,
        allocated: 0,
        links: 2,
        inode: 1,
        created: 0,
        modified: 0,
        accessed: 0,
        changed: 0,
    }
}

/// The error of what the folder never does.
fn read_only(rights: Rights) -> Error {
    match rights.contains(Rights::WRITE) {
        true => Error::ReadOnly,
        false => Error::AccessDenied,
    }
}

fn error(status: Status) -> Error {
    match status {
        Status::NotFound => Error::NotFound,
        Status::AccessDenied => Error::AccessDenied,
        _ => Error::Io,
    }
}

/// Opens the folder itself or a command in it: the command's file in its
/// package, with at most read and execute rights, bound to `subject`.
fn open(
    shared: &Arc<Shared>,
    q: NodeOpenRequest,
    rights: Rights,
    subject: Option<Subject>,
) -> Result<NodeOpenResponse, Error> {
    let changes = OpenFlags(OpenFlags::CREATE.0 | OpenFlags::TRUNCATE.0 | OpenFlags::APPEND.0);
    if q.flags.0 & changes.0 != 0 || !rights.contains(q.rights) {
        return Err(read_only(q.rights));
    }
    let mut parts = q.path.split('/').filter(|p| !p.is_empty() && *p != ".");
    let Some(name) = parts.next() else {
        spawn(shared.clone(), q.node, q.rights, subject);
        return Ok(NodeOpenResponse {
            r#type: NodeType::Directory,
            redirect: None,
        });
    };
    let user = subject.map(|s| s.user);
    let command = visible(shared, user)
        .map_err(error)?
        .remove(name)
        .ok_or(Error::NotFound)?;
    if parts.next().is_some() || q.flags.contains(OpenFlags::DIRECTORY) {
        return Err(Error::NotDirectory);
    }
    let root = shared.root().map_err(error)?;
    // The package's file itself, never a link to it.
    let flags = OpenFlags(q.flags.0 | OpenFlags::NO_FOLLOW.0);
    let Some(subject) = subject else {
        let opened = root
            .open_on(&command.file, flags, q.rights, q.node)
            .map_err(error)?;
        return Ok(NodeOpenResponse {
            r#type: opened.r#type,
            redirect: None,
        });
    };
    // Opened for the install service, then bound to the subject, whose
    // access the file system checks.
    let (node, server) = endpoints::<Node>().map_err(|_| Error::Io)?;
    let opened = root
        .open_on(&command.file, flags, q.rights, server)
        .map_err(error)?;
    let node = NodeClient::new(node);
    let Subject {
        user,
        app,
        pid,
        koid,
        admin,
    } = subject;
    match node.restrict(user, app, pid, koid, admin, q.node, FOREVER) {
        Ok(Ok(_)) => Ok(NodeOpenResponse {
            r#type: opened.r#type,
            redirect: None,
        }),
        Ok(Err(e)) => Err(e),
        Err(_) => Err(Error::Io),
    }
}

/// The commands from `cookie`, at most `ENTRIES_MAX`, with their files'
/// attributes when `attributes`.
fn read_directory(
    shared: &Shared,
    rights: Rights,
    subject: Option<Subject>,
    cookie: u64,
    attributes: bool,
) -> Result<NodeReadDirectoryResponse, Error> {
    if !rights.contains(Rights::READ) {
        return Err(Error::AccessDenied);
    }
    let seen = visible(shared, subject.map(|s| s.user)).map_err(error)?;
    let root = shared.root().map_err(error)?;
    let entries: Vec<DirEntry> = seen
        .into_iter()
        .enumerate()
        .skip(cookie as usize)
        .take(ENTRIES_MAX)
        .map(|(i, (name, command))| {
            let found = attributes
                .then(|| root.open(&command.file, OpenFlags(0), 0, Rights::READ).ok())
                .flatten()
                .and_then(|file| file.get_attributes(FOREVER).ok()?.ok())
                .map(|a| Box::new(a.attributes));
            DirEntry {
                name,
                r#type: NodeType::File,
                inode: i as u64 + 2,
                attributes: found,
            }
        })
        .collect();
    let next = cookie + entries.len() as u64;
    Ok(NodeReadDirectoryResponse { entries, next })
}

/// Serves the connection on `end` to the folder with `rights` and
/// `subject` until the client closes it.
fn serve(shared: &Arc<Shared>, end: ServerEnd<Node>, rights: Rights, subject: Option<Subject>) {
    use NodeRequest as R;
    let server = NodeServer::new(end);
    while let Ok(request) = server.next(FOREVER) {
        let s = &server;
        // A reply that cannot be sent means that the client went away,
        // which the next read finds.
        let _ = match request {
            R::GetAttributes(_, r) => r.reply(
                s,
                Ok(NodeGetAttributesResponse {
                    attributes: folder_attributes(),
                }),
            ),
            R::Clone(q, r) => match rights.contains(q.rights) {
                true => {
                    spawn(shared.clone(), q.node, q.rights, subject);
                    r.reply(s, Ok(NodeCloneResponse {}))
                }
                false => r.reply(s, Err(Error::AccessDenied)),
            },
            R::Sync(_, r) => r.reply(s, Ok(NodeSyncResponse {})),
            R::QueryFilesystem(_, r) => r.reply(
                s,
                Ok(NodeQueryFilesystemResponse {
                    info: FilesystemInfo {
                        r#type: "commands".into(),
                        block_size: 4096,
                        total_bytes: 0,
                        free_bytes: 0,
                        read_only: true,
                        label: String::new(),
                        id: String::new(),
                    },
                }),
            ),
            R::Open(q, r) => r.reply(s, open(shared, q, rights, subject)),
            R::ReadDirectory(q, r) => r.reply(
                s,
                read_directory(shared, rights, subject, q.cookie, q.attributes),
            ),
            R::Restrict(q, r) => r.reply(
                s,
                match subject {
                    Some(_) => Err(Error::BadState),
                    None => {
                        let subject = Subject {
                            user: q.user,
                            app: q.app,
                            pid: q.pid,
                            koid: q.koid,
                            admin: q.admin,
                        };
                        spawn(shared.clone(), q.node, rights, Some(subject));
                        Ok(NodeRestrictResponse {})
                    }
                },
            ),
            R::Read(_, r) => r.reply(s, Err(Error::IsDirectory)),
            R::ReadNext(_, r) => r.reply(s, Err(Error::IsDirectory)),
            R::Write(_, r) => r.reply(s, Err(Error::IsDirectory)),
            R::WriteNext(_, r) => r.reply(s, Err(Error::IsDirectory)),
            R::Append(_, r) => r.reply(s, Err(Error::IsDirectory)),
            R::Truncate(_, r) => r.reply(s, Err(Error::IsDirectory)),
            R::Seek(_, r) => r.reply(s, Err(Error::Invalid)),
            R::ReadLink(_, r) => r.reply(s, Err(Error::Invalid)),
            R::GetMemoryObject(_, r) => r.reply(s, Err(Error::IsDirectory)),
            R::GetRevocableMemoryObject(_, r) => r.reply(s, Err(Error::IsDirectory)),
            R::GetAcl(_, r) => r.reply(s, Err(Error::NotSupported)),
            R::Lock(_, r) => r.reply(s, Err(Error::NotSupported)),
            R::Unlock(_, r) => r.reply(s, Err(Error::NotSupported)),
            R::Watch(_, r) => r.reply(s, Err(Error::NotSupported)),
            R::Unwatch(_, r) => r.reply(s, Err(Error::NotSupported)),
            // Everything that would change the folder.
            R::SetOwner(_, r) => r.reply(s, Err(read_only(rights))),
            R::SetAcl(_, r) => r.reply(s, Err(read_only(rights))),
            R::SetAttributes(_, r) => r.reply(s, Err(read_only(rights))),
            R::CloneRange(_, r) => r.reply(s, Err(read_only(rights))),
            R::Unlink(_, r) => r.reply(s, Err(read_only(rights))),
            R::GetToken(_, r) => r.reply(s, Err(read_only(rights))),
            R::Rename(_, r) => r.reply(s, Err(read_only(rights))),
            R::Link(_, r) => r.reply(s, Err(read_only(rights))),
            R::CreateSymlink(_, r) => r.reply(s, Err(read_only(rights))),
            R::BindSocket(_, r) => r.reply(s, Err(read_only(rights))),
            R::ConnectSocket(_, r) => r.reply(s, Err(read_only(rights))),
            R::Exchange(_, r) => r.reply(s, Err(read_only(rights))),
        };
    }
}
