//! The namespaces of new processes.
//!
//! Until profiles apply, every process gets `/`, the boot file system,
//! and `/dev`. With `root=bootfs` on the command line, profiles apply
//! once the process manager has made tmpfs's `boot/etc`, `boot/var` and
//! `boot/Users` and filled them with the boot image's `etc`, `var` and
//! `Users`. A profile's entries come from the root table: `/usr`, read
//! only and executable, and `/Apps` and `/Libraries`, read only, from the
//! boot image, and `/etc`, `/var` and `/Users` from those directories.

use crate::processes::Shared;
use alloc::format;
use alloc::rc::Rc;
use alloc::string::String;
use alloc::vec::Vec;
use core::cell::RefCell;
use mikros_rt::exec::Reactor;
use mikros_rt::interfaces::__midl::endpoints;
use mikros_rt::interfaces::mikros::base::Status;
use mikros_rt::interfaces::mikros::fs::{
    Error, Filesystem, FilesystemAsyncClient, Node, NodeAsyncClient, OpenFlags, Rights,
};
use mikros_rt::interfaces::mikros::process::NamespaceEntry;
use mikros_rt::interfaces::mikros::service::DirectoryAsyncClient;
use process_manager::bootfs::{Fs, ItemKind};

/// The protocol of the temporary file system.
const TMPFS: &str = "mikros.fs.Filesystem";
/// How long to wait before asking again for tmpfs.
const RETRY: i64 = 20_000_000;
/// The most bytes one write moves.
const CHUNK: usize = 32768;
const RW: Rights = Rights(Rights::READ.0 | Rights::WRITE.0);
const RX: Rights = Rights(Rights::READ.0 | Rights::EXECUTE.0);
const RWX: Rights = Rights(Rights::READ.0 | Rights::WRITE.0 | Rights::EXECUTE.0);

/// The directories of tmpfs that profiles use.
struct Ready {
    tmpfs: Rc<NodeAsyncClient>,
    etc: Rc<NodeAsyncClient>,
    var: Rc<NodeAsyncClient>,
    users: Rc<NodeAsyncClient>,
}

pub struct Namespaces {
    bootfs: bool,
    ready: RefCell<Option<Rc<Ready>>>,
}

/// Where an entry of the root table comes from.
enum Source {
    Boot(&'static str),
    Tmpfs(Rc<NodeAsyncClient>),
}

type Reply<T> = Result<Result<T, Error>, mikros_rt::interfaces::__midl::Error>;

fn flat<T>(reply: Reply<T>) -> Result<T, Error> {
    reply.unwrap_or(Err(Error::Io))
}

/// The status a start fails with for a file system error.
fn status(e: Error) -> Status {
    match e {
        Error::NotFound => Status::NotFound,
        Error::AccessDenied | Error::ReadOnly => Status::AccessDenied,
        Error::NoSpace => Status::NoResources,
        Error::Invalid | Error::NameTooLong => Status::InvalidArgs,
        _ => Status::Internal,
    }
}

/// Opens `path` from `dir` with `flags` and `rights`.
async fn open(
    dir: &NodeAsyncClient,
    path: &str,
    flags: OpenFlags,
    mode: u32,
    rights: Rights,
    reactor: &Reactor,
) -> Result<NodeAsyncClient, Error> {
    let (client, server) = endpoints::<Node>().map_err(|_| Error::Io)?;
    flat(dir.open(path.into(), flags, mode, rights, server).await)?;
    Ok(NodeAsyncClient::new(client, reactor.clone()))
}

/// The directory `path` below `dir`, made with its parents where they
/// are missing.
async fn directories(
    dir: &NodeAsyncClient,
    path: &str,
    reactor: &Reactor,
) -> Result<NodeAsyncClient, Error> {
    let make = OpenFlags(OpenFlags::CREATE.0 | OpenFlags::DIRECTORY.0);
    let mut at: Option<NodeAsyncClient> = None;
    for name in path.split('/').filter(|n| !n.is_empty()) {
        let parent = at.as_ref().unwrap_or(dir);
        at = Some(open(parent, name, make, 0o755, RWX, reactor).await?);
    }
    match at {
        Some(at) => Ok(at),
        None => open(dir, ".", OpenFlags::DIRECTORY, 0, RWX, reactor).await,
    }
}

/// The rights of a manifest entry: `r`, `w` and `x`, each at most once
/// and in that order.
fn rights(text: &str) -> Option<Rights> {
    let mut rights = 0;
    let mut next = 0;
    for c in text.chars() {
        let (at, bit) = match c {
            'r' => (1, Rights::READ.0),
            'w' => (2, Rights::WRITE.0),
            'x' => (3, Rights::EXECUTE.0),
            _ => return None,
        };
        if at <= next {
            return None;
        }
        next = at;
        rights |= bit;
    }
    (rights != 0).then_some(Rights(rights))
}

impl Namespaces {
    /// The namespaces of a system started with `command_line`.
    pub fn new(command_line: &str) -> Namespaces {
        let bootfs = command_line
            .split_whitespace()
            .any(|word| word == "root=bootfs");
        Namespaces {
            bootfs,
            ready: RefCell::new(None),
        }
    }

