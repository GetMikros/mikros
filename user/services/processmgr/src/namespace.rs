//! The namespaces of new processes.
//!
//! Until profiles apply, every process gets `/`, the boot file system,
//! and `/dev`. With `root=bootfs` on the command line, profiles apply
//! once the process manager has made tmpfs's `boot/etc`, `boot/var` and
//! `boot/Users` and filled them with the boot image's `etc`, `var` and
//! `Users`. Once tmpfs has started, the namespace of every other new
//! process waits for that, so that only the processes started before
//! tmpfs, and tmpfs itself, go without their profiles. A profile's
//! entries come from the root table: `/usr`, read only and executable,
//! and `/Apps` and `/Libraries`, read only, from the boot image, and
//! `/etc`, `/var` and `/Users` from those directories.
//!
//! Otherwise profiles apply once the process manager has connected to
//! the root file system, the instance `root` of `device.filesystem`
//! (MIKSPEC-0050.R8, MIKSPEC-0050.R29): the root table's entries then
//! come from the root file system, and `/tmp` from tmpfs, for which an
//! entry waits until tmpfs serves.
//! Processes started before go without their profiles. An entry that is
//! the root file system's root or lies in `@system` fails the start with
//! `ACCESS_DENIED` (MIKSPEC-0050.R28).
//!
//! The process manager also connects to the file system of the EFI
//! system partition that the system booted from, the instance `esp` of
//! `device.filesystem`, once it is published, and from then on gives it
//! as `/boot/efi` to the processes whose manifests list it
//! (MIKSPEC-0064.R9, MIKSPEC-0064.R15). Processes whose profiles apply and
//! that hold `files.volumes` get `/Volumes`, read and write, and so do the
//! command-line tools of `system.cli`, whose home folder may also hold
//! programs.

use crate::processes::Shared;
use alloc::format;
use alloc::rc::Rc;
use alloc::string::String;
use alloc::vec::Vec;
use core::cell::{Cell, RefCell};
use core::future::poll_fn;
use core::task::{Poll, Waker};
use mikros_manifest::path;
use mikros_rt::exec::Reactor;
use mikros_rt::files;
use mikros_rt::interfaces::__midl::{OwnedHandle, endpoints};
use mikros_rt::interfaces::mikros::base::Status;
use mikros_rt::interfaces::mikros::fs::{
    Error, Filesystem, FilesystemAsyncClient, Node, NodeAsyncClient, OpenFlags, Rights,
    TemporaryFilesystem, TemporaryFilesystemAsyncClient,
};
use mikros_rt::interfaces::mikros::process::NamespaceEntry;
use mikros_rt::interfaces::mikros::service::{
    ClientIdentity, DirectoryAsyncClient, DirectoryEvent,
};
use processmgr::bootfs::{Fs, ItemKind};

/// The accounts service's data directory, which no other process's
/// manifest entry may be, lie in, or hold.
const ACCOUNTS_DATA: &str = "/var/lib/accounts";
const ACCOUNTS_APP: &str = "system.accounts";
/// The app ID of the temporary file system service.
const TMPFS_APP: &str = "system.tmpfs";
/// The app ID of command-line tools.
const CLI_APP: &str = "system.cli";
/// The protocols of the temporary file system.
const TMPFS: &str = "mikros.fs.Filesystem";
const TEMPORARY: &str = "mikros.fs.TemporaryFilesystem";
/// The protocol of the file systems of partitions, and the instance of
/// the root file system.
const DEVICE_FILESYSTEM: &str = "device.filesystem";
const ROOT_INSTANCE: &str = "root";
/// The subvolume of system versions, which no namespace may reach.
const SYSTEM: &str = "/@system";
/// Where the EFI system partition is served, and its instance.
const BOOT_EFI: &str = "/boot/efi";
/// The directory of the other volumes, and the permission it needs.
const VOLUMES: &str = "/Volumes";
const VOLUMES_PERMISSION: &str = "files.volumes";
const ESP_INSTANCE: &str = "esp";
/// How long to wait before asking again for tmpfs.
const RETRY: i64 = 20_000_000;
/// The most bytes one write moves.
const CHUNK: usize = 32768;
/// The largest manifest read from the root file system.
const USR_FILE_MAX: usize = 1 << 16;
const RW: Rights = Rights(Rights::READ.0 | Rights::WRITE.0);
const RX: Rights = Rights(Rights::READ.0 | Rights::EXECUTE.0);
const RWX: Rights = Rights(Rights::READ.0 | Rights::WRITE.0 | Rights::EXECUTE.0);

