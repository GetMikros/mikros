//! The users as the file `users` of the service's data directory holds
//! them. A change is written to `users.new` first, which then replaces
//! `users`, so that the file always holds either the old users or the
//! new ones.

use alloc::vec::Vec;
use core::cell::{Ref, RefCell};
use mikros_rt::exec::Reactor;
use mikros_rt::files::{self, Failure};
use mikros_rt::interfaces::__midl::endpoints;
use mikros_rt::interfaces::mikros::fs::{Error, Node, NodeAsyncClient, OpenFlags, Rights};
use mikros_userdb::Users;

const FILE: &str = "users";
const NEW: &str = "users.new";
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
        Ok(Store {
            dir,
            users: RefCell::new(users),
        })
    }

    pub fn users(&self) -> Ref<'_, Users> {
        self.users.borrow()
    }

    /// Writes `users` to the file and then makes them the store's. On
    /// failure, the store and the file keep the users they had.
    pub async fn save(&self, users: Users, reactor: &Reactor) -> Result<(), Error> {
        let bytes = mikros_userdb::encode(&users);
        self.write_new(&bytes, reactor).await?;
        let token = flat(self.dir.get_token().await)?.token;
        flat(self.dir.rename(NEW.into(), token, FILE.into()).await)?;
        *self.users.borrow_mut() = users;
        Ok(())
    }

    /// Writes `bytes` to `NEW`, replacing what it held.
    async fn write_new(&self, bytes: &[u8], reactor: &Reactor) -> Result<(), Error> {
        let (client, server) = endpoints::<Node>().map_err(|_| Error::Io)?;
        let flags = OpenFlags(OpenFlags::CREATE.0 | OpenFlags::TRUNCATE.0);
        flat(self.dir.open(NEW.into(), flags, MODE, RW, server).await)?;
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
