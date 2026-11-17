//! The namespaces of new processes.
//!
//! Until profiles apply, every process gets `/`, the boot file system,
//! and `/dev` (MIKSPEC-0050.R36). Profiles apply once the process manager
//! has connected to the root file system, the instance `root` of
//! `device.filesystem` (MIKSPEC-0050.R8, MIKSPEC-0050.R29): the root
//! table's entries then come from the root file system, and `/tmp` from
//! tmpfs, for which an entry waits until tmpfs serves.
//!
//! When `live` is a word of the command line, the root file system is
//! read-only, and profiles apply only once the process manager has made
//! tmpfs's `live/config`, `live/state`, `live/logs` and `live/users` and,
//! through the union service, a view of each over the root's subvolume
//! of that name, read only, to which changes go; the root table's
//! `/system/config`, `/system/state`, `/system/logs` and `/users` are
//! those views (MIKSPEC-0050.R41, MIKSPEC-0050.R42, MIKSPEC-0050.R46).
//! Processes started before go without their profiles. An entry that is
//! the root file system's root or lies in `@system` fails the start with
//! `ACCESS_DENIED` (MIKSPEC-0050.R28).
//!
//! The process manager also connects to the file system of the EFI
//! system partition that the system booted from, the instance `esp` of
//! `device.filesystem`, once it is published, and from then on gives it
//! as `/system/boot/efi` to the processes whose manifests list it
//! (MIKSPEC-0064.R9, MIKSPEC-0064.R15). Processes whose profiles apply and
//! that hold `user.volumes.access` get `/volumes`, read and write, and so
//! do the command-line tools of `system.cli`, whose home folder may also
//! hold programs. An elevated process also gets its user's home folder,
//! read and write, and `/apps`, read only, unless it has them already
//! (MIKSPEC-0092.R9).
//!
//! A service whose manifest lists `/system/tmp/<its name>` gets tmpfs's
//! `system/<its name>` there, and a manifest that lists another path in
//! `/system/tmp` makes the start fail with `INVALID_ARGS`. Every driver
//! gets `/system/tmp/<app ID>`, tmpfs's `drivers/<app ID>`, and
//! `/system/state/<app ID>`, both read and write, without listing them
//! (MIKSPEC-0100.R14, MIKSPEC-0050.R46, MIKSPEC-0050.R47).

use crate::processes::Shared;
use alloc::format;
use alloc::rc::Rc;
use alloc::string::String;
use alloc::vec::Vec;
use core::cell::RefCell;
use core::future::poll_fn;
use core::task::{Poll, Waker};
use mikros_bootfs::server::ItemKind;
use mikros_manifest::path;
use mikros_rt::exec::Reactor;
use mikros_rt::files;
use mikros_rt::interfaces::__midl::{ClientEnd, OwnedHandle, endpoints};
use mikros_rt::interfaces::mikros::base::Status;
use mikros_rt::interfaces::mikros::fs::{
    Error, Filesystem, FilesystemAsyncClient, Layer, Node, NodeAsyncClient, NodeType, OpenFlags,
    Rights, TemporaryFilesystem, TemporaryFilesystemAsyncClient,
};
use mikros_rt::interfaces::mikros::process::NamespaceEntry;
use mikros_rt::interfaces::mikros::service::{
    ClientIdentity, DirectoryAsyncClient, DirectoryEvent,
};

/// The accounts service's data directory, which no other process's
/// manifest entry may be, lie in, or hold.
const ACCOUNTS_DATA: &str = "/system/state/accounts";
const ACCOUNTS_APP: &str = "system.accountsvc";
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
const BOOT_EFI: &str = "/system/boot/efi";
/// The apps installed for every user.
const APPS: &str = "/apps";
/// The directory of the other volumes, and the permission it needs.
const VOLUMES: &str = "/volumes";
/// The folders that apps share (MIKSPEC-0101.R33).
const PUBLIC: &str = "/public";
/// The folder of the commands of installed packages (MIKSPEC-0101.R44).
const COMMANDS: &str = "/system/commands";
/// The permission that lets an app write its installed package
/// (MIKSPEC-0101.R7).
const APPS_WRITE: &str = "admin.apps.write";
/// The system version, its configuration, and the folders of services'
/// and drivers' temporary files and state.
const SYSTEM_ENTRY: &str = "/system";
const CONFIG: &str = "/system/config";
const SYSTEM_TMP: &str = "/system/tmp";
const STATE: &str = "/system/state";
/// The prefix of the app IDs of drivers.
const DRIVER_PREFIX: &str = "system.driver.";
const VOLUMES_PERMISSION: &str = "user.volumes.access";
const ESP_INSTANCE: &str = "esp";
/// How long to wait before asking again for tmpfs.
const RETRY: i64 = 20_000_000;
/// The largest manifest read from the root file system.
const SYSTEM_FILE_MAX: usize = 1 << 16;
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
    /// The root directory of the root file system.
    Root(Rc<NodeAsyncClient>),
    /// A live system's: the root directory of its root file system, read
    /// and execute only, and the views of `/system/config`,
    /// `/system/state`, `/system/logs` and `/users`, in that order.
    Live {
        root: Rc<NodeAsyncClient>,
        views: [Rc<NodeAsyncClient>; 4],
    },
}

impl Roots {
    /// The root directory of the root file system.
    fn root(&self) -> &Rc<NodeAsyncClient> {
        match self {
            Roots::Root(root) | Roots::Live { root, .. } => root,
        }
    }
}

