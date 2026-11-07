//! As the driver of the test machine's root partition: whether the FAT
//! server serves the EFI system partition of the same disk through its
//! `filesystem` service as it should.

use crate::now;
use alloc::vec::Vec;
use mikros_rt::exec::Reactor;
use mikros_rt::interfaces::__midl::{ClientEnd, endpoints};
use mikros_rt::interfaces::mikros::fs::{
    Error, Filesystem, FilesystemAsyncClient, LockKind, MemoryFlags, Node, NodeAsyncClient,
    NodeType, OpenFlags, Rights,
};
use mikros_rt::interfaces::mikros::service::DirectoryAsyncClient;
use mikros_rt::sys;

/// The protocol of the service.
const FILESYSTEM: &str = "device.filesystem";
/// How long the FAT server has to publish the service.
const PUBLISH_LIMIT: i64 = 10_000_000_000;
const POLL: i64 = 20_000_000;
const PERM_READ: u32 = 1 << 0;
const PERM_WRITE: u32 = 1 << 1;
const PAGE: u64 = 4096;

type Reply<T> = Result<Result<T, Error>, mikros_rt::interfaces::__midl::Error>;

fn ok<T>(reply: Reply<T>) -> Option<T> {
    reply.ok()?.ok()
}

fn failed<T>(reply: Reply<T>) -> Option<Error> {
    reply.ok()?.err()
}

/// A connection to the root of the first `filesystem` service, read and
/// write, once it appears.
async fn root(directory: &DirectoryAsyncClient, reactor: &Reactor) -> Option<NodeAsyncClient> {
    let deadline = now() + PUBLISH_LIMIT;
    let instance = loop {
        if let Ok(Ok(listed)) = directory.list(FILESYSTEM.into()).await
            && let Some(instance) = listed.instances.into_iter().next()
        {
            break instance;
        }
        if now() > deadline {
            return None;
        }
        let _ = reactor.sleep_until(now() + POLL).await;
    };
    let (client, server) = endpoints::<Filesystem>().ok()?;
    let connected = directory
        .connect(FILESYSTEM.into(), Some(instance), server.into_handle())
        .await;
    connected.ok()?.ok()?;
    let fs = FilesystemAsyncClient::new(ClientEnd::new(client.into_handle()), reactor.clone());
    let (node, node_server) = endpoints::<Node>().ok()?;
    ok(fs
        .get_root(node_server, Rights(Rights::READ.0 | Rights::WRITE.0))
        .await)?;
    Some(NodeAsyncClient::new(node, reactor.clone()))
}

