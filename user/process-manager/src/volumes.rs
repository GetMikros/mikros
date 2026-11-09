//! `/Volumes`, which the process manager serves over `Node` to the
//! processes that hold `files.volumes`: one entry for each instance of
//! `device.filesystem` named `volume-<guid>`, which the device manager
//! registers for every volume but the root file system and the boot EFI
//! system partition, added and removed as the instances come and go.
//!
//! An entry is named after its file system's label, or its partition GUID
//! when the label is empty or could not be a name, being `.` or `..` or
//! holding a `/` or a NUL; when an earlier entry has that name, the
//! smallest number from 2 that makes it unique is appended after a space.
//! Opening a path
//! below an entry opens it on that volume's file system, which then serves
//! the node itself. The directory cannot be changed.

use alloc::boxed::Box;
use alloc::format;
use alloc::rc::Rc;
use alloc::string::String;
use alloc::vec::Vec;
use core::cell::RefCell;
use mikros_rt::exec::{Reactor, Spawner};
use mikros_rt::interfaces::__midl::{ClientEnd, ServerEnd, endpoints};
use mikros_rt::interfaces::mikros::base::Status;
use mikros_rt::interfaces::mikros::fs::*;
use mikros_rt::interfaces::mikros::service::{DirectoryAsyncClient, DirectoryEvent};

const DEVICE_FILESYSTEM: &str = "device.filesystem";
/// What the names of the instances of volumes start with.
const PREFIX: &str = "volume-";
/// The most entries one `ReadDirectory` reply holds.
const ENTRIES_MAX: usize = 64;
const BLOCK: u32 = 512;
const RW: Rights = Rights(Rights::READ.0 | Rights::WRITE.0);

/// A volume: its instance, its entry's name and its file system's root.
struct Volume {
    instance: String,
    name: String,
    root: Rc<NodeAsyncClient>,
}

pub struct Volumes {
    entries: RefCell<Vec<Volume>>,
    reactor: Reactor,
    spawner: Spawner,
}

impl Volumes {
    pub fn new(reactor: Reactor, spawner: Spawner) -> Rc<Volumes> {
        Rc::new(Volumes {
            entries: RefCell::new(Vec::new()),
            reactor,
            spawner,
        })
    }

    /// A new connection to the directory with `READ` and `WRITE`, which
    /// nodes opened through it may have.
    pub fn directory(self: &Rc<Self>) -> Result<ClientEnd<Node>, Status> {
        let (client, server) = endpoints::<Node>().map_err(|_| Status::NoResources)?;
        self.serve(server, RW);
        Ok(client)
    }

    fn serve(self: &Rc<Self>, end: ServerEnd<Node>, rights: Rights) {
        let volumes = self.clone();
        self.spawner
            .spawn(async move { serve(volumes, end, rights).await });
    }

    /// Adds and removes entries as the instances of volumes come and go
    /// in `directory`, the process manager's own, for as long as it is
    /// served.
    pub async fn watch(self: Rc<Self>, directory: DirectoryAsyncClient) {
        if !matches!(directory.watch(DEVICE_FILESYSTEM.into()).await, Ok(Ok(_))) {
            return;
        }
        // Instances published before the watch was set are only listed.
        if let Ok(Ok(listed)) = directory.list(DEVICE_FILESYSTEM.into()).await {
            for instance in listed.instances {
                self.add(&directory, instance).await;
            }
        }
        while let Ok(event) = directory.next_event().await {
            match event {
                DirectoryEvent::OnInstanceAdded(added) if added.protocol == DEVICE_FILESYSTEM => {
                    self.add(&directory, added.instance).await;
                }
                DirectoryEvent::OnInstanceRemoved(removed)
                    if removed.protocol == DEVICE_FILESYSTEM =>
                {
                    let instance = removed.instance;
                    self.entries.borrow_mut().retain(|v| v.instance != instance);
                }
                _ => {}
            }
        }
    }