/// What profiles take the entries of the root table from.
struct Ready {
    roots: Roots,
}

/// The root of tmpfs, for `/tmp`, and where the budgets of its
/// directories are set.
struct Tmpfs {
    root: NodeAsyncClient,
    temporary: TemporaryFilesystemAsyncClient,
}

enum Roots {
    /// Running from the boot image: tmpfs's directories for `/etc`,
    /// `/var` and `/Users`, and the boot image for the rest.
    Boot {
        etc: Rc<NodeAsyncClient>,
        var: Rc<NodeAsyncClient>,
        users: Rc<NodeAsyncClient>,
    },
    /// The root directory of the root file system.
    Root(Rc<NodeAsyncClient>),
}

pub struct Namespaces {
    bootfs: bool,
    /// The root of the EFI system partition's file system, once
    /// connected.
    esp: RefCell<Option<Rc<NodeAsyncClient>>>,
    ready: RefCell<Option<Rc<Ready>>>,
    /// Whether tmpfs has started, and the tasks whose namespaces wait for
    /// profiles to apply since.
    tmpfs_started: Cell<bool>,
    waiting: RefCell<Vec<Waker>>,
    /// tmpfs, once it serves, and the tasks that wait for it.
    tmpfs: RefCell<Option<Rc<Tmpfs>>>,
    tmpfs_waiting: RefCell<Vec<Waker>>,
}

/// Where an entry of the root table comes from: a directory of the boot
/// image, or a directory and the path below it.
enum Source {
    Boot(&'static str),
    Dir(Rc<NodeAsyncClient>, &'static str),
}

/// Where the programs below a namespace entry come from, for `Spawn` and
/// `Exec` (MIKSPEC-0034.R27).
pub enum Origin {
    /// None may be executed: the entry's rights lack `EXECUTE`.
    None,
    /// A directory of the boot file system, by its path from the root.
    Boot(String),
    /// A directory on a file system server, through a connection of the
    /// process manager's own with the entry's rights, and whether it is
    /// the root's `/usr`, whose files are system components.
    Dir {
        dir: Rc<NodeAsyncClient>,
        system: bool,
    },
}

/// The entries of a process's namespace, by path, with where the
/// programs below each come from.
pub struct Programs {
    entries: Vec<(String, Origin)>,
}

impl Programs {
    /// The root file system `root` as `/`, which the device manager's
    /// programs under `/usr` are looked for in first once namespaces come
    /// from it.
    pub fn root(root: Rc<NodeAsyncClient>) -> Programs {
        Programs {
            entries: alloc::vec![(
                String::from("/"),
                Origin::Dir {
                    dir: root,
                    system: true
                }
            )],
        }
    }

    /// The boot file system as `/`, with `/dev`: the namespace of init,
    /// in which its programs are resolved (MIKSPEC-0034.R27), and of the
    /// processes that go without their profiles.
    pub fn boot() -> Programs {
        Programs {
            entries: alloc::vec![
                (String::from("/"), Origin::Boot(String::new())),
                (String::from("/dev"), Origin::None),
            ],
        }
    }

    /// Where the normalized absolute `path` lies: the origin of the
    /// longest entry that holds it, and the path below that entry.
    pub fn find<'a, 'p>(&'a self, path: &'p str) -> Option<(&'a Origin, &'p str)> {
        self.entries
            .iter()
            .filter_map(|(at, origin)| {
                let rest = match at.as_str() {
                    "/" => path.strip_prefix('/')?,
                    at => match path.strip_prefix(at)? {
                        "" => "",
                        rest => rest.strip_prefix('/')?,
                    },
                };
                Some((at.len(), origin, rest))
            })
            .max_by_key(|&(len, ..)| len)
            .map(|(_, origin, rest)| (origin, rest))
    }
}

/// Adds `entry`, with where its programs come from, to `entries` and
/// `origins`.
fn add(
    entries: &mut Vec<NamespaceEntry>,
    origins: &mut Vec<(String, Origin)>,
    (entry, origin): (NamespaceEntry, Origin),
) {
    origins.push((entry.path.clone(), origin));
    entries.push(entry);
}

/// `entry`, from which no programs come.
fn none(entry: NamespaceEntry) -> (NamespaceEntry, Origin) {
    (entry, Origin::None)
}

type Reply<T> = Result<Result<T, Error>, mikros_rt::interfaces::__midl::Error>;

fn flat<T>(reply: Reply<T>) -> Result<T, Error> {
    reply.unwrap_or(Err(Error::Io))
}

/// The status a start fails with for a file system error.
pub fn status(e: Error) -> Status {
    match e {
        Error::NotFound => Status::NotFound,
        Error::AccessDenied | Error::ReadOnly => Status::AccessDenied,
        Error::NoSpace => Status::NoResources,
        Error::Invalid | Error::NameTooLong => Status::InvalidArgs,
        _ => Status::Internal,
    }
}

/// Opens `path` from `dir` with `flags` and `rights`.
pub async fn open(
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
            esp: RefCell::new(None),
            ready: RefCell::new(None),
            tmpfs_started: Cell::new(false),
            waiting: RefCell::new(Vec::new()),
            tmpfs: RefCell::new(None),
            tmpfs_waiting: RefCell::new(Vec::new()),
        }
    }

