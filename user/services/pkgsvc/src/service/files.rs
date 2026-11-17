//! The root file system, which pkgsvc connects to itself, its folders and
//! files by path from the root, the records it keeps there, and the
//! names of users, which the folders of local packages are named by.

use super::FOREVER;
use crate::commands::Owners;
use crate::records::Records;
use mikros_rt::interfaces::__midl::{ServerEnd, endpoints};
use mikros_rt::interfaces::mikros::base::Status;
use mikros_rt::interfaces::mikros::fs::btrfs::SubvolumesClient;
use mikros_rt::interfaces::mikros::fs::{
    Error, Filesystem, FilesystemClient, Node, NodeClient, NodeOpenResponse, NodeType, OpenFlags,
    Rights,
};
use mikros_rt::interfaces::mikros::service::DirectoryClient;
use mikros_rt::interfaces::mikros::session::{Accounts, AccountsClient};
use std::ops::Deref;
use std::sync::{Mutex, MutexGuard};

const FILESYSTEM: &str = "device.filesystem";
const ROOT: &str = "root";
const ACCOUNTS: &str = "mikros.session.Accounts";
const SUBVOLUMES: &str = "device.subvolumes";
/// The folder of the records, from the root, and their files in it: the
/// packages' and the commands' owners.
const RECORDS_FOLDER: &str = "state/pkgsvc";
const RECORDS_NAME: &str = "records";
const OWNERS_NAME: &str = "commands";
/// The most bytes one write moves.
const WRITE_MAX: usize = 32768;
/// The longest records file and manifest that are read.
const FILE_MAX: usize = 1 << 20;
const RW: Rights = Rights(Rights::READ.0 | Rights::WRITE.0);
const RWX: Rights = Rights(Rights::READ.0 | Rights::WRITE.0 | Rights::EXECUTE.0);

/// The status of a file system error.
pub fn status(e: Error) -> Status {
    match e {
        Error::NotFound => Status::NotFound,
        Error::Exists => Status::AlreadyExists,
        Error::AccessDenied | Error::ReadOnly => Status::AccessDenied,
        Error::NoSpace => Status::NoResources,
        Error::Invalid | Error::NameTooLong => Status::InvalidArgs,
        _ => Status::Io,
    }
}

fn flat<T>(
    reply: Result<Result<T, Error>, mikros_rt::interfaces::__midl::Error>,
) -> Result<T, Status> {
    match reply {
        Ok(Ok(value)) => Ok(value),
        Ok(Err(e)) => Err(status(e)),
        Err(_) => Err(Status::Io),
    }
}

/// The root directory of the root file system, read, write and execute. Its
/// calls go one at a time, as a connection's replies come in order.
pub struct Root {
    node: Mutex<NodeClient>,
    /// Held by the one change of the installed packages under way.
    changes: Mutex<()>,
}

/// A change of the installed packages, such as an install or a removal:
/// while it lasts no other change runs, and only through it are the
/// records and the commands' owners written, each read, changed and
/// written back in one step. It gives every other call of the root.
pub struct Txn<'a> {
    root: &'a Root,
    _held: MutexGuard<'a, ()>,
}

impl Deref for Txn<'_> {
    type Target = Root;

    fn deref(&self) -> &Root {
        self.root
    }
}

impl Txn<'_> {
    /// Applies `change` to the records, and writes them when it changed
    /// them.
    pub fn update_records<T>(&self, change: impl FnOnce(&mut Records) -> T) -> Result<T, Status> {
        let mut records = self.records()?;
        let before = records.encode();
        let result = change(&mut records);
        let after = records.encode();
        if after != before {
            self.write_state(RECORDS_NAME, &after)?;
        }
        Ok(result)
    }

    /// Applies `change` to the owners of commands (MIKSPEC-0101.R44), and
    /// writes them when it changed them.
    pub fn update_owners<T>(&self, change: impl FnOnce(&mut Owners) -> T) -> Result<T, Status> {
        let mut owners = self.owners()?;
        let before = owners.encode();
        let result = change(&mut owners);
        let after = owners.encode();
        if after != before {
            self.write_state(OWNERS_NAME, &after)?;
        }
        Ok(result)
    }
}

impl Root {
    pub fn connect(directory: &DirectoryClient) -> Result<Root, Status> {
        let (client, server) = endpoints::<Filesystem>().map_err(|_| Status::NoResources)?;
        match directory.connect(
            FILESYSTEM.into(),
            Some(ROOT.into()),
            server.into_handle(),
            FOREVER,
        ) {
            Ok(Ok(_)) => {}
            Ok(Err(status)) => return Err(status),
            Err(_) => return Err(Status::Io),
        }
        let fs = FilesystemClient::new(client);
        let (root, server) = endpoints::<Node>().map_err(|_| Status::NoResources)?;
        // Execute too, for the files that the folder of commands opens.
        flat(fs.get_root(server, RWX, FOREVER))?;
        Ok(Root {
            node: Mutex::new(NodeClient::new(root)),
            changes: Mutex::new(()),
        })
    }