/// Opens `path` from `dir` with `flags` and read and write rights.
async fn open(
    dir: &NodeAsyncClient,
    path: &str,
    flags: OpenFlags,
    reactor: &Reactor,
) -> Result<(NodeAsyncClient, NodeType), Option<Error>> {
    let (node, server) = endpoints::<Node>().map_err(|_| None)?;
    let rights = Rights(Rights::READ.0 | Rights::WRITE.0);
    match dir.open(path.into(), flags, 0o644, rights, server).await {
        Ok(Ok(opened)) => Ok((NodeAsyncClient::new(node, reactor.clone()), opened.r#type)),
        Ok(Err(e)) => Err(Some(e)),
        Err(_) => Err(None),
    }
}

/// Whether the volume behaves: it is FAT32 and writable; the root lists
/// the `EFI` directory; the boot configuration reads by a name in
/// another case; a file created in a new directory is written, read back
/// and appended to; its shared memory object shows a later write, and a
/// private one keeps its own changes; bad names and `..` are refused;
/// byte-range locks of
/// two connections conflict, part of one is released, and a waiting
/// lock is granted once the other connection closes; a rename into the
/// root moves the file, whose open connection follows it; and the file
/// and directory are removed again, after which that connection finds
/// its file gone.
pub async fn works(
    directory: &DirectoryAsyncClient,
    reactor: &Reactor,
    process: sys::Handle,
) -> bool {
    check(directory, reactor, process).await.is_some()
}

async fn check(
    directory: &DirectoryAsyncClient,
    reactor: &Reactor,
    process: sys::Handle,
) -> Option<()> {
    let root = root(directory, reactor).await?;
    let info = ok(root.query_filesystem().await)?.info;
    expect(info.r#type == "fat32" && !info.read_only && info.free_bytes > 0)?;
    let listed = ok(root.read_directory(0, true).await)?.entries;
    expect(
        listed
            .iter()
            .any(|e| e.name == "EFI" && e.r#type == NodeType::Directory),
    )?;

    let (conf, kind) = open(&root, "efi/MIKROS/Boot.Conf", OpenFlags(0), reactor)
        .await
        .ok()?;
    expect(kind == NodeType::File)?;
    let text = ok(conf.read(0, 4096).await)?.data;
    expect(contains(&text, b"root=PARTUUID="))?;

    let create_dir = OpenFlags(OpenFlags::CREATE.0 | OpenFlags::DIRECTORY.0);
    let (folder, kind) = open(&root, "Test Folder", create_dir, reactor).await.ok()?;
    expect(kind == NodeType::Directory)?;
    let create = OpenFlags(OpenFlags::CREATE.0 | OpenFlags::EXCLUSIVE.0);
    let (file, _) = open(&folder, "A long file name.txt", create, reactor)
        .await
        .ok()?;
    expect(ok(file.write(0, b"hello fat".to_vec()).await)?.written == 9)?;
    expect(ok(file.append(b"!".to_vec()).await)?.offset == 9)?;
    let attributes = ok(file.get_attributes().await)?.attributes;
    expect(
        attributes.size == 10 && attributes.mode == 0o644 && attributes.r#type == NodeType::File,
    )?;
    expect(ok(file.read(0, 100).await)?.data == b"hello fat!")?;
    memory(&file, process).await?;
    let again = open(&folder, "a LONG file NAME.txt", create, reactor).await;
    expect(matches!(again, Err(Some(Error::Exists))))?;
    let bad = open(&root, "bad:name", OpenFlags::CREATE, reactor).await;
    expect(matches!(bad, Err(Some(Error::Invalid))))?;
    let up = open(&folder, "../EFI", OpenFlags(0), reactor).await;
    expect(matches!(up, Err(Some(Error::AccessDenied))))?;

    locks(&file, reactor).await?;

    let token = ok(root.get_token().await)?.token;
    ok(folder
        .rename("A long file name.txt".into(), token, "Moved.txt".into())
        .await)?;
    let (moved, _) = open(&root, "moved.txt", OpenFlags(0), reactor).await.ok()?;
    expect(ok(moved.read(0, 100).await)?.data == b"hello fat!")?;
    expect(ok(file.get_attributes().await)?.attributes.size == 10)?;
    ok(root.unlink("Moved.txt".into(), false).await)?;
    let gone = open(&root, "Moved.txt", OpenFlags(0), reactor).await;
    expect(matches!(gone, Err(Some(Error::NotFound))))?;
    expect(failed(file.get_attributes().await) == Some(Error::NotFound))?;
    ok(root.unlink("Test Folder".into(), true).await)?;
    ok(root.sync().await)?;
    Some(())
}

/// Whether the memory objects of `file`, which holds `hello fat!`,
/// behave. It holds the same afterwards.
async fn memory(file: &NodeAsyncClient, process: sys::Handle) -> Option<()> {
    let shared = ok(file.get_memory_object(MemoryFlags(0)).await)?.memory;
    let view = map(shared.raw(), process, PERM_READ)?;
    let page = bytes(view, PAGE as usize);
    expect(&page[..10] == b"hello fat!" && page[10..].iter().all(|&b| b == 0))?;
    ok(file.write(0, b"HELLO".to_vec()).await)?;
    expect(bytes(view, 10) == b"HELLO fat!")?;
    let flags = MemoryFlags(MemoryFlags::PRIVATE.0 | MemoryFlags::WRITE.0);
    let private = ok(file.get_memory_object(flags).await)?.memory;
    let copy = map(private.raw(), process, PERM_READ | PERM_WRITE)?;
    expect(bytes(copy, 10) == b"HELLO fat!")?;
    // SAFETY: the copy is mapped writable at `copy` for a page.
    unsafe { core::ptr::write_volatile(copy as *mut u8, b'J') };
    expect(bytes(copy, 10) == b"JELLO fat!")?;
    expect(ok(file.read(0, 10).await)?.data == b"HELLO fat!")?;
    let writable = file.get_memory_object(MemoryFlags::WRITE).await;
    expect(failed(writable) == Some(Error::NotSupported))?;
    ok(file.write(0, b"hello".to_vec()).await)?;
    Some(())
}

/// Maps a page of the memory object `mem` with `perms`.
fn map(mem: sys::Handle, process: sys::Handle, perms: u32) -> Option<u64> {
    let mut at = 0;
    // SAFETY: the out pointer is valid; the kernel picks a free range.
    let status = unsafe { sys::vm_map(process, perms, 0, mem, 0, PAGE, &mut at) };
    (status == mikros_rt::OK).then_some(at)
}

/// The first `n` bytes mapped at `at`, read anew each time.
fn bytes(at: u64, n: usize) -> Vec<u8> {
    // SAFETY: a page is mapped at `at`, and `n` is at most a page.
    (0..n)
        .map(|i| unsafe { core::ptr::read_volatile((at as *const u8).add(i)) })
        .collect()
}

/// Whether the locks of `file` and of a clone of it behave.
async fn locks(file: &NodeAsyncClient, reactor: &Reactor) -> Option<()> {
    let (other, server) = endpoints::<Node>().ok()?;
    ok(file
        .clone(server, Rights(Rights::READ.0 | Rights::WRITE.0))
        .await)?;
    let other = NodeAsyncClient::new(other, reactor.clone());
    ok(file.lock(LockKind::Exclusive, 0, 0, false).await)?;
    let shared = other.lock(LockKind::Shared, 0, 4, false).await;
    expect(failed(shared) == Some(Error::WouldBlock))?;
    // The exclusive lock reaches to the end of the file.
    let past = other.lock(LockKind::Exclusive, 1000, 1, false).await;
    expect(failed(past) == Some(Error::WouldBlock))?;
    ok(file.unlock(0, 4).await)?;
    ok(other.lock(LockKind::Shared, 0, 4, false).await)?;
    let mine = file.lock(LockKind::Exclusive, 2, 1, false).await;
    expect(failed(mine) == Some(Error::WouldBlock))?;
    // Closing the other connection releases its lock, which this one
    // waits for.
    drop(other);
    ok(file.lock(LockKind::Exclusive, 0, 4, true).await)?;
    ok(file.unlock(0, 0).await)?;
    Some(())
}

fn expect(condition: bool) -> Option<()> {
    condition.then_some(())
}

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack.windows(needle.len()).any(|w| w == needle)
}