    /// Records that a process of `app_id` has started: once it is tmpfs,
    /// new namespaces wait for profiles to apply.
    pub fn started(&self, app_id: &str) {
        if app_id == TMPFS_APP {
            self.tmpfs_started.set(true);
        }
    }

    /// Completes once profiles apply.
    async fn profiles(&self) {
        poll_fn(|cx| match self.ready.borrow().is_some() {
            true => Poll::Ready(()),
            false => {
                self.waiting.borrow_mut().push(cx.waker().clone());
                Poll::Pending
            }
        })
        .await
    }

    /// Whether profiles apply to processes started under `root=bootfs`.
    pub fn runs_from_boot_image(&self) -> bool {
        self.bootfs
    }

    /// Connects to tmpfs through `directory`, the process manager's own,
    /// asking again until it serves, makes its `boot/` directories, fills
    /// them from the boot image, from then on builds namespaces from
    /// profiles, and tells the service manager.
    pub async fn prepare(
        &self,
        shared: &Shared,
        directory: DirectoryAsyncClient,
        fs: Rc<Fs>,
        reactor: Reactor,
    ) {
        let tmpfs = self.serve_tmpfs(&directory, &reactor).await;
        match fill(&tmpfs.root, &fs, &reactor).await {
            Ok((etc, var, users)) => {
                self.apply(Ready {
                    roots: Roots::Boot { etc, var, users },
                });
                self.announce_root(shared).await;
            }
            Err(e) => mikros_rt::log!(Error, "process-manager: filling tmpfs failed: {e:?}"),
        }
    }

    /// tmpfs, asking through `directory` again until it serves, which
    /// the `/tmp` entries that wait for it get from then on.
    async fn serve_tmpfs(&self, directory: &DirectoryAsyncClient, reactor: &Reactor) -> Rc<Tmpfs> {
        let (root, temporary) = tmpfs_once_served(directory, reactor).await;
        let tmpfs = Rc::new(Tmpfs { root, temporary });
        *self.tmpfs.borrow_mut() = Some(tmpfs.clone());
        for waker in self.tmpfs_waiting.take() {
            waker.wake();
        }
        tmpfs
    }

    /// tmpfs, once it serves.
    async fn tmpfs(&self) -> Rc<Tmpfs> {
        poll_fn(|cx| match self.tmpfs.borrow().clone() {
            Some(tmpfs) => Poll::Ready(tmpfs),
            None => {
                self.tmpfs_waiting.borrow_mut().push(cx.waker().clone());
                Poll::Pending
            }
        })
        .await
    }

