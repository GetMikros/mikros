//! The broker's files: once the `device.filesystem` instance `root` is
//! published, the broker reads the grants and the numbers of app IDs from
//! `state/permissions/` there, the folder that processes see as
//! `/system/state/permissions/`, and writes the files that changed anew a
//! second after a change, with the changes of that second, through a file
//! `<name>.new` that replaces the old one. A root that
//! cannot be written, as a live system's, is left as it is: the files are
//! then in `live/state/permissions/` of the temporary file system, which
//! processes of a live system see there instead. Missing directories are
//! made.

use crate::permissions::Store;
use alloc::format;
use alloc::rc::Rc;
use alloc::vec::Vec;
use core::cell::{Cell, RefCell};
use core::future::poll_fn;
use core::task::{Poll, Waker};
use mikros_rt::exec::Reactor;
use mikros_rt::interfaces::__midl::endpoints;
use mikros_rt::interfaces::mikros::fs::{
    Error, Filesystem, FilesystemAsyncClient, Node, NodeAsyncClient, OpenFlags, Rights,
};
use mikros_rt::interfaces::mikros::service::{DirectoryAsyncClient, DirectoryEvent};

/// The service the root file system is published as, and its instance.
const FILESYSTEM: &str = "device.filesystem";
const ROOT: &str = "root";
/// The protocol of the temporary file system.
const TMPFS: &str = "mikros.fs.Filesystem";
/// Where the files are on the root, and on the temporary file system of
/// a live system.
const FOLDER: [&str; 2] = ["state", "permissions"];
const LIVE_FOLDER: [&str; 3] = ["live", "state", "permissions"];
const GRANTS: &str = "grants";
const NUMBERS: &str = "app-numbers";
/// The most bytes a file holds that the broker reads.
const FILE_MAX: usize = 16 << 20;
const WRITE_MAX: usize = 32768;
/// How long the broker waits after a change before it writes its files,
/// in nanoseconds, so that the changes of that while go out together.
const SAVE_DELAY: i64 = 1_000_000_000;
const MODE: u32 = 0o600;
const RW: Rights = Rights(Rights::READ.0 | Rights::WRITE.0);

type Reply<T, E = Error> = Result<Result<T, E>, mikros_rt::interfaces::__midl::Error>;

fn ok<T, E>(reply: Reply<T, E>) -> Option<T> {
    reply.ok()?.ok()
}

/// Whether the stored grants and numbers have been read, and whether the
/// store changed since they were last written, with the tasks that wait
/// for either.
#[derive(Default)]
pub struct Files {
    loaded: Cell<bool>,
    changed: Cell<bool>,
    waiting: RefCell<Vec<Waker>>,
    writer: RefCell<Option<Waker>>,
}

impl Files {
    /// Waits until the stored numbers have been read, so that no number
    /// is given that a stored one has.
    pub async fn loaded(&self) {
        poll_fn(|cx| match self.loaded.get() {
            true => Poll::Ready(()),
            false => {
                self.waiting.borrow_mut().push(cx.waker().clone());
                Poll::Pending
            }
        })
        .await
    }

    /// Notes that the store changed, so that its files are written anew.
    pub fn changed(&self) {
        self.changed.set(true);
        if let Some(waker) = self.writer.take() {
            waker.wake();
        }
    }

    fn set_loaded(&self) {
        self.loaded.set(true);
        for waker in self.waiting.take() {
            waker.wake();
        }
    }

    async fn next_change(&self) {
        poll_fn(|cx| match self.changed.replace(false) {
            true => Poll::Ready(()),
            false => {
                *self.writer.borrow_mut() = Some(cx.waker().clone());
                Poll::Pending
            }
        })
        .await
    }
}