    /// Starts a change of the installed packages, once the one under way,
    /// if any, has ended.
    pub fn begin(&self) -> Result<Txn<'_>, Status> {
        let held = self.changes.lock().map_err(|_| Status::Internal)?;
        Ok(Txn {
            root: self,
            _held: held,
        })
    }

    /// Opens `path` from the root with `flags` and `rights`.
    pub fn open(
        &self,
        path: &str,
        flags: OpenFlags,
        mode: u32,
        rights: Rights,
    ) -> Result<NodeClient, Status> {
        let (node, server) = endpoints::<Node>().map_err(|_| Status::NoResources)?;
        let root = self.node.lock().map_err(|_| Status::Internal)?;
        flat(root.open(path.into(), flags, mode, rights, server, FOREVER))?;
        Ok(NodeClient::new(node))
    }

    /// The contents of the file at `path`, at most `FILE_MAX` bytes.
    pub fn read(&self, path: &str) -> Result<Vec<u8>, Status> {
        self.read_at_most(path, FILE_MAX)
    }

    /// The contents of the file at `path`, at most `max` bytes.
    pub fn read_at_most(&self, path: &str, max: usize) -> Result<Vec<u8>, Status> {
        let file = self.open(path, OpenFlags(0), 0, Rights::READ)?;
        let mut bytes = Vec::new();
        loop {
            let data = flat(file.read(bytes.len() as u64, 32768, FOREVER))?.data;
            if data.is_empty() {
                return Ok(bytes);
            }
            bytes.extend_from_slice(&data);
            if bytes.len() > max {
                return Err(Status::OutOfRange);
            }
        }
    }

    /// The paths, relative to `path`, of the regular files below the
    /// folder at `path`, at any depth; none when there is no such folder.
    pub fn files(&self, path: &str) -> Result<Vec<String>, Status> {
        let mut found = Vec::new();
        let mut folders = vec![String::new()];
        while let Some(below) = folders.pop() {
            let at = match below.is_empty() {
                true => String::from(path),
                false => format!("{path}/{below}"),
            };
            let dir = match self.open(&at, OpenFlags::DIRECTORY, 0, Rights::READ) {
                Ok(dir) => dir,
                Err(Status::NotFound) => continue,
                Err(e) => return Err(e),
            };
            let mut cookie = 0;
            loop {
                let listed = flat(dir.read_directory(cookie, false, FOREVER))?;
                if listed.entries.is_empty() {
                    break;
                }
                for e in listed.entries {
                    if e.name == "." || e.name == ".." {
                        continue;
                    }
                    let relative = match below.is_empty() {
                        true => e.name,
                        false => format!("{below}/{}", e.name),
                    };
                    match e.r#type {
                        NodeType::Directory => folders.push(relative),
                        NodeType::File => found.push(relative),
                        _ => {}
                    }
                }
                cookie = listed.next;
            }
        }
        found.sort();
        Ok(found)
    }

    /// The names of the folders in the folder at `path`; none when it is
    /// missing.
    pub fn folders(&self, path: &str) -> Result<Vec<String>, Status> {
        let dir = match self.open(path, OpenFlags::DIRECTORY, 0, Rights::READ) {
            Ok(dir) => dir,
            Err(Status::NotFound) => return Ok(Vec::new()),
            Err(e) => return Err(e),
        };
        let mut names = Vec::new();
        let mut cookie = 0;
        loop {
            let listed = flat(dir.read_directory(cookie, false, FOREVER))?;
            if listed.entries.is_empty() {
                return Ok(names);
            }
            names.extend(
                listed
                    .entries
                    .into_iter()
                    .filter(|e| e.r#type == NodeType::Directory && e.name != "." && e.name != "..")
                    .map(|e| e.name),
            );
            cookie = listed.next;
        }
    }

    /// Makes the folder `path` and the folders above it where they are
    /// missing.
    pub fn make_folders(&self, path: &str) -> Result<(), Status> {
        let flags = OpenFlags(OpenFlags::CREATE.0 | OpenFlags::DIRECTORY.0);
        let mut at = String::new();
        for name in path.split('/') {
            if !at.is_empty() {
                at.push('/');
            }
            at.push_str(name);
            self.open(&at, flags, 0o755, RW)?;
        }
        Ok(())
    }

    /// Writes `contents` to a new file at `path` with `mode`, and syncs it.
    pub fn write_file(&self, path: &str, contents: &[u8], mode: u32) -> Result<(), Status> {
        let flags = OpenFlags(OpenFlags::CREATE.0 | OpenFlags::TRUNCATE.0);
        let file = self.open(path, flags, mode, RW)?;
        let mut at = 0;
        while at < contents.len() {
            let end = contents.len().min(at + WRITE_MAX);
            let written = flat(file.write(at as u64, contents[at..end].to_vec(), FOREVER))?.written;
            if written == 0 {
                return Err(Status::Io);
            }
            at += written as usize;
        }
        flat(file.sync(FOREVER)).map(|_| ())
    }

    /// Makes a symbolic link at `path` to `target`.
    pub fn symlink(&self, path: &str, target: &str) -> Result<(), Status> {
        let (dir, name) = match path.rsplit_once('/') {
            Some((dir, name)) => (self.open(dir, OpenFlags::DIRECTORY, 0, RW)?, name),
            None => return Err(Status::InvalidArgs),
        };
        flat(dir.create_symlink(name.into(), target.into(), FOREVER)).map(|_| ())
    }

    /// Puts `from` of the folder `parent` in the place of `to`: renamed
    /// to it when there is no `to`, and otherwise swapped with it in one
    /// step, after which the old `to`, now at `from`, a subvolume, is
    /// deleted (MIKSPEC-0101.R36), after a read-only snapshot of it is
    /// made at `keep`, a path from the root, when it is given
    /// (MIKSPEC-0101.R51).
    pub fn replace(
        &self,
        parent: &str,
        from: &str,
        to: &str,
        subvolumes: &Subvolumes,
        keep: Option<&str>,
    ) -> Result<(), Status> {
        let dir = self.open(parent, OpenFlags::DIRECTORY, 0, RW)?;
        let token = flat(dir.get_token(FOREVER))?.token;
        let exists = self
            .open(
                &format!("{parent}/{to}"),
                OpenFlags::DIRECTORY,
                0,
                Rights::READ,
            )
            .is_ok();
        if !exists {
            return flat(dir.rename(from.into(), token, to.into(), FOREVER)).map(|_| ());
        }
        flat(dir.exchange(from.into(), token, to.into(), FOREVER))?;
        let Some(keep) = keep else {
            return subvolumes.delete(&format!("{parent}/{from}"));
        };
        // A read-only subvolume cannot move to another folder, and is not
        // made writable while programs map its files: a read-only snapshot
        // of it is kept, which shares its data, and it goes.
        let (folder, _) = keep.rsplit_once('/').ok_or(Status::InvalidArgs)?;
        self.make_folders(folder)?;
        let old = format!("{parent}/{from}");
        subvolumes.snapshot(&old, keep, true)?;
        subvolumes.delete(&old)
    }

    /// Removes `path` and everything below it.
    pub fn remove_all(&self, path: &str) -> Result<(), Status> {
        let (parent, name) = path.rsplit_once('/').ok_or(Status::InvalidArgs)?;
        let dir = self.open(parent, OpenFlags::DIRECTORY, 0, RW)?;
        if flat(dir.unlink(name.into(), false, FOREVER)).is_ok() {
            return Ok(());
        }
        let below = self.open(path, OpenFlags::DIRECTORY, 0, RW)?;
        let mut names = Vec::new();
        let mut cookie = 0;
        loop {
            let listed = flat(below.read_directory(cookie, false, FOREVER))?;
            if listed.entries.is_empty() {
                break;
            }
            names.extend(
                listed
                    .entries
                    .into_iter()
                    .map(|e| e.name)
                    .filter(|n| n != "." && n != ".."),
            );
            cookie = listed.next;
        }
        for name in names {
            self.remove_all(&format!("{path}/{name}"))?;
        }
        flat(dir.unlink(name.into(), true, FOREVER)).map(|_| ())
    }

    /// Opens `path` from the root with `flags` and `rights` on `server`.
    pub fn open_on(
        &self,
        path: &str,
        flags: OpenFlags,
        rights: Rights,
        server: ServerEnd<Node>,
    ) -> Result<NodeOpenResponse, Status> {
        let root = self.node.lock().map_err(|_| Status::Internal)?;
        flat(root.open(path.into(), flags, 0, rights, server, FOREVER))
    }

    /// Writes `text` as the file `name` of the records' folder, through a
    /// new file that replaces the old one.
    fn write_state(&self, name: &str, text: &str) -> Result<(), Status> {
        self.make_folders(RECORDS_FOLDER)?;
        let new = format!("{RECORDS_FOLDER}/{name}.new");
        self.write_file(&new, text.as_bytes(), 0o600)?;
        let dir = self.open(RECORDS_FOLDER, OpenFlags::DIRECTORY, 0, RW)?;
        let token = flat(dir.get_token(FOREVER))?.token;
        flat(dir.rename(format!("{name}.new"), token, name.into(), FOREVER))?;
        flat(dir.sync(FOREVER)).map(|_| ())
    }

    /// The text of the file `name` of the records' folder, empty when
    /// there is none yet.
    fn state(&self, name: &str) -> Result<String, Status> {
        match self.read(&format!("{RECORDS_FOLDER}/{name}")) {
            Ok(bytes) => Ok(String::from_utf8_lossy(&bytes).into_owned()),
            Err(Status::NotFound) => Ok(String::new()),
            Err(e) => Err(e),
        }
    }

    /// The records, none when there are none yet. Each write replaces
    /// the whole file at once, so the records read are whole.
    pub fn records(&self) -> Result<Records, Status> {
        self.state(RECORDS_NAME).map(|text| Records::parse(&text))
    }

    /// The owners of commands, none when there are none yet.
    pub fn owners(&self) -> Result<Owners, Status> {
        self.state(OWNERS_NAME).map(|text| Owners::parse(&text))
    }
}