    /// Connects through `directory`, the process manager's own, to the
    /// root file system once it is published, from then on builds
    /// namespaces from profiles, tells the service manager, and connects
    /// to tmpfs once it serves.
    pub async fn prepare_root(
        &self,
        shared: &Shared,
        directory: DirectoryAsyncClient,
        reactor: Reactor,
    ) {
        let root = match root_filesystem(&directory, &reactor).await {
            Ok(root) => root,
            // The system ends before the root file system came.
            Err(Status::PeerClosed) => return,
            Err(e) => {
                mikros_rt::log!(Error, "process-manager: no root file system: {e:?}");
                return;
            }
        };
        self.apply(Ready {
            roots: Roots::Root(Rc::new(root)),
        });
        mikros_rt::log!(
            Info,
            "process-manager: namespaces come from the root file system"
        );
        self.announce_root(shared).await;
        self.serve_tmpfs(&directory, &reactor).await;
    }

    /// The file at `path` below `/usr`: the root file system's once
    /// namespaces come from it, or else, as before, the boot file
    /// system's, which holds the files of early services at the same
    /// paths.
    pub async fn usr_file(&self, shared: &Shared, path: &str) -> Option<Vec<u8>> {
        let ready = self.ready.borrow().clone();
        if let Some(Roots::Root(root)) = ready.as_deref().map(|r| &r.roots) {
            let at = format!("usr/{path}");
            if let Ok(bytes) = files::read(root, &shared.reactor, &at, USR_FILE_MAX).await {
                return Some(bytes);
            }
        }
        shared.fs.file(&format!("usr/{path}")).map(<[u8]>::to_vec)
    }

    /// Where the device manager's programs under `/usr` are looked for
    /// first: the root file system, once namespaces come from it.
    pub fn root_programs(&self) -> Option<Programs> {
        match self.ready.borrow().as_deref().map(|r| &r.roots) {
            Some(Roots::Root(root)) => Some(Programs::root(root.clone())),
            _ => None,
        }
    }

    /// Whether namespaces come from the root file system.
    pub fn on_root(&self) -> bool {
        matches!(
            self.ready.borrow().as_deref().map(|r| &r.roots),
            Some(Roots::Root(_))
        )
    }

    /// Gives the service manager read-only connections to the `/usr` and
    /// `/etc` of the profiles that now apply, through the control
    /// connection, so that it reads its manifests there from then on.
    async fn announce_root(&self, shared: &Shared) {
        let Some(ready) = self.ready.borrow().clone() else {
            return;
        };
        let Some(control) = shared.control.borrow().clone() else {
            return;
        };
        let usr = self.entry(shared, &ready, "/usr", Rights::READ).await;
        let etc = self.entry(shared, &ready, "/etc", Rights::READ).await;
        let (usr, etc) = match (usr, etc) {
            (Ok((usr, _)), Ok((etc, _))) => (usr.directory, etc.directory),
            (Err(e), _) | (_, Err(e)) => {
                mikros_rt::log!(
                    Error,
                    "process-manager: opening /usr and /etc for the service manager failed: {e:?}"
                );
                return;
            }
        };
        // The service manager's programs come from the root's /usr from
        // now on, so that it starts the services there.
        if let Some(pid) = shared.identity_setter.get() {
            let app_id = shared.records.borrow().get(pid).map(|r| r.app_id.clone());
            if let Some(app_id) = app_id {
                match self.of(shared, &app_id, 0, true).await {
                    Ok((_, programs)) => {
                        if let Some(record) = shared.records.borrow_mut().get_mut(pid) {
                            record.programs = Rc::new(programs);
                        }
                    }
                    Err(e) => mikros_rt::log!(
                        Error,
                        "process-manager: the service manager's programs stay the boot file system's: {e:?}"
                    ),
                }
            }
        }
        match control.root_available(usr, etc).await {
            Ok(Ok(_)) => {}
            Ok(Err(e)) => mikros_rt::log!(Error, "process-manager: RootAvailable failed: {e:?}"),
            Err(e) => mikros_rt::log!(Error, "process-manager: RootAvailable failed: {e:?}"),
        }
    }

    /// Connects through `directory`, the process manager's own, to the
    /// file system of the EFI system partition the system booted from,
    /// once it is published, to serve it as `/boot/efi`.
    pub async fn prepare_esp(&self, directory: DirectoryAsyncClient, reactor: Reactor) {
        match filesystem(&directory, &reactor, ESP_INSTANCE, RW).await {
            Ok(root) => {
                *self.esp.borrow_mut() = Some(Rc::new(root));
                mikros_rt::log!(
                    Info,
                    "process-manager: serving the EFI system partition as /boot/efi"
                );
            }
            Err(Status::PeerClosed) => {}
            Err(e) => mikros_rt::log!(Error, "process-manager: no EFI system partition: {e:?}"),
        }
    }