pub struct Namespaces {
    live: bool,
    /// The root of the EFI system partition's file system, once
    /// connected.
    esp: RefCell<Option<Rc<NodeAsyncClient>>>,
    ready: RefCell<Option<Rc<Ready>>>,
    /// tmpfs, once it serves, and the tasks that wait for it.
    tmpfs: RefCell<Option<Rc<Tmpfs>>>,
    tmpfs_waiting: RefCell<Vec<Waker>>,
    /// The process manager's connection to the folder of commands, once
    /// the install service gave it.
    commands: RefCell<Option<Rc<NodeAsyncClient>>>,
}

/// Where an entry of the root table comes from: a directory and the path
/// below it.
enum Source {
    Dir(Rc<NodeAsyncClient>, &'static str),
}

/// Where the programs below a namespace entry come from, for `Spawn` and
/// `Exec` (MIKSPEC-0034.R27).
pub enum Origin {
    /// None may be executed: the entry's rights lack `EXECUTE`.
    None,
    /// A folder of the root file system at its real path, from which
    /// none may be executed, and the entry's rights.
    Folder { real: String, rights: Rights },
    /// A directory of the boot file system, by its path from the root.
    Boot(String),
    /// A directory on a file system server, through a connection of the
    /// process manager's own with the entry's rights, whether it is the
    /// root's `/system`, whose files are system components, its path on
    /// the root file system, when it is a folder there and not a view,
    /// and the entry's rights.
    Dir {
        dir: Rc<NodeAsyncClient>,
        system: bool,
        real: Option<String>,
        rights: Rights,
    },
}

/// The entries of a process's namespace, by path, with where the
/// programs below each come from.
pub struct Programs {
    entries: Vec<(String, Origin)>,
}

impl Programs {
    /// The root file system `root` as `/`, which the device manager's
    /// programs under `/system` are looked for in first once namespaces
    /// come from it.
    pub fn root(root: Rc<NodeAsyncClient>) -> Programs {
        Programs {
            entries: alloc::vec![(
                String::from("/"),
                Origin::Dir {
                    dir: root,
                    system: true,
                    real: Some(String::from("/")),
                    rights: RX,
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

    /// The ID of the installed app whose package holds the file that the
    /// absolute `path` names, judged by the folder of the root file
    /// system that the entry it lies in comes from, as the system call
    /// gate judges `/system` (MIKSPEC-0101.R39).
    pub fn package_of(&self, path: &str) -> Option<String> {
        package_of(&self.real_path(path)?).map(String::from)
    }

    /// The name of the command that the absolute `path` reaches through
    /// the entry `/system/commands` (MIKSPEC-0101.R44).
    pub fn command_of(&self, path: &str) -> Option<String> {
        let normal = path::normalize(path)?;
        let name = normal.strip_prefix(COMMANDS)?.strip_prefix('/')?;
        let reached = self.entries.iter().any(|(at, _)| at == COMMANDS);
        let longer = self
            .entries
            .iter()
            .any(|(at, _)| at.len() > COMMANDS.len() && path::overlaps(at, &normal));
        (reached && !longer && !name.is_empty() && !name.contains('/')).then(|| String::from(name))
    }

    /// The real path on the root file system of the folder that the
    /// absolute `path` names, and the rights of the entry it lies in, when
    /// that entry is a folder there and not a view.
    pub fn folder_of(&self, path: &str) -> Option<(String, Rights)> {
        let normal = path::normalize(path)?;
        let (real, rights, rest) = match self.find(&normal)? {
            (
                Origin::Dir {
                    real: Some(real),
                    rights,
                    ..
                },
                rest,
            ) => (real, *rights, rest),
            (Origin::Folder { real, rights }, rest) => (real, *rights, rest),
            _ => return None,
        };
        Some((join(real, rest), rights))
    }

    /// Adds the entry at `path`, whose programs come from `origin`.
    pub fn add(&mut self, path: String, origin: Origin) {
        self.entries.push((path, origin));
    }

    /// The path on the root file system of the file that the absolute
    /// `path` names, when the entry it lies in is a folder there.
    pub fn real_path(&self, path: &str) -> Option<String> {
        let normal = path::normalize(path)?;
        let (
            Origin::Dir {
                real: Some(real), ..
            },
            rest,
        ) = self.find(&normal)?
        else {
            return None;
        };
        Some(join(real, rest))
    }
}

/// The path `rest` below the folder `real`.
fn join(real: &str, rest: &str) -> String {
    match (real.trim_end_matches('/'), rest) {
        ("", "") => String::from("/"),
        (real, "") => String::from(real),
        (real, rest) => format!("{real}/{rest}"),
    }
}

/// The ID of the installed app whose package holds the normalized
/// absolute `path`: one below `/apps/<id>/` or below
/// `/users/<user>/apps/<id>/package/` (MIKSPEC-0101.R36).
pub fn package_of(path: &str) -> Option<&str> {
    let (id, below) = match path.strip_prefix("/apps/") {
        Some(rest) => rest.split_once('/')?,
        None => {
            let rest = path.strip_prefix("/users/")?;
            let (user, rest) = rest.split_once('/')?;
            let (id, rest) = rest.strip_prefix("apps/")?.split_once('/')?;
            let below = rest.strip_prefix("package/")?;
            if user.is_empty() {
                return None;
            }
            (id, below)
        }
    };
    (!below.is_empty() && mikros_mika::id::is_valid(id)).then_some(id)
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
        let live = command_line.split_whitespace().any(|word| word == "live");
        Namespaces {
            live,
            esp: RefCell::new(None),
            ready: RefCell::new(None),
            tmpfs: RefCell::new(None),
            tmpfs_waiting: RefCell::new(Vec::new()),
            commands: RefCell::new(None),
        }
    }

    /// The entry `/system/commands`, read and execute, a connection to the
    /// install service's folder of commands (MIKSPEC-0101.R44), or none
    /// when the install service cannot give it. Programs come from it
    /// through a connection of the process manager's own bound to `user`,
    /// so that the user's own commands are found.
    async fn commands(&self, shared: &Shared, user: u32) -> Option<(NamespaceEntry, Origin)> {
        for _ in 0..2 {
            let kept = self.commands.borrow().clone();
            let folder = match kept {
                Some(folder) => folder,
                None => match crate::users::commands(shared).await {
                    Ok(folder) => {
                        let folder = Rc::new(folder);
                        *self.commands.borrow_mut() = Some(folder.clone());
                        folder
                    }
                    // A system without the install service has no commands
                    // of packages.
                    Err(Status::NotFound) => return None,
                    Err(e) => {
                        mikros_rt::log!(
                            Warning,
                            "process-manager: no {COMMANDS} from the install service ({e:?})"
                        );
                        return None;
                    }
                },
            };
            match (
                connection(&folder, RX).await,
                bound(&folder, user, shared).await,
            ) {
                (Ok(directory), Ok(programs)) => {
                    let entry = NamespaceEntry {
                        path: COMMANDS.into(),
                        directory,
                    };
                    let origin = Origin::Dir {
                        dir: Rc::new(programs),
                        system: false,
                        real: None,
                        rights: RX,
                    };
                    return Some((entry, origin));
                }
                // The install service went away: ask it again.
                _ => *self.commands.borrow_mut() = None,
            }
        }
        None
    }

    /// The entry of the folder `path` of the root table with `rights`,
    /// and where the programs below it come from.
    pub async fn folder(
        &self,
        shared: &Shared,
        path: &str,
        rights: Rights,
    ) -> Result<(NamespaceEntry, Origin), Status> {
        let ready = self.ready.borrow().clone().ok_or(Status::BadState)?;
        self.entry(shared, &ready, path, rights).await
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
    /// to tmpfs once it serves. A live system connects to tmpfs first, and
    /// builds namespaces from profiles only once it has made the views of
    /// `/system/config`, `/system/state`, `/system/logs` and `/users`.
    pub async fn prepare_root(
        &self,
        shared: &Shared,
        directory: DirectoryAsyncClient,
        reactor: Reactor,
    ) {
        let rights = if self.live { RX } else { RWX };
        let root = match filesystem(&directory, &reactor, ROOT_INSTANCE, rights).await {
            Ok(root) => Rc::new(root),
            // The system ends before the root file system came.
            Err(Status::PeerClosed) => return,
            Err(e) => {
                mikros_rt::log!(Error, "process-manager: no root file system: {e:?}");
                return;
            }
        };
        let roots = match self.live {
            true => {
                let tmpfs = self.serve_tmpfs(&directory, &reactor).await;
                match live_views(shared, &root, &tmpfs.root, &reactor).await {
                    Ok(views) => Roots::Live { root, views },
                    Err(e) => {
                        mikros_rt::log!(
                            Error,
                            "process-manager: making the live system's views failed: {e:?}"
                        );
                        return;
                    }
                }
            }
            false => Roots::Root(root),
        };
        self.apply(Ready { roots });
        mikros_rt::log!(
            Info,
            "process-manager: namespaces come from the root file system"
        );
        self.announce_root(shared).await;
        if !self.live {
            self.serve_tmpfs(&directory, &reactor).await;
        }
    }

    /// The file at `path` below `/system`: the root file system's once
    /// namespaces come from it, or else, as before, the boot file
    /// system's, which holds the files of early services at the same
    /// paths.
    pub async fn system_file(&self, shared: &Shared, path: &str) -> Option<Vec<u8>> {
        let ready = self.ready.borrow().clone();
        if let Some(root) = ready.as_deref().map(|r| r.roots.root()) {
            let at = format!("system/{path}");
            if let Ok(bytes) = files::read(root, &shared.reactor, &at, SYSTEM_FILE_MAX).await {
                return Some(bytes);
            }
        }
        shared
            .fs
            .file(&format!("system/{path}"))
            .map(<[u8]>::to_vec)
    }

    /// The names of the files that end in `suffix` in the folder at
    /// `path` below `/system`: the root file system's once namespaces
    /// come from it, or else the boot file system's. None when there is
    /// no such folder.
    pub async fn system_names(&self, shared: &Shared, path: &str, suffix: &str) -> Vec<String> {
        let ready = self.ready.borrow().clone();
        let at = format!("system/{path}");
        if let Some(root) = ready.as_deref().map(|r| r.roots.root()) {
            let listed = files::list(root, &shared.reactor, &at, NodeType::File, suffix).await;
            return listed.unwrap_or_default();
        }
        let items = shared.fs.subtree(&at).unwrap_or_default();
        items
            .into_iter()
            .filter(|i| matches!(i.kind, ItemKind::File(_)) && !i.path.contains('/'))
            .filter(|i| i.path.ends_with(suffix))
            .map(|i| i.path)
            .collect()
    }

    /// Where the device manager's programs under `/system` are looked for
    /// first: the root file system, once namespaces come from it.
    pub fn root_programs(&self) -> Option<Programs> {
        let ready = self.ready.borrow();
        let root = ready.as_deref().map(|r| r.roots.root().clone());
        root.map(Programs::root)
    }

    /// Whether namespaces come from the root file system.
    pub fn on_root(&self) -> bool {
        self.ready.borrow().is_some()
    }

    /// Gives the service manager read-only connections to the `/system`
    /// and `/system/config` of the profiles that now apply, through the
    /// control connection, so that it reads its manifests there from then
    /// on.
    async fn announce_root(&self, shared: &Shared) {
        let Some(ready) = self.ready.borrow().clone() else {
            return;
        };
        let Some(control) = shared.control.borrow().clone() else {
            return;
        };
        let system = self.entry(shared, &ready, SYSTEM_ENTRY, Rights::READ).await;
        let config = self.entry(shared, &ready, CONFIG, Rights::READ).await;
        let (system, config) = match (system, config) {
            (Ok((system, _)), Ok((config, _))) => (system.directory, config.directory),
            (Err(e), _) | (_, Err(e)) => {
                mikros_rt::log!(
                    Error,
                    "process-manager: opening /system and /system/config for the service manager failed: {e:?}"
                );
                return;
            }
        };
        // The service manager's programs come from the root's /system from
        // now on, so that it starts the services there.
        if let Some(pid) = shared.identity_setter.get() {
            let app_id = shared.records.borrow().get(pid).map(|r| r.app_id.clone());
            if let Some(app_id) = app_id {
                match self.of(shared, &app_id, 0, 0, true).await {
                    Ok((_, programs, _)) => {
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
        match control.root_available(system, config).await {
            Ok(Ok(_)) => {}
            Ok(Err(e)) => mikros_rt::log!(Error, "process-manager: RootAvailable failed: {e:?}"),
            Err(e) => mikros_rt::log!(Error, "process-manager: RootAvailable failed: {e:?}"),
        }
    }

    /// Connects through `directory`, the process manager's own, to the
    /// file system of the EFI system partition the system booted from,
    /// once it is published, to serve it as `/system/boot/efi`.
    pub async fn prepare_esp(&self, directory: DirectoryAsyncClient, reactor: Reactor) {
        match filesystem(&directory, &reactor, ESP_INSTANCE, RW).await {
            Ok(root) => {
                *self.esp.borrow_mut() = Some(Rc::new(root));
                mikros_rt::log!(
                    Info,
                    "process-manager: serving the EFI system partition as /system/boot/efi"
                );
            }
            Err(Status::PeerClosed) => {}
            Err(e) => mikros_rt::log!(Error, "process-manager: no EFI system partition: {e:?}"),
        }
    }

    /// Makes profiles apply, from `ready`.
    fn apply(&self, ready: Ready) {
        *self.ready.borrow_mut() = Some(Rc::new(ready));
    }

    /// The namespace of a new process of `app_id` and `user`, the service
    /// manager that init asked for when `service_manager`, and where the
    /// programs it names come from.
    pub async fn of(
        &self,
        shared: &Shared,
        app_id: &str,
        user: u32,
        admin: u32,
        service_manager: bool,
    ) -> Result<(Vec<NamespaceEntry>, Programs, Option<App>), Status> {
        let dev = NamespaceEntry {
            path: "/dev".into(),
            directory: shared.dev.directory()?,
        };
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
            return Ok((entries, Programs::boot(), None));
        };
        let reactor = &shared.reactor;
        let mut entries = Vec::new();
        let mut origins = Vec::new();
        let mut app = None;
        if service_manager {
            add(
                &mut entries,
                &mut origins,
                self.entry(shared, &ready, SYSTEM_ENTRY, RX).await?,
            );
            add(
                &mut entries,
                &mut origins,
                self.entry(shared, &ready, CONFIG, Rights::READ).await?,
            );
        } else if app_id == CLI_APP {
            // User 0 has no folder in /users, and so no name is needed.
            let name = match user {
                0 => None,
                _ => Some(crate::users::name(shared, user).await?),
            };
            add(
                &mut entries,
                &mut origins,
                self.entry(shared, &ready, SYSTEM_ENTRY, RX).await?,
            );
            add(
                &mut entries,
                &mut origins,
                self.entry(shared, &ready, CONFIG, Rights::READ).await?,
            );
            add(
                &mut entries,
                &mut origins,
                self.entry(shared, &ready, APPS, RX).await?,
            );
            let libraries = self.entry(shared, &ready, "/libraries", RX).await?;
            let libraries = match &name {
                None => libraries,
                Some(name) => {
                    self.libraries(shared, &ready, libraries, name, false)
                        .await?
                }
            };
            add(&mut entries, &mut origins, libraries);
            if let Some(name) = &name {
                let home = format!("/users/{name}");
                add(
                    &mut entries,
                    &mut origins,
                    self.entry(shared, &ready, &home, RWX).await?,
                );
                let apps = self.apps_read_only(shared, &ready, name, user, RX).await?;
                add(&mut entries, &mut origins, apps);
            }
            if let Some(commands) = self.commands(shared, user).await {
                add(&mut entries, &mut origins, commands);
            }
            let budget = shared.user_budget(user)?;
            let tmpfs = self.tmpfs().await;
            add(
                &mut entries,
                &mut origins,
                none(tmp(&tmpfs, &format!("cli/{user}"), "/tmp", budget, reactor).await?),
            );
        } else if !app_id.starts_with("system.") {
            // A native app, which runs for a user.
            if user == 0 {
                return Err(Status::AccessDenied);
            }
            let name = crate::users::name(shared, user).await?;
            let started = self
                .app(
                    shared,
                    &ready,
                    app_id,
                    user,
                    admin,
                    &name,
                    &mut entries,
                    &mut origins,
                )
                .await?;
            app = Some(started);
        } else {
            let (kind, listed) = manifest_entries(self, shared, app_id).await;
            if kind == Manifest::Program {
                add(
                    &mut entries,
                    &mut origins,
                    self.entry(shared, &ready, SYSTEM_ENTRY, RX).await?,
                );
                // A program whose manifest lists `/system/config` gets it
                // with the rights listed there instead.
                let lists_config = listed
                    .iter()
                    .any(|(path, _)| path::normalize(path).as_deref() == Some(CONFIG));
                if !lists_config {
                    add(
                        &mut entries,
                        &mut origins,
                        self.entry(shared, &ready, CONFIG, Rights::READ).await?,
                    );
                }
                let path = format!("apps/{user}/{app_id}");
                let budget = shared.budget_of(user, app_id)?;
                let tmpfs = self.tmpfs().await;
                add(
                    &mut entries,
                    &mut origins,
                    none(tmp(&tmpfs, &path, "/tmp", budget, reactor).await?),
                );
                if user != 0 {
                    // The folders of the user's app need the user's name.
                    return Err(Status::NotSupported);
                }
            }
            if kind == Manifest::Driver {
                // Its own folders for temporary files and state.
                let budget = shared.budget_of(user, app_id)?;
                let tmpfs = self.tmpfs().await;
                let at = format!("{SYSTEM_TMP}/{app_id}");
                let dir = format!("drivers/{app_id}");
                add(
                    &mut entries,
                    &mut origins,
                    none(tmp(&tmpfs, &dir, &at, budget, reactor).await?),
                );
                let state = format!("{STATE}/{app_id}");
                add(
                    &mut entries,
                    &mut origins,
                    self.entry(shared, &ready, &state, RW).await?,
                );
            }
            for (path, text) in listed {
                if path.contains("<user>") {
                    match user {
                        0 => continue,
                        _ => return Err(Status::NotSupported),
                    }
                }
                let path = path::normalize(&path).ok_or(Status::NotFound)?;
                // Only the accounts service reaches its data, which only
                // entries in `/system/state` can: `/system` itself comes
                // from the system version.
                let in_state = path == STATE || path.starts_with("/system/state/");
                if app_id != ACCOUNTS_APP && in_state && path::overlaps(&path, ACCOUNTS_DATA) {
                    return Err(Status::AccessDenied);
                }
                let rights = rights(&text).ok_or(Status::InvalidArgs)?;
                if path == SYSTEM_TMP || path.starts_with("/system/tmp/") {
                    // Only a service's own folder there.
                    let name = app_id.strip_prefix("system.").unwrap_or(app_id);
                    if kind != Manifest::Service || path != format!("{SYSTEM_TMP}/{name}") {
                        return Err(Status::InvalidArgs);
                    }
                    let budget = shared.budget_of(user, app_id)?;
                    let tmpfs = self.tmpfs().await;
                    let dir = format!("system/{name}");
                    add(
                        &mut entries,
                        &mut origins,
                        none(tmp(&tmpfs, &dir, &path, budget, reactor).await?),
                    );
                    continue;
                }
                add(
                    &mut entries,
                    &mut origins,
                    self.entry(shared, &ready, &path, rights).await?,
                );
            }
        }
        // An elevated process gets the folders of every user permission:
        // the user's home folder, read and write, `/apps`, read only, and
        // `/volumes`, whose permission the broker allows it.
        if admin != 0 {
            let name = match user {
                0 => None,
                _ => Some(crate::users::name(shared, user).await?),
            };
            let home = name.as_ref().map(|name| format!("/users/{name}"));
            let folders = home.clone().map(|h| (h, RW)).into_iter();
            for (path, rights) in folders.chain([(String::from(APPS), Rights::READ)]) {
                if !entries.iter().any(|e| e.path == path) {
                    let entry = self.entry(shared, &ready, &path, rights).await?;
                    add(&mut entries, &mut origins, entry);
                    // Its apps' folders stay read-only (MIKSPEC-0101.R42).
                    if let Some(name) = name.as_ref().filter(|_| Some(&path) == home.as_ref()) {
                        let apps = self.apps_read_only(shared, &ready, name, user, RX).await?;
                        add(&mut entries, &mut origins, apps);
                    }
                }
            }
        }
        if app_id == CLI_APP || self.holds_volumes(shared, app_id, user, admin).await {
            let volumes = NamespaceEntry {
                path: VOLUMES.into(),
                directory: shared.volumes.directory()?,
            };
            add(&mut entries, &mut origins, none(volumes));
        }
        add(&mut entries, &mut origins, none(dev));
        Ok((entries, Programs { entries: origins }, app))
    }

    /// The entries of the installed app `app_id` for user `user`, named
    /// `name` (MIKSPEC-0101.R7, MIKSPEC-0050.R45): `/system` and
    /// `/libraries`, `/apps/<id>`, a view of the installed package below
    /// the user's `data` folder of the app, its `cache` and its `package`,
    /// `/public` and, when it exists, `/public/<id>`, and a `/tmp` of its
    /// own; and how it starts.
    #[allow(clippy::too_many_arguments)]
    async fn app(
        &self,
        shared: &Shared,
        ready: &Ready,
        app_id: &str,
        user: u32,
        admin: u32,
        name: &str,
        entries: &mut Vec<NamespaceEntry>,
        origins: &mut Vec<(String, Origin)>,
    ) -> Result<App, Status> {
        if !mikros_mika::id::is_valid(app_id) {
            return Err(Status::InvalidArgs);
        }
        let reactor = &shared.reactor;
        // Installed for every user, or for this one.
        let global = format!("{APPS}/{app_id}");
        let local = format!("/users/{name}/apps/{app_id}/package");
        let (installed, mode) = match self.entry(shared, ready, &global, Rights::READ).await {
            Ok(_) => (global.clone(), "global"),
            Err(Status::NotFound) => (local.clone(), "local"),
            Err(e) => return Err(e),
        };
        let manifest = files::read(
            ready.roots.root(),
            reactor,
            &format!("{}/manifest.yaml", installed.trim_start_matches('/')),
            SYSTEM_FILE_MAX,
        )
        .await
        .map_err(|_| Status::NotFound)?;
        let manifest =
            mikros_mika::manifest::Manifest::parse(&manifest).map_err(|_| Status::InvalidArgs)?;
        if manifest.id != app_id {
            return Err(Status::InvalidArgs);
        }
        add(
            entries,
            origins,
            self.entry(shared, ready, SYSTEM_ENTRY, RX).await?,
        );
        // A local install also gets its user's libraries, which are looked
        // for first (MIKSPEC-0050.R50).
        let libraries = self.entry(shared, ready, "/libraries", RX).await?;
        let libraries = match mode {
            "local" => self.libraries(shared, ready, libraries, name, true).await?,
            _ => libraries,
        };
        add(entries, origins, libraries);
        if let Some(commands) = self.commands(shared, user).await {
            add(entries, origins, commands);
        }
        // The app's own folders of the user, made where they are missing:
        // the app's folder as a subvolume of its own (MIKSPEC-0101.R38).
        let folder = format!("/users/{name}/apps/{app_id}");
        let missing = matches!(
            self.entry(shared, ready, &folder, Rights::READ).await,
            Err(Status::NotFound)
        );
        if missing && matches!(ready.roots, Roots::Root(_)) {
            self.apps_folder(shared, ready, name, user).await?;
            match crate::users::subvolume(shared, folder.trim_start_matches('/')).await {
                // A root file system without subvolumes gets a folder.
                Ok(()) | Err(Status::NotSupported) => {}
                Err(e) => return Err(e),
            }
        }
        let (data, _) = self
            .entry(shared, ready, &format!("{folder}/data"), RWX)
            .await?;
        let (cache, _) = self
            .entry(shared, ready, &format!("{folder}/cache"), RWX)
            .await?;
        // They are the user's, as their home folder is.
        for path in [
            folder.clone(),
            format!("{folder}/data"),
            format!("{folder}/cache"),
        ] {
            self.own(shared, ready, &path, user).await;
        }
        let (package, _) = self.entry(shared, ready, &installed, RX).await?;
        let layers = alloc::vec![
            Layer {
                directory: data.directory,
                writable: true,
            },
            Layer {
                directory: package.directory,
                writable: false,
            },
        ];
        let view = NodeAsyncClient::new(
            crate::users::view(shared, layers, RWX).await?,
            reactor.clone(),
        );
        let at = format!("{APPS}/{app_id}");
        add(
            entries,
            origins,
            (
                NamespaceEntry {
                    path: at.clone(),
                    directory: connection(&view, RWX).await?,
                },
                Origin::Dir {
                    dir: Rc::new(view),
                    system: false,
                    real: None,
                    rights: RWX,
                },
            ),
        );
        add(
            entries,
            origins,
            none(NamespaceEntry {
                path: format!("{at}/cache"),
                directory: cache.directory,
            }),
        );
        // The package itself, writable for a local install and with
        // admin.apps.write.
        let writes = mode == "local" || self.holds(shared, app_id, user, admin, APPS_WRITE).await;
        let rights = if writes { RWX } else { RX };
        let (package, origin) = self.entry(shared, ready, &installed, rights).await?;
        add(
            entries,
            origins,
            (
                NamespaceEntry {
                    path: format!("{at}/package"),
                    directory: package.directory,
                },
                origin,
            ),
        );
        add(
            entries,
            origins,
            self.entry(shared, ready, PUBLIC, Rights::READ).await?,
        );
        if let Ok(own) = self
            .entry(shared, ready, &format!("{PUBLIC}/{app_id}"), RW)
            .await
        {
            add(entries, origins, own);
        }
        let budget = shared.budget_of(user, app_id)?;
        let tmpfs = self.tmpfs().await;
        let path = format!("apps/{user}/{app_id}");
        add(
            entries,
            origins,
            none(tmp(&tmpfs, &path, "/tmp", budget, reactor).await?),
        );
        let executable = manifest
            .executable
            .map(|e| format!("{at}/{e}"))
            .unwrap_or_default();
        Ok(App {
            folder: at,
            mode,
            executable,
            installed,
        })
    }

    /// The entry `/users/<name>/apps` of user `user` with `rights`, which
    /// lack `WRITE`, over a writable entry of the user's home folder, so
    /// that the user's installed apps, also those installed later, are
    /// written only as MIKSPEC-0101.R26 allows (MIKSPEC-0101.R42).
    async fn apps_read_only(
        &self,
        shared: &Shared,
        ready: &Ready,
        name: &str,
        user: u32,
        rights: Rights,
    ) -> Result<(NamespaceEntry, Origin), Status> {
        let path = self.apps_folder(shared, ready, name, user).await?;
        self.entry(shared, ready, &path, rights).await
    }

    /// The path of the folder of the apps of user `user`, named `name`,
    /// which is made, owned by the user, when it is missing.
    async fn apps_folder(
        &self,
        shared: &Shared,
        ready: &Ready,
        name: &str,
        user: u32,
    ) -> Result<String, Status> {
        let path = format!("/users/{name}/apps");
        if let Err(Status::NotFound) = self.entry(shared, ready, &path, Rights::READ).await {
            self.entry(shared, ready, &path, RWX).await?;
            self.own(shared, ready, &path, user).await;
        }
        Ok(path)
    }

    /// Makes `user` the owner of the folder `path` of the root table, when
    /// it can be reached; a folder that keeps its owner is still used.
    async fn own(&self, shared: &Shared, ready: &Ready, path: &str, user: u32) {
        if let Ok((entry, _)) = self.entry(shared, ready, path, RW).await {
            let dir = NodeAsyncClient::new(entry.directory, shared.reactor.clone());
            let _ = dir.set_owner(user).await;
        }
    }

    /// Whether the processes of `app_id` and `user`, elevated by `admin`
    /// unless it is 0, hold `permission`, as the broker says.
    async fn holds(
        &self,
        shared: &Shared,
        app_id: &str,
        user: u32,
        admin: u32,
        permission: &str,
    ) -> bool {
        let client = ClientIdentity {
            pid: 0,
            user,
            app_id: app_id.into(),
            admin,
            koid: 0,
        };
        shared.permissions.holds(client, false, permission).await
    }

    /// Whether the processes of `app_id` and `user`, elevated by `admin`
    /// unless it is 0, hold `user.volumes.access`, as the broker says.
    async fn holds_volumes(&self, shared: &Shared, app_id: &str, user: u32, admin: u32) -> bool {
        let client = ClientIdentity {
            pid: 0,
            user,
            app_id: app_id.into(),
            admin,
            koid: 0,
        };
        shared
            .permissions
            .holds(client, false, VOLUMES_PERMISSION)
            .await
    }

    /// `/libraries` for user `name`: a view of `system`, the
    /// system's, merged with the user's `libraries` folder, which a name
    /// resolves to first when `own_first`, or `system` alone when the user
    /// has none.
    async fn libraries(
        &self,
        shared: &Shared,
        ready: &Ready,
        system: (NamespaceEntry, Origin),
        name: &str,
        own_first: bool,
    ) -> Result<(NamespaceEntry, Origin), Status> {
        let path = format!("/users/{name}/libraries");
        let own = match self.entry(shared, ready, &path, RX).await {
            Ok((own, _)) => own,
            Err(e) => {
                mikros_rt::log!(
                    Warning,
                    "process-manager: {path} cannot be opened ({e:?}); /libraries is only the system's"
                );
                return Ok(system);
            }
        };
        let layers = match own_first {
            true => alloc::vec![own.directory, system.0.directory],
            false => alloc::vec![system.0.directory, own.directory],
        };
        let view = crate::users::merged(shared, layers, RX).await?;
        Ok(none(NamespaceEntry {
            path: "/libraries".into(),
            directory: view,
        }))
    }

    /// The entry `path` of the root table with `rights`, and where the
    /// programs below it come from. A path that does not exist is made
    /// when the rights hold `WRITE` and it lies on tmpfs, and fails with
    /// `NOT_FOUND` otherwise; rights beyond those of its root fail with
    /// `ACCESS_DENIED`.
    async fn entry(
        &self,
        shared: &Shared,
        ready: &Ready,
        path: &str,
        rights: Rights,
    ) -> Result<(NamespaceEntry, Origin), Status> {
        let normal = path.trim_end_matches('/');
        let root = ready.roots.root();
        if normal.is_empty() || path::overlaps(normal, SYSTEM) {
            return Err(Status::AccessDenied);
        }
        // The writable subvolumes, or a live system's views of them.
        let changing = match &ready.roots {
            Roots::Root(root) => {
                ["config", "state", "logs", "users"].map(|top| Source::Dir(root.clone(), top))
            }
            Roots::Live { views, .. } => views.clone().map(|view| Source::Dir(view, "")),
        };
        let [config, state, logs, users] = changing;
        let mut table: Vec<(&str, Source, Rights)> = alloc::vec![
            (SYSTEM_ENTRY, Source::Dir(root.clone(), "system"), RX),
            (CONFIG, config, RWX),
            (STATE, state, RWX),
            ("/system/logs", logs, RWX),
            (APPS, Source::Dir(root.clone(), "apps"), RWX),
            ("/libraries", Source::Dir(root.clone(), "libraries"), RX),
            (PUBLIC, Source::Dir(root.clone(), "public"), RW),
            ("/users", users, RWX),
        ];
        if let Some(esp) = self.esp.borrow().as_ref() {
            table.push((BOOT_EFI, Source::Dir(esp.clone(), ""), RW));
        }
        // The longest root that holds the path.
        let (top, source, most, rest) = table
            .into_iter()
            .filter_map(|(root, source, most)| {
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
            .max_by_key(|(root, ..)| root.len())
            .ok_or(Status::NotFound)?;
        // Missing directories are made only where services and drivers
        // keep configuration and state, and in users' folders.
        let creatable = matches!(top, CONFIG | STATE | "/system/logs" | "/users");
        let executable = rights.contains(Rights::EXECUTE);
        // The system's own programs are those of the root's /system; the
        // boot file system tells its own apart itself.
        let system = top == SYSTEM_ENTRY;
        if rights.0 & !most.0 != 0 {
            return Err(Status::AccessDenied);
        }
        let (client, server) = endpoints::<Node>().map_err(|_| Status::NoResources)?;
        match source {
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
                                real: Some(String::from(normal)),
                                rights,
                            },
                            false => Origin::Folder {
                                real: String::from(normal),
                                rights,
                            },
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
                        real: Some(String::from(normal)),
                        rights,
                    },
                    false => Origin::Folder {
                        real: String::from(normal),
                        rights,
                    },
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

/// An entry at `at` for the directory `path` of tmpfs, made if missing,
/// with read and write rights, whose files are charged to `budget`.
async fn tmp(
    tmpfs: &Tmpfs,
    path: &str,
    at: &str,
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
        path: at.into(),
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

/// The views of a live system's `/system/config`, `/system/state`,
/// `/system/logs` and `/users`, in that order: for each, tmpfs's
/// `live/config`, `live/state`, `live/logs` or `live/users` of `tmpfs`,
/// made where missing, over the subvolume of the same name of `root`, read
/// only.
async fn live_views(
    shared: &Shared,
    root: &NodeAsyncClient,
    tmpfs: &NodeAsyncClient,
    reactor: &Reactor,
) -> Result<[Rc<NodeAsyncClient>; 4], Status> {
    let mut made = Vec::new();
    for top in ["config", "state", "logs", "users"] {
        let upper = directories(tmpfs, &format!("live/{top}"), reactor)
            .await
            .map_err(status)?;
        let lower = open(root, top, OpenFlags::DIRECTORY, 0, Rights::READ, reactor)
            .await
            .map_err(status)?;
        let layers = alloc::vec![
            Layer {
                directory: connection(&upper, RWX).await?,
                writable: true,
            },
            Layer {
                directory: connection(&lower, Rights::READ).await?,
                writable: false,
            },
        ];
        let view = crate::users::view(shared, layers, RWX).await?;
        made.push(Rc::new(NodeAsyncClient::new(view, reactor.clone())));
    }
    made.try_into().map_err(|_| Status::Io)
}

/// A new connection to the directory `dir` with `rights`.
/// A connection of the process manager's own to `dir`, read and execute,
/// bound to `user` alone.
async fn bound(
    dir: &NodeAsyncClient,
    user: u32,
    shared: &Shared,
) -> Result<NodeAsyncClient, Status> {
    let own = NodeAsyncClient::new(connection(dir, RX).await?, shared.reactor.clone());
    let (client, server) = endpoints::<Node>().map_err(|_| Status::NoResources)?;
    flat(own.restrict(user, 0, 0, 0, 0, server).await).map_err(status)?;
    Ok(NodeAsyncClient::new(client, shared.reactor.clone()))
}

async fn connection(dir: &NodeAsyncClient, rights: Rights) -> Result<ClientEnd<Node>, Status> {
    let (client, server) = endpoints::<Node>().map_err(|_| Status::NoResources)?;
    flat(dir.clone(server, rights).await).map_err(status)?;
    Ok(client)
}

/// How an installed app starts (MIKSPEC-0101.R7, MIKSPEC-0101.R9): in
/// its folder, `/apps/<id>`, which is also its home, with its cache
/// below it, installed for every user (`global`) or for one (`local`),
/// the path of its program there, and the path of its package on the
/// root file system.
pub struct App {
    pub folder: String,
    pub mode: &'static str,
    pub executable: String,
    pub installed: String,
}

impl App {
    /// The app's package as the app sees it.
    pub fn package(&self) -> String {
        format!("{}/package", self.folder)
    }

    /// The path at which the app sees the file of its package at `real`
    /// on the root file system.
    pub fn sees(&self, real: &str) -> Option<String> {
        match real.strip_prefix(self.installed.as_str())? {
            "" => Some(self.package()),
            rest => rest
                .strip_prefix('/')
                .map(|rest| format!("{}/{rest}", self.package())),
        }
    }
}

/// The kind of manifest that an app ID has.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Manifest {
    /// None that is valid.
    None,
    Driver,
    Service,
    Program,
}

/// The kind of `app_id`'s manifest, and its namespace entries: a
/// driver's, a service's or a program's, with their paths and rights,
/// read from `/system` as `namespaces` serves it. Without a valid manifest
/// it has none.
async fn manifest_entries(
    namespaces: &Namespaces,
    shared: &Shared,
    app_id: &str,
) -> (Manifest, Vec<(String, String)>) {
    let pairs = |entries: Vec<mikros_manifest::NamespaceEntry>| {
        entries.into_iter().map(|e| (e.path, e.rights)).collect()
    };
    if app_id.starts_with(DRIVER_PREFIX) {
        let path = format!("drivers/{app_id}/driver.yaml");
        return match namespaces
            .system_file(shared, &path)
            .await
            .map(|b| mikros_manifest::parse_driver(app_id, &b))
        {
            Some(Ok(m)) => (Manifest::Driver, pairs(m.driver.namespace)),
            _ => (Manifest::None, Vec::new()),
        };
    }
    if !app_id.starts_with("system.") {
        return (Manifest::None, Vec::new());
    }
    let service = format!("services/{app_id}/{}", mikros_manifest::SERVICE_FILE);
    if let Some(bytes) = namespaces.system_file(shared, &service).await {
        return match mikros_manifest::parse(app_id, &bytes) {
            Ok(m) => (Manifest::Service, pairs(m.service.namespace)),
            Err(_) => (Manifest::None, Vec::new()),
        };
    }
    let program = format!("{app_id}.yaml");
    match namespaces
        .system_file(shared, &format!("programs/{program}"))
        .await
        .map(|b| mikros_manifest::parse_program(&program, &b))
    {
        Some(Ok(m)) => (Manifest::Program, pairs(m.program.namespace)),
        _ => (Manifest::None, Vec::new()),
    }
}