/// Every user's ID and name, from the accounts service.
pub fn users(directory: &DirectoryClient) -> Result<Vec<(u32, String)>, Status> {
    let (client, server) = endpoints::<Accounts>().map_err(|_| Status::NoResources)?;
    match directory.connect(ACCOUNTS.into(), None, server.into_handle(), FOREVER) {
        Ok(Ok(_)) => {}
        Ok(Err(status)) => return Err(status),
        Err(_) => return Err(Status::Io),
    }
    let accounts = AccountsClient::new(client);
    let mut found = Vec::new();
    let mut after = 0;
    loop {
        let listed = accounts
            .list_users(after, FOREVER)
            .map_err(|_| Status::Io)?;
        let Some(last) = listed.users.last().map(|u| u.id) else {
            return Ok(found);
        };
        found.extend(listed.users.into_iter().map(|u| (u.id, u.name)));
        after = last;
    }
}

/// The name of the user `user`, from the accounts service.
pub fn user_name(directory: &DirectoryClient, user: u32) -> Result<String, Status> {
    let (client, server) = endpoints::<Accounts>().map_err(|_| Status::NoResources)?;
    match directory.connect(ACCOUNTS.into(), None, server.into_handle(), FOREVER) {
        Ok(Ok(_)) => {}
        Ok(Err(status)) => return Err(status),
        Err(_) => return Err(Status::Io),
    }
    let accounts = AccountsClient::new(client);
    // The users are listed by ID from the one after `after`.
    let after = user.checked_sub(1).ok_or(Status::NotFound)?;
    let listed = accounts
        .list_users(after, FOREVER)
        .map_err(|_| Status::Io)?;
    listed
        .users
        .into_iter()
        .find(|u| u.id == user)
        .map(|u| u.name)
        .ok_or(Status::NotFound)
}