    /// Makes profiles apply, from `ready`.
    fn apply(&self, ready: Ready) {
        *self.ready.borrow_mut() = Some(Rc::new(ready));
        for waker in self.waiting.take() {
            waker.wake();
        }
    }

    /// The namespace of a new process of `app_id` and `user`, the service
    /// manager that init asked for when `service_manager`, and where the
    /// programs it names come from. Once tmpfs has started, that of any
    /// other process waits for profiles to apply.
    pub async fn of(
        &self,
        shared: &Shared,
        app_id: &str,
        user: u32,
        service_manager: bool,
    ) -> Result<(Vec<NamespaceEntry>, Programs), Status> {
        let dev = NamespaceEntry {
            path: "/dev".into(),
            directory: shared.dev.directory()?,
        };
        if self.bootfs && self.tmpfs_started.get() && app_id != TMPFS_APP {
            self.profiles().await;
        }
        let ready = self.ready.borrow().clone();
        let Some(ready) = ready else {
            let root = crate::start::root(&shared.fs)?;
            let entries = alloc::vec![
                NamespaceEntry {
                    path: "/".into(),
                    directory: root
                },
                dev
            ];
            return Ok((entries, Programs::boot()));
        };
        let reactor = &shared.reactor;
        let mut entries = Vec::new();
        let mut origins = Vec::new();
        if service_manager {
            add(
                &mut entries,
                &mut origins,
                self.entry(shared, &ready, "/usr", RX).await?,
            );
            add(
                &mut entries,
                &mut origins,
                self.entry(shared, &ready, "/etc", Rights::READ).await?,
            );
        } else if app_id == CLI_APP {
            // User 0 has no folder in /Users, and so no name is needed.
            let name = match user {
                0 => None,
                _ => Some(crate::users::name(shared, user).await?),
            };
            add(
                &mut entries,
                &mut origins,
                self.entry(shared, &ready, "/usr", RX).await?,
            );
            add(
                &mut entries,
                &mut origins,
                self.entry(shared, &ready, "/etc", Rights::READ).await?,
            );
            add(
                &mut entries,
                &mut origins,
                self.entry(shared, &ready, "/Apps", Rights::READ).await?,
            );
            let libraries = self
                .entry(shared, &ready, "/Libraries", Rights::READ)
                .await?;
            let libraries = match &name {
                None => libraries,
                Some(name) => self.libraries(shared, &ready, libraries, name).await?,
            };
            add(&mut entries, &mut origins, libraries);
            if let Some(name) = &name {
                let home = format!("/Users/{name}");
                add(
                    &mut entries,
                    &mut origins,
                    self.entry(shared, &ready, &home, RWX).await?,
                );
            }
            let budget = shared.user_budget(user)?;
            let tmpfs = self.tmpfs().await;
            add(
                &mut entries,
                &mut origins,
                none(tmp(&tmpfs, &format!("cli/{user}"), budget, reactor).await?),
            );
        } else if !app_id.starts_with("system.") {
            // A native app.
            if user == 0 {
                return Err(Status::AccessDenied);
            }
            // Its profile names the user, whose name is not known yet.
            return Err(Status::NotSupported);
        } else {
            let (program, listed) = manifest_entries(self, shared, app_id).await;
            if program {
                add(
                    &mut entries,
                    &mut origins,
                    self.entry(shared, &ready, "/usr", RX).await?,
                );
                // A program whose manifest lists `/etc` gets it with the
                // rights listed there instead.
                let lists_etc = listed
                    .iter()
                    .any(|(path, _)| path::normalize(path).as_deref() == Some("/etc"));
                if !lists_etc {
                    add(
                        &mut entries,
                        &mut origins,
                        self.entry(shared, &ready, "/etc", Rights::READ).await?,
                    );
                }
                let path = format!("apps/{user}/{app_id}");
                let budget = shared.budget_of(user, app_id)?;
                let tmpfs = self.tmpfs().await;
                add(
                    &mut entries,
                    &mut origins,
                    none(tmp(&tmpfs, &path, budget, reactor).await?),
                );
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
                let path = path::normalize(&path).ok_or(Status::NotFound)?;
                // Only the accounts service reaches its data.
                if app_id != ACCOUNTS_APP && path::overlaps(&path, ACCOUNTS_DATA) {
                    return Err(Status::AccessDenied);
                }
                let rights = rights(&text).ok_or(Status::InvalidArgs)?;
                add(
                    &mut entries,
                    &mut origins,
                    self.entry(shared, &ready, &path, rights).await?,
                );
            }
        }
        if app_id == CLI_APP || self.holds_volumes(shared, app_id, user).await {
            let volumes = NamespaceEntry {
                path: VOLUMES.into(),
                directory: shared.volumes.directory()?,
            };
            add(&mut entries, &mut origins, none(volumes));
        }
        add(&mut entries, &mut origins, none(dev));
        Ok((entries, Programs { entries: origins }))
    }