    /// Adds the entry of `instance` when it names a volume that has none.
    async fn add(&self, directory: &DirectoryAsyncClient, instance: String) {
        let Some(guid) = instance.strip_prefix(PREFIX) else {
            return;
        };
        if self.entries.borrow().iter().any(|v| v.instance == instance) {
            return;
        }
        let Some(root) = connect(directory, &self.reactor, &instance).await else {
            mikros_rt::log!(Error, "process-manager: connecting to {instance} failed");
            return;
        };
        let label = match root.query_filesystem().await {
            Ok(Ok(queried)) => queried.info.label,
            _ => String::new(),
        };
        let mut entries = self.entries.borrow_mut();
        let taken: Vec<&str> = entries.iter().map(|v| v.name.as_str()).collect();
        let name = entry_name(&label, guid, &taken);
        mikros_rt::log!(
            Info,
            "process-manager: serving {instance} as /Volumes/{name}"
        );
        entries.push(Volume {
            instance,
            name,
            root: Rc::new(root),
        });
    }

    /// Opens `q.path` below the directory, for a connection with `rights`.
    async fn open(
        self: &Rc<Self>,
        q: NodeOpenRequest,
        rights: Rights,
    ) -> Result<NodeOpenResponse, Error> {
        if !rights.contains(q.rights) {
            return Err(Error::AccessDenied);
        }
        let mut parts = q.path.split('/').filter(|p| !p.is_empty() && *p != ".");
        let Some(first) = parts.next() else {
            if q.flags.contains(OpenFlags::CREATE | OpenFlags::EXCLUSIVE) {
                return Err(Error::Exists);
            }
            if q.flags.contains(OpenFlags::TRUNCATE) {
                return Err(Error::IsDirectory);
            }
            self.serve(q.node, q.rights);
            return Ok(NodeOpenResponse {
                r#type: NodeType::Directory,
                redirect: None,
            });
        };
        let root = self
            .entries
            .borrow()
            .iter()
            .find(|v| v.name == first)
            .map(|v| v.root.clone());
        let Some(root) = root else {
            return Err(match q.flags.contains(OpenFlags::CREATE) {
                true => write_denied(rights),
                false => Error::NotFound,
            });
        };
        let rest: Vec<&str> = parts.collect();
        let rest = match rest.is_empty() {
            true => String::from("."),
            false => rest.join("/"),
        };
        match root.open(rest, q.flags, q.mode, q.rights, q.node).await {
            Ok(Ok(opened)) => Ok(NodeOpenResponse {
                r#type: opened.r#type,
                redirect: opened.redirect,
            }),
            Ok(Err(e)) => Err(e),
            Err(_) => Err(Error::Io),
        }
    }

    /// The entries from `cookie`, at most `ENTRIES_MAX`.
    fn read_directory(
        &self,
        rights: Rights,
        cookie: u64,
        attributes: bool,
    ) -> Result<NodeReadDirectoryResponse, Error> {
        if !rights.contains(Rights::READ) {
            return Err(Error::AccessDenied);
        }
        let entries: Vec<DirEntry> = self
            .entries
            .borrow()
            .iter()
            .enumerate()
            .skip(cookie as usize)
            .take(ENTRIES_MAX)
            .map(|(i, v)| {
                let inode = i as u64 + 2;
                DirEntry {
                    name: v.name.clone(),
                    r#type: NodeType::Directory,
                    inode,
                    attributes: attributes.then(|| Box::new(directory_attributes(inode))),
                }
            })
            .collect();
        let next = cookie + entries.len() as u64;
        Ok(NodeReadDirectoryResponse { entries, next })
    }
}

/// The most bytes in an entry's name.
const NAME_MAX: usize = 255;

/// The name of a new entry for a volume with `label` and partition GUID
/// `guid`, given the names that earlier entries have. The GUID takes the
/// place of a label that cannot be a name, and of one whose name, with
/// the suffix that makes it unique, would be longer than `NAME_MAX`.
fn entry_name(label: &str, guid: &str, taken: &[&str]) -> String {
    let usable = !label.is_empty() && label != "." && label != ".." && !label.contains(['/', '\0']);
    if usable {
        let name = unique(label, taken);
        if name.len() <= NAME_MAX {
            return name;
        }
    }
    unique(guid, taken)
}

/// `base`, or, when an earlier entry has that name, `base` with a space
/// and the smallest number from 2 that no earlier entry has.
fn unique(base: &str, taken: &[&str]) -> String {
    if !taken.contains(&base) {
        return String::from(base);
    }
    (2u32..)
        .map(|n| format!("{base} {n}"))
        .find(|name| !taken.contains(&name.as_str()))
        .unwrap_or_default()
}