/// Reads the stored grants and numbers into `store` once the folder can
/// be reached through `directory`, and writes those that changed anew a
/// second after each change from then on. Without the folder, nothing is read or written,
/// and numbers wait.
pub async fn keep(
    store: Rc<RefCell<Store>>,
    files: Rc<Files>,
    directory: Rc<DirectoryAsyncClient>,
    reactor: Reactor,
) {
    let folder = match root(&directory, &reactor).await {
        Ok(fs) => made(fs, &FOLDER, &reactor).await,
        Err(Some(Error::ReadOnly)) => match tmpfs(&directory, &reactor).await {
            Ok(fs) => made(fs, &LIVE_FOLDER, &reactor).await,
            Err(_) => None,
        },
        Err(_) => None,
    };
    let Some(folder) = folder else {
        mikros_rt::log!(
            Error,
            "permbroker: the folder of its grants cannot be reached"
        );
        return;
    };
    // A file that is missing holds nothing yet; one that cannot be read
    // keeps the store from giving numbers that it might hold.
    let numbers = match read(&folder, NUMBERS, &reactor).await {
        Ok(bytes) => bytes,
        Err(e) => {
            mikros_rt::log!(Error, "permbroker: reading {NUMBERS} failed: {e:?}");
            return;
        }
    };
    let grants = read(&folder, GRANTS, &reactor).await.unwrap_or_else(|e| {
        mikros_rt::log!(Error, "permbroker: reading {GRANTS} failed: {e:?}");
        Vec::new()
    });
    {
        let mut store = store.borrow_mut();
        for (name, skipped) in [
            (NUMBERS, store.load_numbers(&numbers)),
            (GRANTS, store.load_grants(&grants)),
        ] {
            if skipped != 0 {
                mikros_rt::log!(
                    Warning,
                    "permbroker: {skipped} lines of {name} were left out"
                );
            }
        }
    }
    files.set_loaded();
    // Grants made before the files were read are written with the rest.
    files.changed();
    // What each file holds, so that one that did not change is not
    // written again.
    let mut written = [numbers, grants];
    loop {
        files.next_change().await;
        // The changes of a while go out together, as each file that is
        // written commits the file system.
        let mut now = 0;
        // SAFETY: the out pointer is valid for the call.
        unsafe { mikros_rt::sys::clock_get_monotonic(&mut now) };
        if reactor.sleep_until(now + SAVE_DELAY).await.is_err() {
            return;
        }
        files.changed.set(false);
        let (numbers, grants) = {
            let store = store.borrow();
            (store.encode_numbers(), store.encode_grants())
        };
        for (at, (name, bytes)) in [(NUMBERS, numbers), (GRANTS, grants)]
            .into_iter()
            .enumerate()
        {
            if bytes == written[at] {
                continue;
            }
            match replace(&folder, name, &bytes, &reactor).await {
                Ok(()) => written[at] = bytes,
                Err(e) => mikros_rt::log!(Error, "permbroker: writing {name} failed: {e:?}"),
            }
        }
    }
}

/// The root directory of the root file system, read and write, once it
/// is published: `READ_ONLY` when it cannot be written.
async fn root(
    directory: &DirectoryAsyncClient,
    reactor: &Reactor,
) -> Result<NodeAsyncClient, Option<Error>> {
    ok(directory.watch(FILESYSTEM.into()).await).ok_or(None)?;
    loop {
        let listed = match directory.list(FILESYSTEM.into()).await {
            Ok(Ok(listed)) => listed.instances,
            // The service has no instances until a first one is
            // registered.
            Ok(Err(_)) => Vec::new(),
            Err(_) => return Err(None),
        };
        if listed.iter().any(|i| i == ROOT) {
            return root_of(directory, FILESYSTEM, Some(ROOT), reactor).await;
        }
        loop {
            match directory.next_event().await {
                Ok(DirectoryEvent::OnInstanceAdded(added)) if added.protocol == FILESYSTEM => break,
                Ok(_) => {}
                Err(_) => return Err(None),
            }
        }
    }
}