    /// Whether the processes of `app_id` and `user` hold `files.volumes`,
    /// as the broker says.
    async fn holds_volumes(&self, shared: &Shared, app_id: &str, user: u32) -> bool {
        let client = ClientIdentity {
            pid: 0,
            user,
            app_id: app_id.into(),
        };
        shared
            .permissions
            .holds(client, false, VOLUMES_PERMISSION)
            .await
    }

    /// The entry `path` of the root table with `rights`, and where the
    /// programs below it come from. A path that does not exist is made
    /// when the rights hold `WRITE` and it lies on tmpfs, and fails with
    /// `NOT_FOUND` otherwise; rights beyond those of its root fail with
    /// `ACCESS_DENIED`.
    /// `/Libraries` for user `name`: a read-only view of `system`, the
    /// system's, merged with the user's `Libraries` folder, or `system`
    /// alone when the user has none.
    async fn libraries(
        &self,
        shared: &Shared,
        ready: &Ready,
        system: (NamespaceEntry, Origin),
        name: &str,
    ) -> Result<(NamespaceEntry, Origin), Status> {
        let path = format!("/Users/{name}/Libraries");
        let own = match self.entry(shared, ready, &path, Rights::READ).await {
            Ok((own, _)) => own,
            Err(e) => {
                mikros_rt::log!(
                    Warning,
                    "process-manager: {path} cannot be opened ({e:?}); /Libraries is only the system's"
                );
                return Ok(system);
            }
        };
        let layers = alloc::vec![system.0.directory, own.directory];
        let view = crate::users::merged(shared, layers, Rights::READ).await?;
        Ok(none(NamespaceEntry {
            path: "/Libraries".into(),
            directory: view,
        }))
    }