    /// Whether profiles apply to processes started under `root=bootfs`.
    pub fn runs_from_boot_image(&self) -> bool {
        self.bootfs
    }

    /// Connects to tmpfs through `directory`, the process manager's own,
    /// asking again until it serves, makes its `boot/` directories, fills
    /// them from the boot image, and from then on builds namespaces from
    /// profiles.
    pub async fn prepare(&self, directory: DirectoryAsyncClient, fs: Rc<Fs>, reactor: Reactor) {
        let root = loop {
            if let Some(root) = tmpfs_root(&directory, &reactor).await {
                break root;
            }
            let mut now = 0;
            // SAFETY: the out pointer is valid for the call.
            unsafe { mikros_rt::sys::clock_get_monotonic(&mut now) };
            let _ = reactor.sleep_until(now + RETRY).await;
        };
        match fill(&root, &fs, &reactor).await {
            Ok((etc, var, users)) => {
                let tmpfs = Rc::new(root);
                *self.ready.borrow_mut() = Some(Rc::new(Ready {
                    tmpfs,
                    etc,
                    var,
                    users,
                }));
            }
            Err(e) => mikros_rt::log!(Error, "process-manager: filling tmpfs failed: {e:?}"),
        }
    }

    /// The namespace of a new process of `app_id` and `user`, the service
    /// manager that init asked for when `service_manager`.
    pub async fn of(
        &self,
        shared: &Shared,
        app_id: &str,
        user: u32,
        service_manager: bool,
    ) -> Result<Vec<NamespaceEntry>, Status> {
        let dev = NamespaceEntry {
            path: "/dev".into(),
            directory: shared.dev.directory()?,
        };
        let ready = self.ready.borrow().clone();
        let Some(ready) = ready.filter(|_| self.bootfs) else {
            let root = crate::start::root(&shared.fs)?;
            return Ok(alloc::vec![
                NamespaceEntry {
                    path: "/".into(),
                    directory: root
                },
                dev
            ]);
        };
        let reactor = &shared.reactor;
        let mut entries = Vec::new();
        if service_manager {
            entries.push(self.entry(shared, &ready, "/usr", RX).await?);
            entries.push(self.entry(shared, &ready, "/etc", Rights::READ).await?);
        } else if app_id == "system.cli" || !app_id.starts_with("system.") {
            // Their profiles name the user, whose name is not known yet.
            return Err(Status::NotSupported);
        } else {
            let (program, listed) = manifest_entries(&shared.fs, app_id);
            if program {
                entries.push(self.entry(shared, &ready, "/usr", RX).await?);
                entries.push(self.entry(shared, &ready, "/etc", Rights::READ).await?);
                let path = format!("apps/{user}/{app_id}");
                let tmp = directories(&ready.tmpfs, &path, reactor)
                    .await
                    .map_err(status)?;
                let (directory, server) = endpoints::<Node>().map_err(|_| Status::NoResources)?;
                flat(tmp.clone(server, RW).await).map_err(status)?;
                entries.push(NamespaceEntry {
                    path: "/tmp".into(),
                    directory,
                });
                if user != 0 {
                    // The folders of the user's app need the user's name.
                    return Err(Status::NotSupported);
                }
            }
            for (path, text) in listed {
                if path.contains("<user>") {
                    match user {
                        0 => continue,
                        _ => return Err(Status::NotSupported),
                    }
                }
                let rights = rights(&text).ok_or(Status::InvalidArgs)?;
                entries.push(self.entry(shared, &ready, &path, rights).await?);
            }
        }
        entries.push(dev);
        Ok(entries)
    }

