//! A program that the service manager starts as a boot service after the
//! temporary file system and the union server, to drive views as a
//! client. It makes two layers on the temporary file system, a view of
//! them, and checks lookup, listing and refusals through it. It exits
//! with 0 when every check passed, or with the number of the first that
//! failed.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::string::String;
use alloc::vec::Vec;
use core::future::{Future, poll_fn};
use core::pin::pin;
use core::task::Poll;
use mikros_rt::exec::{Executor, Reactor, Spawner};
use mikros_rt::interfaces::__midl::{ClientEnd, endpoints};
use mikros_rt::interfaces::mikros::fs::{
    Attributes, Error, Filesystem, FilesystemClient, Layer, LockKind, MemoryFlags, Node,
    NodeAsyncClient, NodeAsyncServer, NodeClient, NodeCloneResponse, NodeGetAttributesResponse,
    NodeRequest, NodeType, NodeUnlockResponse, OpenFlags, Rights, Union, UnionClient,
};
use mikros_rt::interfaces::mikros::service::DirectoryClient;
use mikros_rt::{PANIC_EXIT_CODE, managed, sys};

mikros_rt::raw_main!(main);

const FOREVER: i64 = i64::MAX;
const RW: Rights = Rights(Rights::READ.0 | Rights::WRITE.0);

type Reply<T> = Result<Result<T, Error>, mikros_rt::interfaces::__midl::Error>;

fn ok<T>(reply: Reply<T>) -> Option<T> {
    reply.ok()?.ok()
}

fn err<T>(reply: Reply<T>) -> Option<Error> {
    reply.ok()?.err()
}

fn main(bootstrap: sys::Handle, _arg2: u64) -> i64 {
    let Ok(mut executor) = Executor::new() else {
        return PANIC_EXIT_CODE;
    };
    let reactor = executor.reactor().clone();
    let received = executor.block_on(managed::receive_async(bootstrap, reactor, |_| {}));
    let Ok(managed) = received else {
        return PANIC_EXIT_CODE;
    };
    let directory = DirectoryClient::new(managed.header.directory);
    match check(&directory, &mut executor) {
        Ok(()) => 0,
        Err(failed) => failed,
    }
}

/// Connects to `protocol` through `directory`.
fn connect(
    directory: &DirectoryClient,
    protocol: &str,
) -> Option<mikros_rt::interfaces::__midl::OwnedHandle> {
    let (ours, theirs) = mikros_rt::midl::channel_create().ok()?;
    // SAFETY: the channel was just created for us.
    let (ours, theirs) = unsafe {
        (
            mikros_rt::interfaces::__midl::OwnedHandle::from_raw(ours),
            mikros_rt::interfaces::__midl::OwnedHandle::from_raw(theirs),
        )
    };
    match directory.connect(protocol.into(), None, theirs, FOREVER) {
        Ok(Ok(_)) => Some(ours),
        _ => None,
    }
}