async fn tmpfs(
    directory: &DirectoryAsyncClient,
    reactor: &Reactor,
) -> Result<NodeAsyncClient, Option<Error>> {
    root_of(directory, TMPFS, None, reactor).await
}

/// The root directory, read and write, of the file system that
/// `protocol` and `instance` lead to, or the error of `GetRoot`.
async fn root_of(
    directory: &DirectoryAsyncClient,
    protocol: &str,
    instance: Option<&str>,
    reactor: &Reactor,
) -> Result<NodeAsyncClient, Option<Error>> {
    let (client, server) = endpoints::<Filesystem>().map_err(|_| None)?;
    let connected = directory
        .connect(
            protocol.into(),
            instance.map(Into::into),
            server.into_handle(),
        )
        .await;
    ok(connected).ok_or(None)?;
    let fs = FilesystemAsyncClient::new(client, reactor.clone());
    let (root, server) = endpoints::<Node>().map_err(|_| None)?;
    flat(fs.get_root(server, RW).await).map_err(Some)?;
    Ok(NodeAsyncClient::new(root, reactor.clone()))
}

/// The directory at `path` below `dir`, made where it is missing.
async fn made(
    mut dir: NodeAsyncClient,
    path: &[&str],
    reactor: &Reactor,
) -> Option<NodeAsyncClient> {
    for name in path {
        let (next, server) = endpoints::<Node>().ok()?;
        let flags = OpenFlags(OpenFlags::CREATE.0 | OpenFlags::DIRECTORY.0);
        ok(dir.open((*name).into(), flags, 0o700, RW, server).await)?;
        dir = NodeAsyncClient::new(next, reactor.clone());
    }
    Some(dir)
}

/// The bytes of the file `name` in `folder`, none when it is missing.
async fn read(folder: &NodeAsyncClient, name: &str, reactor: &Reactor) -> Result<Vec<u8>, Error> {
    let (client, server) = endpoints::<Node>().map_err(|_| Error::Io)?;
    match flat(
        folder
            .open(name.into(), OpenFlags(0), 0, Rights::READ, server)
            .await,
    ) {
        Ok(_) => {}
        Err(Error::NotFound) => return Ok(Vec::new()),
        Err(e) => return Err(e),
    }
    let file = NodeAsyncClient::new(client, reactor.clone());
    let mut bytes = Vec::new();
    loop {
        let data = flat(file.read(bytes.len() as u64, WRITE_MAX as u32).await)?.data;
        if data.is_empty() {
            return Ok(bytes);
        }
        bytes.extend_from_slice(&data);
        if bytes.len() > FILE_MAX {
            return Err(Error::TooBig);
        }
    }
}

/// Replaces the file `name` in `folder` with one that holds `bytes`.
async fn replace(
    folder: &NodeAsyncClient,
    name: &str,
    bytes: &[u8],
    reactor: &Reactor,
) -> Result<(), Error> {
    let new = format!("{name}.new");
    let (client, server) = endpoints::<Node>().map_err(|_| Error::Io)?;
    let flags = OpenFlags(OpenFlags::CREATE.0 | OpenFlags::TRUNCATE.0);
    flat(folder.open(new.clone(), flags, MODE, RW, server).await)?;
    let file = NodeAsyncClient::new(client, reactor.clone());
    let mut at = 0;
    while at < bytes.len() {
        let end = bytes.len().min(at + WRITE_MAX);
        let written = flat(file.write(at as u64, bytes[at..end].into()).await)?.written as usize;
        if written == 0 {
            return Err(Error::Io);
        }
        at += written;
    }
    flat(file.sync().await)?;
    let token = flat(folder.get_token().await)?.token;
    flat(folder.rename(new, token, name.into()).await)?;
    flat(folder.sync().await).map(|_| ())
}

fn flat<T>(reply: Reply<T>) -> Result<T, Error> {
    match reply {
        Ok(result) => result,
        Err(_) => Err(Error::Io),
    }
}