/// The root of the file system of the instance `instance` of
/// `device.filesystem`, read and write.
async fn connect(
    directory: &DirectoryAsyncClient,
    reactor: &Reactor,
    instance: &str,
) -> Option<NodeAsyncClient> {
    let (client, server) = endpoints::<Filesystem>().ok()?;
    let connected = directory
        .connect(
            DEVICE_FILESYSTEM.into(),
            Some(instance.into()),
            server.into_handle(),
        )
        .await;
    if !matches!(connected, Ok(Ok(_))) {
        return None;
    }
    let fs = FilesystemAsyncClient::new(client, reactor.clone());
    let (root, server) = endpoints::<Node>().ok()?;
    match fs.get_root(server, RW).await {
        Ok(Ok(_)) => Some(NodeAsyncClient::new(root, reactor.clone())),
        _ => None,
    }
}

/// The error of a change that is not allowed: without `WRITE` the right
/// is missing, and with it the directory is still read-only.
fn write_denied(rights: Rights) -> Error {
    match rights.contains(Rights::WRITE) {
        true => Error::ReadOnly,
        false => Error::AccessDenied,
    }
}

fn directory_attributes(inode: u64) -> Attributes {
    Attributes {
        r#type: NodeType::Directory,
        mode: 0o755,
        owner: 0,
        size: 0,
        allocated: 0,
        links: 2,
        inode,
        created: 0,
        modified: 0,
        accessed: 0,
        changed: 0,
    }
}

/// Serves the connection on `end` to the directory with `rights` until
/// the client closes it or sends an invalid message.
async fn serve(volumes: Rc<Volumes>, end: ServerEnd<Node>, rights: Rights) {
    use NodeRequest as R;
    let server = NodeAsyncServer::new(end, volumes.reactor.clone());
    while let Ok(request) = server.next().await {
        let s = server.server();
        // A reply that cannot be sent means that the client went away,
        // which the next read finds.
        let _ = match request {
            R::GetAttributes(_, r) => r.reply(
                s,
                Ok(NodeGetAttributesResponse {
                    attributes: directory_attributes(1),
                }),
            ),
            R::Clone(q, r) => match rights.contains(q.rights) {
                true => {
                    volumes.serve(q.node, q.rights);
                    r.reply(s, Ok(NodeCloneResponse {}))
                }
                false => r.reply(s, Err(Error::AccessDenied)),
            },
            R::Sync(_, r) => r.reply(s, Ok(NodeSyncResponse {})),
            R::QueryFilesystem(_, r) => r.reply(
                s,
                Ok(NodeQueryFilesystemResponse {
                    info: FilesystemInfo {
                        r#type: "volumes".into(),
                        block_size: BLOCK,
                        total_bytes: 0,
                        free_bytes: 0,
                        read_only: true,
                        label: String::new(),
                    },
                }),
            ),
            R::Open(q, r) => {
                let opened = volumes.open(q, rights).await;
                r.reply(s, opened)
            }
            R::ReadDirectory(q, r) => {
                r.reply(s, volumes.read_directory(rights, q.cookie, q.attributes))
            }
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
            R::Lock(_, r) => r.reply(s, Err(Error::NotSupported)),
            R::Unlock(_, r) => r.reply(s, Err(Error::NotSupported)),
            R::Watch(_, r) => r.reply(s, Err(Error::NotSupported)),
            R::Unwatch(_, r) => r.reply(s, Err(Error::NotSupported)),
            // Everything that would change the directory or its
            // attributes.
            R::SetAttributes(_, r) => r.reply(s, Err(write_denied(rights))),
            R::CloneRange(_, r) => r.reply(s, Err(write_denied(rights))),
            R::Unlink(_, r) => r.reply(s, Err(write_denied(rights))),
            R::GetToken(_, r) => r.reply(s, Err(write_denied(rights))),
            R::Rename(_, r) => r.reply(s, Err(write_denied(rights))),
            R::Link(_, r) => r.reply(s, Err(write_denied(rights))),
            R::CreateSymlink(_, r) => r.reply(s, Err(write_denied(rights))),
            R::BindSocket(_, r) => r.reply(s, Err(write_denied(rights))),
            R::ConnectSocket(_, r) => r.reply(s, Err(write_denied(rights))),
        };
    }
}
