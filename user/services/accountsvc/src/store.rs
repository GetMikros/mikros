//! The users as the file `users` of the service's data directory holds
//! them, and the IDs of the users whose home folders the service has made
//! their own, one decimal ID per line in the file `owned`. A change is
//! written to `<file>.new` first, which then replaces the file, so that
//! the file always holds either the old contents or the new ones.

use alloc::collections::BTreeSet;
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use core::cell::{Ref, RefCell};
use mikros_rt::exec::Reactor;
use mikros_rt::files::{self, Failure};
use mikros_rt::interfaces::__midl::endpoints;
use mikros_rt::interfaces::mikros::fs::{Error, Node, NodeAsyncClient, OpenFlags, Rights};
use mikros_userdb::Users;

const FILE: &str = "users";
/// The file of the users whose home folders are their own.
const OWNED: &str = "owned";
/// The largest file of users read.
const FILE_MAX: usize = 16 << 20;
/// The most bytes one `Write` takes.
const WRITE_MAX: usize = 32768;
/// The mode of the file: read and write for its owner only.
const MODE: u32 = 0o600;
const RW: Rights = Rights(Rights::READ.0 | Rights::WRITE.0);

/// The data directory or its file could not be read, or the file is not
/// one that the service wrote.
#[derive(Debug)]
pub struct Unreadable;

/// The data directory and the users its file holds.
pub struct Store {
    dir: NodeAsyncClient,
    users: RefCell<Users>,
    owned: RefCell<BTreeSet<u32>>,
}

type Reply<T> = Result<Result<T, Error>, mikros_rt::interfaces::__midl::Error>;

fn flat<T>(reply: Reply<T>) -> Result<T, Error> {
    reply.unwrap_or(Err(Error::Io))
}

impl Store {
    /// Opens the data directory at the relative path `at` under `fs` and
    /// reads its users. Without a file, there are none yet.
    pub async fn open(
        fs: &NodeAsyncClient,
        at: &str,
        reactor: &Reactor,
    ) -> Result<Store, Unreadable> {
        let (client, server) = endpoints::<Node>().map_err(|_| Unreadable)?;
        flat(
            fs.open(at.into(), OpenFlags::DIRECTORY, 0, RW, server)
                .await,
        )
        .map_err(|_| Unreadable)?;
        let dir = NodeAsyncClient::new(client, reactor.clone());
        let users = match files::read(&dir, reactor, FILE, FILE_MAX).await {
            Ok(bytes) => mikros_userdb::decode(&bytes).map_err(|_| Unreadable)?,
            Err(Failure::Fs(Error::NotFound)) => Users::new(),
            Err(_) => return Err(Unreadable),
        };
        let owned = match files::read(&dir, reactor, OWNED, FILE_MAX).await {
            Ok(bytes) => core::str::from_utf8(&bytes)
                .map_err(|_| Unreadable)?
                .lines()
                .map(|line| line.parse().map_err(|_| Unreadable))
                .collect::<Result<_, _>>()?,
            Err(Failure::Fs(Error::NotFound)) => BTreeSet::new(),
            Err(_) => return Err(Unreadable),
        };
        Ok(Store {
            dir,
            users: RefCell::new(users),
            owned: RefCell::new(owned),
        })
    }

    /// Whether the service has made the home folder of user `id` the
    /// user's own.
    pub fn owned(&self, id: u32) -> bool {
        self.owned.borrow().contains(&id)
    }

    /// Records that the service has made the home folder of user `id` the
    /// user's own.
    pub async fn mark_owned(&self, id: u32, reactor: &Reactor) -> Result<(), Error> {
        let mut owned = self.owned.borrow().clone();
        owned.insert(id);
        let text: String = owned.iter().map(|id| format!("{id}\n")).collect();
        self.replace(OWNED, text.as_bytes(), reactor).await?;
        *self.owned.borrow_mut() = owned;
        Ok(())
    }

    pub fn users(&self) -> Ref<'_, Users> {
        self.users.borrow()
    }

    /// Writes `users` to the file and then makes them the store's. On
    /// failure, the store and the file keep the users they had.
    pub async fn save(&self, users: Users, reactor: &Reactor) -> Result<(), Error> {
        let bytes = mikros_userdb::encode(&users);
        self.replace(FILE, &bytes, reactor).await?;
        *self.users.borrow_mut() = users;
        Ok(())
    }

    /// Replaces the file `name` with one that holds `bytes`, through
    /// `<name>.new`.
    async fn replace(&self, name: &str, bytes: &[u8], reactor: &Reactor) -> Result<(), Error> {
        let new = format!("{name}.new");
        self.write_new(&new, bytes, reactor).await?;
        let token = flat(self.dir.get_token().await)?.token;
        flat(self.dir.rename(new, token, name.into()).await)?;
        Ok(())
    }

    /// Writes `bytes` to the file `new`, replacing what it held.
    async fn write_new(&self, new: &str, bytes: &[u8], reactor: &Reactor) -> Result<(), Error> {
        let (client, server) = endpoints::<Node>().map_err(|_| Error::Io)?;
        let flags = OpenFlags(OpenFlags::CREATE.0 | OpenFlags::TRUNCATE.0);
        flat(self.dir.open(new.into(), flags, MODE, RW, server).await)?;
        let file = NodeAsyncClient::new(client, reactor.clone());
        let mut at = 0;
        while at < bytes.len() {
            let end = bytes.len().min(at + WRITE_MAX);
            let chunk: Vec<u8> = bytes[at..end].into();
            let written = flat(file.write(at as u64, chunk).await)?.written as usize;
            if written == 0 {
                return Err(Error::Io);
            }
            at += written;
        }
        flat(file.sync().await).map(|_| ())
    }
}