    async fn entry(
        &self,
        shared: &Shared,
        ready: &Ready,
        path: &str,
        rights: Rights,
    ) -> Result<(NamespaceEntry, Origin), Status> {
        let normal = path.trim_end_matches('/');
        let mut table: Vec<(&str, Source, Rights)> = match &ready.roots {
            Roots::Boot { etc, var, users } => alloc::vec![
                ("/usr", Source::Boot("usr"), RX),
                ("/Apps", Source::Boot("Apps"), Rights::READ),
                ("/Libraries", Source::Boot("Libraries"), Rights::READ),
                ("/etc", Source::Dir(etc.clone(), ""), RWX),
                ("/var", Source::Dir(var.clone(), ""), RWX),
                ("/Users", Source::Dir(users.clone(), ""), RWX),
            ],
            Roots::Root(root) => {
                if normal.is_empty() || path::overlaps(normal, SYSTEM) {
                    return Err(Status::AccessDenied);
                }
                alloc::vec![
                    ("/usr", Source::Dir(root.clone(), "usr"), RX),
                    ("/Apps", Source::Dir(root.clone(), "Apps"), Rights::READ),
                    (
                        "/Libraries",
                        Source::Dir(root.clone(), "Libraries"),
                        Rights::READ,
                    ),
                    ("/etc", Source::Dir(root.clone(), "etc"), RWX),
                    ("/var", Source::Dir(root.clone(), "var"), RWX),
                    ("/Users", Source::Dir(root.clone(), "Users"), RWX),
                ]
            }
        };
        if let Some(esp) = self.esp.borrow().as_ref() {
            table.push((BOOT_EFI, Source::Dir(esp.clone(), ""), RW));
        }
        let (top, source, most, rest) = table
            .into_iter()
            .find_map(|(root, source, most)| {
                let rest = normal.strip_prefix(root)?;
                (rest.is_empty() || rest.starts_with('/')).then(|| {
                    (
                        root,
                        source,
                        most,
                        String::from(rest.trim_start_matches('/')),
                    )
                })
            })
            .ok_or(Status::NotFound)?;
        // Missing directories are made only where services keep state.
        let creatable = matches!(top, "/etc" | "/var" | "/Users");
        let executable = rights.contains(Rights::EXECUTE);
        // The system's own programs are those of the root's /usr; the
        // boot file system tells its own apart itself.
        let system = top == "/usr";
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
                let origin = match executable {
                    true => Origin::Boot(boot_path),
                    false => Origin::None,
                };
                let entry = NamespaceEntry {
                    path: String::from(normal),
                    directory: client,
                };
                Ok((entry, origin))
            }
            Source::Dir(dir, top) => {
                let rest = match (top.is_empty(), rest.is_empty()) {
                    (true, _) => rest,
                    (false, true) => String::from(top),
                    (false, false) => format!("{top}/{rest}"),
                };
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
                    Err(Error::NotFound) if creatable && rights.contains(Rights::WRITE) => {
                        let made = directories(&dir, &rest, &shared.reactor)
                            .await
                            .map_err(status)?;
                        let (again, server) =
                            endpoints::<Node>().map_err(|_| Status::NoResources)?;
                        flat(made.clone(server, rights).await).map_err(status)?;
                        let origin = match executable {
                            true => Origin::Dir {
                                dir: Rc::new(made),
                                system,
                            },
                            false => Origin::None,
                        };
                        let entry = NamespaceEntry {
                            path: String::from(normal),
                            directory: again,
                        };
                        return Ok((entry, origin));
                    }
                    Err(e) => return Err(status(e)),
                }
                // The process manager keeps a connection of its own to a
                // directory that programs may come from.
                let origin = match executable {
                    true => Origin::Dir {
                        dir: Rc::new(
                            open(
                                &dir,
                                &target,
                                OpenFlags::DIRECTORY,
                                0,
                                rights,
                                &shared.reactor,
                            )
                            .await
                            .map_err(status)?,
                        ),
                        system,
                    },
                    false => Origin::None,
                };
                let entry = NamespaceEntry {
                    path: String::from(normal),
                    directory: client,
                };
                Ok((entry, origin))
            }
        }
    }
}

/// A `/tmp` entry for the directory `path` of tmpfs, made if missing,
/// with read and write rights, whose files are charged to `budget`.
async fn tmp(
    tmpfs: &Tmpfs,
    path: &str,
    budget: OwnedHandle,
    reactor: &Reactor,
) -> Result<NamespaceEntry, Status> {
    let dir = directories(&tmpfs.root, path, reactor)
        .await
        .map_err(status)?;
    flat(tmpfs.temporary.set_budget(path.into(), budget).await).map_err(status)?;
    let (directory, server) = endpoints::<Node>().map_err(|_| Status::NoResources)?;
    flat(dir.clone(server, RW).await).map_err(status)?;
    Ok(NamespaceEntry {
        path: "/tmp".into(),
        directory,
    })
}

/// The root of tmpfs and its `TemporaryFilesystem`, asking again until
/// it serves.
async fn tmpfs_once_served(
    directory: &DirectoryAsyncClient,
    reactor: &Reactor,
) -> (NodeAsyncClient, TemporaryFilesystemAsyncClient) {
    loop {
        if let Some(found) = tmpfs(directory, reactor).await {
            return found;
        }
        let mut now = 0;
        // SAFETY: the out pointer is valid for the call.
        unsafe { mikros_rt::sys::clock_get_monotonic(&mut now) };
        let _ = reactor.sleep_until(now + RETRY).await;
    }
}

/// The root directory of the root file system, with read, write and
/// execute rights, once its `filesystem` service is published as the
/// instance `root`.
async fn root_filesystem(
    directory: &DirectoryAsyncClient,
    reactor: &Reactor,
) -> Result<NodeAsyncClient, Status> {
    filesystem(directory, reactor, ROOT_INSTANCE, RWX).await
}