/// Opens `path` from `dir` with `flags` and `rights`.
fn open(
    dir: &NodeClient,
    path: &str,
    flags: OpenFlags,
    rights: Rights,
) -> Result<(NodeClient, NodeType), Option<Error>> {
    let (node, server) = endpoints::<Node>().map_err(|_| None)?;
    match dir.open(path.into(), flags, 0o644, rights, server, FOREVER) {
        Ok(Ok(opened)) => Ok((NodeClient::new(node), opened.r#type)),
        Ok(Err(e)) => Err(Some(e)),
        Err(_) => Err(None),
    }
}

/// Makes the file `path` under `dir` holding `data`.
fn file(dir: &NodeClient, path: &str, data: &[u8]) -> Option<()> {
    let create = OpenFlags(OpenFlags::CREATE.0 | OpenFlags::EXCLUSIVE.0);
    let (node, _) = open(dir, path, create, RW).ok()?;
    ok(node.write(0, data.to_vec(), FOREVER)).map(|_| ())
}

fn mkdir(dir: &NodeClient, path: &str) -> Option<NodeClient> {
    let flags = OpenFlags(OpenFlags::CREATE.0 | OpenFlags::DIRECTORY.0);
    open(dir, path, flags, RW).ok().map(|(n, _)| n)
}

fn expect(condition: bool, code: i64) -> Result<(), i64> {
    if condition { Ok(()) } else { Err(code) }
}

fn read(dir: &NodeClient, path: &str) -> Option<Vec<u8>> {
    let (node, _) = open(dir, path, OpenFlags(0), Rights::READ).ok()?;
    ok(node.read(0, 1000, FOREVER)).map(|r| r.data)
}

fn check(directory: &DirectoryClient, executor: &mut Executor) -> Result<(), i64> {
    let fs = FilesystemClient::new(ClientEnd::<Filesystem>::new(
        connect(directory, "mikros.fs.Filesystem").ok_or(1)?,
    ));
    let (root, server) = endpoints::<Node>().map_err(|_| 1)?;
    ok(fs.get_root(server, RW, FOREVER)).ok_or(1)?;
    // The probe works in a directory of its own, as other probes share
    // the file system.
    let root = mkdir(&NodeClient::new(root), "union-probe").ok_or(1)?;

    // The layers: the upper one hides `gone`, overrides `both.txt` and
    // adds to `d`.
    let lower = mkdir(&root, "lower").ok_or(2)?;
    let upper = mkdir(&root, "upper").ok_or(2)?;
    for (path, data) in [
        ("a.txt", &b"lower a"[..]),
        ("both.txt", b"lower both"),
        ("gone", b"x"),
    ] {
        file(&lower, path, data).ok_or(2)?;
    }
    mkdir(&lower, "d").ok_or(2)?;
    file(&lower, "d/b.txt", b"lower b").ok_or(2)?;
    file(&upper, "both.txt", b"upper both").ok_or(2)?;
    file(&upper, ".wh.gone", b"").ok_or(2)?;
    mkdir(&upper, "d").ok_or(2)?;
    file(&upper, "d/c.txt", b"upper c").ok_or(2)?;

    let union = UnionClient::new(ClientEnd::<Union>::new(
        connect(directory, "mikros.fs.Union").ok_or(3)?,
    ));
    let layer = |dir: &NodeClient, writable: bool| -> Option<Layer> {
        let (client, server) = endpoints::<Node>().ok()?;
        ok(dir.clone(server, Rights::READ, FOREVER))?;
        Some(Layer {
            directory: client,
            writable,
        })
    };
    let layers = alloc::vec![
        layer(&upper, false).ok_or(3)?,
        layer(&lower, false).ok_or(3)?
    ];
    let (view, server) = endpoints::<Node>().map_err(|_| 3)?;
    ok(union.create(layers, Rights::READ, server, FOREVER)).ok_or(3)?;
    let view = NodeClient::new(view);
    let bad = alloc::vec![
        layer(&upper, false).ok_or(4)?,
        layer(&lower, true).ok_or(4)?
    ];
    let (_, bad_server) = endpoints::<Node>().map_err(|_| 4)?;
    expect(
        err(union.create(bad, Rights::READ, bad_server, FOREVER)) == Some(Error::Invalid),
        4,
    )?;

    // Lookup: the first layer that has a name decides, and a whiteout
    // hides the layers after it.
    expect(
        read(&view, "both.txt").as_deref() == Some(&b"upper both"[..]),
        5,
    )?;
    expect(read(&view, "a.txt").as_deref() == Some(&b"lower a"[..]), 6)?;
    expect(
        matches!(
            open(&view, "gone", OpenFlags(0), Rights::READ),
            Err(Some(Error::NotFound))
        ),
        7,
    )?;
    expect(
        matches!(
            open(&view, ".wh.gone", OpenFlags(0), Rights::READ),
            Err(Some(Error::NotFound))
        ),
        7,
    )?;
    // Directories of one name merge.
    expect(
        read(&view, "d/b.txt").as_deref() == Some(&b"lower b"[..]),
        8,
    )?;
    expect(
        read(&view, "d/c.txt").as_deref() == Some(&b"upper c"[..]),
        8,
    )?;
    let (d, kind) = open(&view, "d", OpenFlags::DIRECTORY, Rights::READ).map_err(|_| 9)?;
    expect(kind == NodeType::Directory, 9)?;

    // Listings show each visible name once, without whiteouts.
    let names = |dir: &NodeClient| -> Option<Vec<String>> {
        Some(
            ok(dir.read_directory(0, false, FOREVER))?
                .entries
                .into_iter()
                .map(|e| e.name)
                .collect(),
        )
    };
    expect(names(&view).ok_or(10)? == ["both.txt", "d", "a.txt"], 10)?;
    expect(names(&d).ok_or(10)? == ["c.txt", "b.txt"], 10)?;

    // A view without a writable layer changes nothing.
    let write = open(&view, "a.txt", OpenFlags(0), RW);
    expect(matches!(write, Err(Some(Error::AccessDenied))), 11)?;
    let (reader, server) = endpoints::<Node>().map_err(|_| 12)?;
    ok(view.clone(server, Rights::READ, FOREVER)).ok_or(12)?;
    let reader = NodeClient::new(reader);
    expect(
        err(reader.unlink("a.txt".into(), false, FOREVER)) == Some(Error::AccessDenied),
        12,
    )?;
    // Even one whose rights allow writing.
    let layers = alloc::vec![
        layer(&upper, false).ok_or(13)?,
        layer(&lower, false).ok_or(13)?
    ];
    let (writer, server) = endpoints::<Node>().map_err(|_| 13)?;
    ok(union.create(layers, RW, server, FOREVER)).ok_or(13)?;
    let writer = NodeClient::new(writer);
    expect(
        matches!(
            open(&writer, "a.txt", OpenFlags(0), RW),
            Err(Some(Error::ReadOnly))
        ),
        13,
    )?;
    expect(
        err(writer.unlink("a.txt".into(), false, FOREVER)) == Some(Error::ReadOnly),
        13,
    )?;
    writes(&root, &union, &lower)?;
    revocable(&root, &union)?;
    quick_calls(&union, executor)?;
    waiting_locks(&root, &union, executor)
}

/// How long the open has to reach the backing node, and how long the view
/// has to answer a call that never waits.
const SETTLE: i64 = 200_000_000;
const QUICK: i64 = 3_000_000_000;

/// A connection of `view`'s to the file `name` with `READ` and `WRITE`.
async fn file_of(view: &NodeAsyncClient, reactor: &Reactor) -> Option<NodeAsyncClient> {
    let (node, server) = endpoints::<Node>().ok()?;
    ok(view.open("f".into(), OpenFlags(0), 0o644, RW, server).await)?;
    Some(NodeAsyncClient::new(node, reactor.clone()))
}

/// Waits `SETTLE`, which ends early with false when `lock` is done.
async fn settles<T>(reactor: &Reactor, lock: &mut (impl Future<Output = T> + Unpin)) -> bool {
    let mut now = 0;
    // SAFETY: the pointer is valid for the call.
    unsafe { sys::clock_get_monotonic(&mut now) };
    let mut settle = pin!(reactor.sleep_until(now + SETTLE));
    poll_fn(|cx| {
        if core::pin::Pin::new(&mut *lock).poll(cx).is_ready() {
            return Poll::Ready(false);
        }
        settle.as_mut().poll(cx).map(|_| true)
    })
    .await
}

/// Waits `SETTLE`.
async fn pause(reactor: &Reactor) {
    let mut now = 0;
    // SAFETY: the pointer is valid for the call.
    unsafe { sys::clock_get_monotonic(&mut now) };
    let _ = reactor.sleep_until(now + SETTLE).await;
}

/// Whether, through a view over the temporary file system's file, an
/// `Unlock` is answered while a `Lock` that waits is waiting, the `Lock`
/// is granted once the other holder lets go, and a `Lock` whose
/// connection closed meanwhile never takes the lock (MIKSPEC-0049.R38,
/// MIKSPEC-0051.R19).
fn waiting_locks(
    root: &NodeClient,
    union: &UnionClient,
    executor: &mut Executor,
) -> Result<(), i64> {
    let reactor = executor.reactor().clone();
    let dir = mkdir(root, "waiting").ok_or(46)?;
    file(&dir, "f", b"data").ok_or(46)?;
    let (backing, server) = endpoints::<Node>().map_err(|_| 46)?;
    ok(dir.clone(server, RW, FOREVER)).ok_or(46)?;
    let (view, view_server) = endpoints::<Node>().map_err(|_| 46)?;
    ok(union.create_revocable(backing, RW, 13, view_server, FOREVER)).ok_or(46)?;
    let view = NodeAsyncClient::new(view, reactor.clone());
    executor.block_on(async {
        let holder = file_of(&view, &reactor).await.ok_or(47)?;
        let waiter = file_of(&view, &reactor).await.ok_or(47)?;
        ok(holder.lock(LockKind::Exclusive, 0, 4, false).await).ok_or(48)?;
        let mut locking = pin!(waiter.lock(LockKind::Exclusive, 0, 4, true));
        if !settles(&reactor, &mut locking).await {
            return Err(49);
        }
        let mut now = 0;
        // SAFETY: the pointer is valid for the call.
        unsafe { sys::clock_get_monotonic(&mut now) };
        let mut timeout = pin!(reactor.sleep_until(now + QUICK));
        let mut unlocking = pin!(waiter.unlock(8, 1));
        let answered = poll_fn(|cx| {
            if locking.as_mut().poll(cx).is_ready() {
                return Poll::Ready(None);
            }
            if unlocking.as_mut().poll(cx).is_ready() {
                return Poll::Ready(Some(true));
            }
            match timeout.as_mut().poll(cx).is_ready() {
                true => Poll::Ready(Some(false)),
                false => Poll::Pending,
            }
        })
        .await;
        if answered != Some(true) {
            return Err(50);
        }
        ok(holder.unlock(0, 0).await).ok_or(51)?;
        ok(locking.await).ok_or(51)?;
        ok(waiter.unlock(0, 0).await).ok_or(51)?;
        // A waiting lock whose connection closes never takes the lock.
        let closing = file_of(&view, &reactor).await.ok_or(52)?;
        ok(holder.lock(LockKind::Exclusive, 0, 4, false).await).ok_or(52)?;
        {
            let mut locking = pin!(closing.lock(LockKind::Exclusive, 0, 4, true));
            if !settles(&reactor, &mut locking).await {
                return Err(53);
            }
        }
        drop(closing);
        pause(&reactor).await;
        ok(holder.unlock(0, 0).await).ok_or(54)?;
        let third = file_of(&view, &reactor).await.ok_or(54)?;
        ok(third.lock(LockKind::Exclusive, 0, 4, false).await).ok_or(55)?;
        Ok(())
    })
}

/// What a directory shows.
fn directory_attributes() -> Attributes {
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

/// A revocable view with `key` over a node that answers `GetAttributes`,
/// `Clone` and `Unlock`, and holds every `Open` and every waiting `Lock`
/// unanswered.
fn stalled_view(
    union: &UnionClient,
    key: u64,
    reactor: &Reactor,
    spawner: &Spawner,
) -> Result<NodeAsyncClient, i64> {
    let (backing, server) = endpoints::<Node>().map_err(|_| 40)?;
    let served = reactor.clone();
    spawner.spawn(async move {
        let server = NodeAsyncServer::new(server, served);
        // The calls of `Open` that are never answered.
        let mut held = Vec::new();
        let mut kept = Vec::new();
        let mut held_locks = Vec::new();
        while let Ok(request) = server.next().await {
            match request {
                NodeRequest::GetAttributes(_, r) => {
                    let attributes = directory_attributes();
                    let reply = NodeGetAttributesResponse { attributes };
                    let _ = r.reply(server.server(), Ok(reply));
                }
                NodeRequest::Open(q, r) => held.push((q, r)),
                NodeRequest::Lock(q, r) => held_locks.push((q, r)),
                NodeRequest::Unlock(_, r) => {
                    let _ = r.reply(server.server(), Ok(NodeUnlockResponse {}));
                }
                NodeRequest::Clone(q, r) => {
                    kept.push(q.node);
                    let _ = r.reply(server.server(), Ok(NodeCloneResponse {}));
                }
                // The view makes no other call while the test runs.
                _ => {}
            }
        }
    });
    let (view, view_server) = endpoints::<Node>().map_err(|_| 40)?;
    ok(union.create_revocable(backing, RW, key, view_server, FOREVER)).ok_or(40)?;
    Ok(NodeAsyncClient::new(view, reactor.clone()))
}

/// Whether a connection of a revocable view serves `GetAttributes` and
/// `Clone` while an `Open` of it waits for its backing node, which never
/// answers `Open` (MIKSPEC-0051.R18), and `Unlock` while a `Lock` that
/// waits is still waiting.
fn quick_calls(union: &UnionClient, executor: &mut Executor) -> Result<(), i64> {
    let (reactor, spawner) = (executor.reactor().clone(), executor.spawner().clone());
    let view = stalled_view(union, 11, &reactor, &spawner)?;
    let (_node, node_server) = endpoints::<Node>().map_err(|_| 40)?;
    let (_cloned, clone_server) = endpoints::<Node>().map_err(|_| 40)?;
    executor.block_on(async {
        let mut opening = pin!(view.open("f".into(), OpenFlags(0), 0o644, RW, node_server));
        // Let the open reach the backing node and wait there.
        let mut now = 0;
        // SAFETY: the pointer is valid for the call.
        unsafe { sys::clock_get_monotonic(&mut now) };
        let mut settle = pin!(reactor.sleep_until(now + SETTLE));
        let waited = poll_fn(|cx| {
            if opening.as_mut().poll(cx).is_ready() {
                return Poll::Ready(false);
            }
            settle.as_mut().poll(cx).map(|_| true)
        })
        .await;
        if !waited {
            return Err(41);
        }
        let mut timeout = pin!(reactor.sleep_until(now + QUICK));
        let mut attributes = pin!(view.get_attributes());
        let mut cloning = pin!(view.clone(clone_server, Rights::READ));
        let (mut got, mut cloned) = (false, false);
        poll_fn(|cx| {
            if opening.as_mut().poll(cx).is_ready() {
                return Poll::Ready(Err(41));
            }
            if !got && attributes.as_mut().poll(cx).is_ready() {
                got = true;
            }
            if !cloned && cloning.as_mut().poll(cx).is_ready() {
                cloned = true;
            }
            match (got && cloned, timeout.as_mut().poll(cx).is_ready()) {
                (true, _) => Poll::Ready(Ok(())),
                (false, true) => Poll::Ready(Err(42)),
                _ => Poll::Pending,
            }
        })
        .await
    })?;
    let view = stalled_view(union, 12, &reactor, &spawner)?;
    executor.block_on(async {
        let mut locking = pin!(view.lock(LockKind::Exclusive, 0, 0, true));
        let mut now = 0;
        // SAFETY: the pointer is valid for the call.
        unsafe { sys::clock_get_monotonic(&mut now) };
        let mut settle = pin!(reactor.sleep_until(now + SETTLE));
        let waited = poll_fn(|cx| {
            if locking.as_mut().poll(cx).is_ready() {
                return Poll::Ready(false);
            }
            settle.as_mut().poll(cx).map(|_| true)
        })
        .await;
        if !waited {
            return Err(43);
        }
        let mut timeout = pin!(reactor.sleep_until(now + QUICK));
        let mut unlocking = pin!(view.unlock(0, 0));
        poll_fn(|cx| {
            if locking.as_mut().poll(cx).is_ready() {
                return Poll::Ready(Err(43));
            }
            if unlocking.as_mut().poll(cx).is_ready() {
                return Poll::Ready(Ok(()));
            }
            match timeout.as_mut().poll(cx).is_ready() {
                true => Poll::Ready(Err(44)),
                false => Poll::Pending,
            }
        })
        .await
    })
}

/// Whether a view with a writable layer over `lower` changes only that
/// layer: files of the lower layer are copied up before they change,
/// removed names of the lower layer are whited out, a directory made over
/// a whiteout hides the lower one, and what views refuse is refused.
fn writes(root: &NodeClient, union: &UnionClient, lower: &NodeClient) -> Result<(), i64> {
    use OpenFlags as F;
    let upper = mkdir(root, "upper2").ok_or(20)?;
    mkdir(lower, "e").ok_or(20)?;
    file(lower, "e/x", b"lower x").ok_or(20)?;
    let layer = |dir: &NodeClient, rights: Rights, writable: bool| -> Option<Layer> {
        let (client, server) = endpoints::<Node>().ok()?;
        ok(dir.clone(server, rights, FOREVER))?;
        Some(Layer {
            directory: client,
            writable,
        })
    };
    let layers = alloc::vec![
        layer(&upper, RW, true).ok_or(20)?,
        layer(lower, Rights::READ, false).ok_or(20)?
    ];
    let (view, server) = endpoints::<Node>().map_err(|_| 20)?;
    ok(union.create(layers, RW, server, FOREVER)).ok_or(20)?;
    let view = NodeClient::new(view);

    // Copy-up: a lower file written through the view.
    let (a, _) = open(&view, "a.txt", F(0), RW).map_err(|_| 21)?;
    expect(ok(a.write(0, b"UPPER".to_vec(), FOREVER)).is_some(), 21)?;
    expect(read(&view, "a.txt").as_deref() == Some(&b"UPPER a"[..]), 21)?;
    expect(
        read(&upper, "a.txt").as_deref() == Some(&b"UPPER a"[..]),
        21,
    )?;
    expect(read(lower, "a.txt").as_deref() == Some(&b"lower a"[..]), 21)?;
    // Creating, in the writable layer, also below a lower directory.
    file(&view, "d/new.txt", b"new").ok_or(22)?;
    expect(
        read(&upper, "d/new.txt").as_deref() == Some(&b"new"[..]),
        22,
    )?;
    // Removing a lower name leaves a whiteout.
    expect(
        ok(view.unlink("both.txt".into(), false, FOREVER)).is_some(),
        23,
    )?;
    expect(
        matches!(
            open(&view, "both.txt", F(0), Rights::READ),
            Err(Some(Error::NotFound))
        ),
        23,
    )?;
    expect(read(&upper, ".wh.both.txt").is_some(), 23)?;
    expect(
        read(lower, "both.txt").as_deref() == Some(&b"lower both"[..]),
        23,
    )?;
    // A directory of the lower layer cannot be moved, nor removed while
    // it shows entries.
    let token = ok(view.get_token(FOREVER)).ok_or(24)?.token;
    expect(
        err(view.rename("d".into(), token, "d2".into(), FOREVER)) == Some(Error::CrossDevice),
        24,
    )?;
    expect(
        err(view.unlink("d".into(), true, FOREVER)) == Some(Error::NotEmpty),
        24,
    )?;
    // A file renamed out of a lower layer.
    let token = ok(view.get_token(FOREVER)).ok_or(25)?.token;
    expect(
        ok(view.rename("a.txt".into(), token, "moved.txt".into(), FOREVER)).is_some(),
        25,
    )?;
    expect(
        read(&view, "moved.txt").as_deref() == Some(&b"UPPER a"[..]),
        25,
    )?;
    expect(
        matches!(
            open(&view, "a.txt", F(0), Rights::READ),
            Err(Some(Error::NotFound))
        ),
        25,
    )?;
    // A lower directory removed and made again shows none of its old
    // entries.
    let (e, _) = open(&view, "e", F::DIRECTORY, RW).map_err(|_| 26)?;
    expect(ok(e.unlink("x".into(), false, FOREVER)).is_some(), 26)?;
    expect(ok(view.unlink("e".into(), true, FOREVER)).is_some(), 26)?;
    let (e, _) = open(&view, "e", flags(&[F::CREATE, F::DIRECTORY]), RW).map_err(|_| 26)?;
    file(lower, "e/y", b"lower y").ok_or(26)?;
    let names: Vec<String> = ok(e.read_directory(0, false, FOREVER))
        .ok_or(26)?
        .entries
        .into_iter()
        .map(|e| e.name)
        .collect();
    expect(names.is_empty(), 26)?;
    // Whiteout names are not made through a view.
    expect(
        matches!(
            open(&view, ".wh.z", F::CREATE, RW),
            Err(Some(Error::Invalid))
        ),
        27,
    )?;
    Ok(())
}

/// Whether a revocable view forwards to its directory, keeps clones and
/// opens in its domain, gives no shared memory object of a file system
/// that has no revocable ones, and closes everything once revoked; and
/// whether its key then serves a new view.
fn revocable(root: &NodeClient, union: &UnionClient) -> Result<(), i64> {
    let dir = mkdir(root, "rev").ok_or(30)?;
    file(&dir, "f", b"secret").ok_or(30)?;
    let backing = |dir: &NodeClient| -> Option<ClientEnd<Node>> {
        let (client, server) = endpoints::<Node>().ok()?;
        ok(dir.clone(server, RW, FOREVER))?;
        Some(client)
    };
    let (view, server) = endpoints::<Node>().map_err(|_| 30)?;
    ok(union.create_revocable(backing(&dir).ok_or(30)?, RW, 9, server, FOREVER)).ok_or(30)?;
    let view = NodeClient::new(view);
    let (f, _) = open(&view, "f", OpenFlags(0), RW).map_err(|_| 31)?;
    expect(ok(f.read(0, 100, FOREVER)).ok_or(31)?.data == b"secret", 31)?;
    expect(ok(f.write(0, b"S".to_vec(), FOREVER)).is_some(), 32)?;
    expect(read(&dir, "f").as_deref() == Some(&b"Secret"[..]), 32)?;
    let (other, server) = endpoints::<Node>().map_err(|_| 33)?;
    ok(view.clone(server, Rights::READ, FOREVER)).ok_or(33)?;
    let other = NodeClient::new(other);
    let shared = f.get_memory_object(MemoryFlags(0), FOREVER);
    expect(err(shared) == Some(Error::NotSupported), 34)?;
    expect(
        ok(f.get_memory_object(MemoryFlags::PRIVATE, FOREVER)).is_some(),
        34,
    )?;
    ok(union.revoke(9, FOREVER)).ok_or(35)?;
    // Revocation has closed every connection of the domain.
    expect(f.read(0, 1, FOREVER).is_err(), 36)?;
    expect(view.read_directory(0, false, FOREVER).is_err(), 36)?;
    expect(other.get_attributes(FOREVER).is_err(), 36)?;
    expect(ok(union.revoke(9, FOREVER)).is_some(), 37)?;
    expect(err(union.revoke(10, FOREVER)) == Some(Error::NotFound), 37)?;
    let (again, server) = endpoints::<Node>().map_err(|_| 38)?;
    ok(union.create_revocable(backing(&dir).ok_or(38)?, Rights::READ, 9, server, FOREVER))
        .ok_or(38)?;
    expect(
        read(&NodeClient::new(again), "f").as_deref() == Some(&b"Secret"[..]),
        38,
    )?;
    Ok(())
}

fn flags(bits: &[OpenFlags]) -> OpenFlags {
    OpenFlags(bits.iter().fold(0, |all, f| all | f.0))
}
