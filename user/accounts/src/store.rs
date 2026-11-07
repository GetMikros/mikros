//! The users as the file `users` of the service's data directory holds
//! them.

use core::cell::{Ref, RefCell};
use mikros_rt::exec::Reactor;
use mikros_rt::files::{self, Failure};
use mikros_rt::interfaces::__midl::endpoints;
use mikros_rt::interfaces::mikros::fs::{Error, Node, NodeAsyncClient, OpenFlags, Rights};
use mikros_userdb::Users;

const FILE: &str = "users";
/// The largest file of users read.
const FILE_MAX: usize = 16 << 20;

/// The data directory or its file could not be read, or the file is not
/// one that the service wrote.
#[derive(Debug)]
pub struct Unreadable;

/// The users the data directory's file holds.
pub struct Store {
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
            fs.open(at.into(), OpenFlags::DIRECTORY, 0, Rights::READ, server)
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
            users: RefCell::new(users),
        })
    }

    pub fn users(&self) -> Ref<'_, Users> {
        self.users.borrow()
    }
}