/// The subvolume management of the root file system. Its calls go one at
/// a time.
pub struct Subvolumes(Mutex<SubvolumesClient>);

impl Subvolumes {
    pub fn connect(directory: &DirectoryClient) -> Result<Subvolumes, Status> {
        let (client, server) = endpoints::<mikros_rt::interfaces::mikros::fs::btrfs::Subvolumes>()
            .map_err(|_| Status::NoResources)?;
        match directory.connect(
            SUBVOLUMES.into(),
            Some(ROOT.into()),
            server.into_handle(),
            FOREVER,
        ) {
            Ok(Ok(_)) => Ok(Subvolumes(Mutex::new(SubvolumesClient::new(client)))),
            Ok(Err(_)) => Err(Status::NotSupported),
            Err(_) => Err(Status::Io),
        }
    }

    pub fn create(&self, path: &str) -> Result<(), Status> {
        let client = self.0.lock().map_err(|_| Status::Internal)?;
        flat(client.create(path.into(), FOREVER)).map(|_| ())
    }

    pub fn delete(&self, path: &str) -> Result<(), Status> {
        let client = self.0.lock().map_err(|_| Status::Internal)?;
        flat(client.delete(path.into(), FOREVER)).map(|_| ())
    }

    /// Makes `path` a snapshot of the subvolume `source`, read-only when
    /// `read_only`.
    pub fn snapshot(&self, source: &str, path: &str, read_only: bool) -> Result<(), Status> {
        let client = self.0.lock().map_err(|_| Status::Internal)?;
        flat(client.snapshot(source.into(), path.into(), read_only, FOREVER)).map(|_| ())
    }

    pub fn set_read_only(&self, path: &str, read_only: bool) -> Result<(), Status> {
        let client = self.0.lock().map_err(|_| Status::Internal)?;
        flat(client.set_read_only(path.into(), read_only, FOREVER)).map(|_| ())
    }
}