/// The root directory, with `rights`, of the file system published as
/// `instance` of `device.filesystem`, once it is, which a watch of the
/// protocol waits for.
async fn filesystem(
    directory: &DirectoryAsyncClient,
    reactor: &Reactor,
    instance: &str,
    rights: Rights,
) -> Result<NodeAsyncClient, Status> {
    let midl = |_| Status::PeerClosed;
    directory
        .watch(DEVICE_FILESYSTEM.into())
        .await
        .map_err(midl)??;
    loop {
        // The protocol has no instances until a first one is registered.
        let listed = match directory
            .list(DEVICE_FILESYSTEM.into())
            .await
            .map_err(midl)?
        {
            Ok(listed) => listed.instances,
            Err(Status::NotFound) => Vec::new(),
            Err(e) => return Err(e),
        };
        if listed.iter().any(|i| i == instance) {
            break;
        }
        loop {
            match directory.next_event().await.map_err(midl)? {
                DirectoryEvent::OnInstanceAdded(added)
                    if added.protocol == DEVICE_FILESYSTEM && added.instance == instance =>
                {
                    break;
                }
                _ => {}
            }
        }
    }
    let (client, server) = endpoints::<Filesystem>().map_err(|_| Status::NoResources)?;
    directory
        .connect(
            DEVICE_FILESYSTEM.into(),
            Some(instance.into()),
            server.into_handle(),
        )
        .await
        .map_err(midl)??;
    let fs = FilesystemAsyncClient::new(client, reactor.clone());
    let (root, server) = endpoints::<Node>().map_err(|_| Status::NoResources)?;
    flat(fs.get_root(server, rights).await).map_err(status)?;
    Ok(NodeAsyncClient::new(root, reactor.clone()))
}

/// The root of tmpfs, read and write, and its `TemporaryFilesystem`,
/// once it serves.
async fn tmpfs(
    directory: &DirectoryAsyncClient,
    reactor: &Reactor,
) -> Option<(NodeAsyncClient, TemporaryFilesystemAsyncClient)> {
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
    let (temporary, server) = endpoints::<TemporaryFilesystem>().ok()?;
    match directory
        .connect(TEMPORARY.into(), None, server.into_handle())
        .await
    {
        Ok(Ok(_)) => {}
        _ => return None,
    }
    let temporary = TemporaryFilesystemAsyncClient::new(temporary, reactor.clone());
    Some((NodeAsyncClient::new(root, reactor.clone()), temporary))
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
/// and rights, read from `/usr` as `namespaces` serves it. Without a valid
/// manifest it has none.
async fn manifest_entries(
    namespaces: &Namespaces,
    shared: &Shared,
    app_id: &str,
) -> (bool, Vec<(String, String)>) {
    let pairs = |entries: Vec<mikros_manifest::NamespaceEntry>| {
        entries.into_iter().map(|e| (e.path, e.rights)).collect()
    };
    if let Some(name) = app_id.strip_prefix("system.driver.") {
        let path = format!("lib/drivers/{name}/driver.yaml");
        return match namespaces
            .usr_file(shared, &path)
            .await
            .map(|b| mikros_manifest::parse_driver(name, &b))
        {
            Some(Ok(m)) => (false, pairs(m.driver.namespace)),
            _ => (false, Vec::new()),
        };
    }
    let Some(name) = app_id.strip_prefix("system.") else {
        return (false, Vec::new());
    };
    let service = format!("{name}.yaml");
    if let Some(bytes) = namespaces
        .usr_file(shared, &format!("lib/services/{service}"))
        .await
    {
        return match mikros_manifest::parse(&service, &bytes) {
            Ok(m) => (false, pairs(m.service.namespace)),
            Err(_) => (false, Vec::new()),
        };
    }
    let program = format!("{app_id}.yaml");
    match namespaces
        .usr_file(shared, &format!("lib/programs/{program}"))
        .await
        .map(|b| mikros_manifest::parse_program(&program, &b))
    {
        Some(Ok(m)) => (true, pairs(m.program.namespace)),
        _ => (false, Vec::new()),
    }
}