    /// The entry `path` of the root table with `rights`. A path that does
    /// not exist is made when the rights hold `WRITE` and it lies on
    /// tmpfs, and fails with `NOT_FOUND` otherwise; rights beyond those of
    /// its root fail with `ACCESS_DENIED`.
    async fn entry(
        &self,
        shared: &Shared,
        ready: &Ready,
        path: &str,
        rights: Rights,
    ) -> Result<NamespaceEntry, Status> {
        let table: [(&str, Source, Rights); 6] = [
            ("/usr", Source::Boot("usr"), RX),
            ("/Apps", Source::Boot("Apps"), Rights::READ),
            ("/Libraries", Source::Boot("Libraries"), Rights::READ),
            ("/etc", Source::Tmpfs(ready.etc.clone()), RWX),
            ("/var", Source::Tmpfs(ready.var.clone()), RWX),
            ("/Users", Source::Tmpfs(ready.users.clone()), RWX),
        ];
        let normal = path.trim_end_matches('/');
        let (source, most, rest) = table
            .into_iter()
            .find_map(|(root, source, most)| {
                let rest = normal.strip_prefix(root)?;
                (rest.is_empty() || rest.starts_with('/'))
                    .then(|| (source, most, String::from(rest.trim_start_matches('/'))))
            })
            .ok_or(Status::NotFound)?;
        if rights.0 & !most.0 != 0 {
            return Err(Status::AccessDenied);
        }
        let (client, server) = endpoints::<Node>().map_err(|_| Status::NoResources)?;
        match source {
            Source::Boot(top) => {
                let boot_path = match rest.is_empty() {
                    true => String::from(top),
                    false => format!("{top}/{rest}"),
                };
                if !shared.fs.serve_directory(server, &boot_path, rights) {
                    return Err(Status::NotFound);
                }
            }
            Source::Tmpfs(dir) => {
                let target = match rest.is_empty() {
                    true => String::from("."),
                    false => rest.clone(),
                };
                let opened = flat(
                    dir.open(target.clone(), OpenFlags::DIRECTORY, 0, rights, server)
                        .await,
                );
                match opened {
                    Ok(_) => {}
                    Err(Error::NotFound) if rights.contains(Rights::WRITE) => {
                        let made = directories(&dir, &rest, &shared.reactor)
                            .await
                            .map_err(status)?;
                        let (again, server) =
                            endpoints::<Node>().map_err(|_| Status::NoResources)?;
                        flat(made.clone(server, rights).await).map_err(status)?;
                        return Ok(NamespaceEntry {
                            path: String::from(normal),
                            directory: again,
                        });
                    }
                    Err(e) => return Err(status(e)),
                }
            }
        }
        Ok(NamespaceEntry {
            path: String::from(normal),
            directory: client,
        })
    }
}

/// The root of tmpfs, read and write, once it serves.
async fn tmpfs_root(
    directory: &DirectoryAsyncClient,
    reactor: &Reactor,
) -> Option<NodeAsyncClient> {
    let (client, server) = endpoints::<Filesystem>().ok()?;
    match directory
        .connect(TMPFS.into(), None, server.into_handle())
        .await
    {
        Ok(Ok(_)) => {}
        _ => return None,
    }
    let fs = FilesystemAsyncClient::new(client, reactor.clone());
    let (root, server) = endpoints::<Node>().ok()?;
    flat(fs.get_root(server, RWX).await).ok()?;
    Some(NodeAsyncClient::new(root, reactor.clone()))
}

type Dirs = (
    Rc<NodeAsyncClient>,
    Rc<NodeAsyncClient>,
    Rc<NodeAsyncClient>,
);

/// Makes `boot/etc`, `boot/var` and `boot/Users` on the tmpfs of `root`
/// and copies into each what the boot image holds at that path.
async fn fill(root: &NodeAsyncClient, fs: &Fs, reactor: &Reactor) -> Result<Dirs, Error> {
    let mut made = Vec::new();
    for top in ["etc", "var", "Users"] {
        let dir = directories(root, &format!("boot/{top}"), reactor).await?;
        for item in fs.subtree(top).unwrap_or_default() {
            match item.kind {
                ItemKind::Directory => {
                    let flags = OpenFlags(
                        OpenFlags::CREATE.0 | OpenFlags::DIRECTORY.0 | OpenFlags::EXCLUSIVE.0,
                    );
                    open(&dir, &item.path, flags, item.mode, RW, reactor).await?;
                }
                ItemKind::File(bytes) => {
                    let flags = OpenFlags(OpenFlags::CREATE.0 | OpenFlags::EXCLUSIVE.0);
                    let file = open(&dir, &item.path, flags, item.mode, RW, reactor).await?;
                    for (i, chunk) in bytes.chunks(CHUNK).enumerate() {
                        flat(file.write((i * CHUNK) as u64, chunk.to_vec()).await)?;
                    }
                }
                ItemKind::Symlink(target) => {
                    let (parent, name) = match item.path.rsplit_once('/') {
                        Some((parent, name)) => (Some(parent), name),
                        None => (None, item.path.as_str()),
                    };
                    let target = String::from_utf8_lossy(&target).into_owned();
                    match parent {
                        Some(parent) => {
                            let parent =
                                open(&dir, parent, OpenFlags::DIRECTORY, 0, RW, reactor).await?;
                            flat(parent.create_symlink(name.into(), target).await)?;
                        }
                        None => {
                            flat(dir.create_symlink(name.into(), target).await)?;
                        }
                    }
                }
            }
        }
        made.push(Rc::new(dir));
    }
    let users = made.pop().ok_or(Error::Io)?;
    let var = made.pop().ok_or(Error::Io)?;
    let etc = made.pop().ok_or(Error::Io)?;
    Ok((etc, var, users))
}

/// Whether `app_id` has a program manifest, and the namespace entries of
/// its manifest, a driver's, a service's or a program's, with their paths
/// and rights. Without a valid manifest it has none.
fn manifest_entries(fs: &Fs, app_id: &str) -> (bool, Vec<(String, String)>) {
    let pairs = |entries: Vec<mikros_manifest::NamespaceEntry>| {
        entries.into_iter().map(|e| (e.path, e.rights)).collect()
    };
    if let Some(name) = app_id.strip_prefix("system.driver.") {
        let path = format!("usr/lib/drivers/{name}/driver.yaml");
        return match fs
            .file(&path)
            .map(|b| mikros_manifest::parse_driver(name, b))
        {
            Some(Ok(m)) => (false, pairs(m.driver.namespace)),
            _ => (false, Vec::new()),
        };
    }
    let Some(name) = app_id.strip_prefix("system.") else {
        return (false, Vec::new());
    };
    let service = format!("{name}.yaml");
    if let Some(bytes) = fs.file(&format!("usr/lib/services/{service}")) {
        return match mikros_manifest::parse(&service, bytes) {
            Ok(m) => (false, pairs(m.service.namespace)),
            Err(_) => (false, Vec::new()),
        };
    }
    let program = format!("{app_id}.yaml");
    match fs
        .file(&format!("usr/lib/programs/{program}"))
        .map(|b| mikros_manifest::parse_program(&program, b))
    {
        Some(Ok(m)) => (true, pairs(m.program.namespace)),
        _ => (false, Vec::new()),
    }
}
