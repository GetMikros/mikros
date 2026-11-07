//! A program that the service manager starts as a boot service after the
//! temporary file system, to drive it as a client. It connects to
//! `mikros.fs.Filesystem` through its directory and checks files,
//! directories, links, names and memory objects on the root it gets. It exits with 0
//! when every check passed, or with the number of the first that failed.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::string::String;
use alloc::vec::Vec;
use mikros_rt::exec::Executor;
use mikros_rt::interfaces::__midl::{ClientEnd, endpoints};
use mikros_rt::interfaces::mikros::fs::{
    Error, Filesystem, FilesystemClient, MemoryFlags, Node, NodeClient, NodeType, OpenFlags, Rights,
};
use mikros_rt::interfaces::mikros::service::DirectoryClient;
use mikros_rt::{PANIC_EXIT_CODE, managed, sys};

mikros_rt::raw_main!(main);

const FOREVER: i64 = i64::MAX;
const PROTOCOL: &str = "mikros.fs.Filesystem";
const RW: Rights = Rights(Rights::READ.0 | Rights::WRITE.0);
const PERM_READ: u32 = 1 << 0;
const PERM_WRITE: u32 = 1 << 1;
const PAGE: u64 = 4096;

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
    let process = managed.header.process.raw();
    let directory = DirectoryClient::new(managed.header.directory);
    let Some(root) = root(&directory) else {
        return 1;
    };
    match check(&root).and_then(|()| memory(&root, process)) {
        Ok(()) => 0,
        Err(failed) => failed,
    }
}

/// A connection, read and write, to the root of the file system.
fn root(directory: &DirectoryClient) -> Option<NodeClient> {
    let (client, server) = endpoints::<Filesystem>().ok()?;
    ok(directory
        .connect(PROTOCOL.into(), None, server.into_handle(), FOREVER)
        .map(|r| r.map_err(|_| Error::Io)))?;
    let fs = FilesystemClient::new(ClientEnd::new(client.into_handle()));
    let (node, node_server) = endpoints::<Node>().ok()?;
    ok(fs.get_root(node_server, RW, FOREVER))?;
    Some(NodeClient::new(node))
}

/// Opens `path` from `dir` with `flags` and `rights`.
fn open(
    dir: &NodeClient,
    path: &str,
    flags: OpenFlags,
    rights: Rights,
) -> Result<(NodeClient, NodeType, Option<String>), Option<Error>> {
    let (node, server) = endpoints::<Node>().map_err(|_| None)?;
    match dir.open(path.into(), flags, 0o640, rights, server, FOREVER) {
        Ok(Ok(opened)) => Ok((NodeClient::new(node), opened.r#type, opened.redirect)),
        Ok(Err(e)) => Err(Some(e)),
        Err(_) => Err(None),
    }
}

/// Fails with `code` unless `condition` holds.
fn expect(condition: bool, code: i64) -> Result<(), i64> {
    if condition { Ok(()) } else { Err(code) }
}

fn flags(bits: &[OpenFlags]) -> OpenFlags {
    OpenFlags(bits.iter().fold(0, |all, f| all | f.0))
}

fn check(root: &NodeClient) -> Result<(), i64> {
    use OpenFlags as F;
    let info = ok(root.query_filesystem(FOREVER)).ok_or(2)?.info;
    expect(info.r#type == "tmpfs" && !info.read_only, 2)?;

    // Directories and files, with a gap of zeros and a truncation.
    let (dir, kind, _) = open(root, "a", flags(&[F::CREATE, F::DIRECTORY]), RW).map_err(|_| 3)?;
    expect(kind == NodeType::Directory, 3)?;
    let (file, _, _) = open(&dir, "f", flags(&[F::CREATE, F::EXCLUSIVE]), RW).map_err(|_| 4)?;
    expect(
        matches!(
            open(&dir, "f", flags(&[F::CREATE, F::EXCLUSIVE]), RW),
            Err(Some(Error::Exists))
        ),
        4,
    )?;
    expect(ok(file.write(0, b"hello".to_vec(), FOREVER)).is_some(), 5)?;
    expect(ok(file.write(10000, b"x".to_vec(), FOREVER)).is_some(), 5)?;
    let attributes = ok(file.get_attributes(FOREVER)).ok_or(6)?.attributes;
    expect(
        attributes.size == 10001 && attributes.mode == 0o640 && attributes.links == 1,
        6,
    )?;
    let gap = ok(file.read(4, 10, FOREVER)).ok_or(7)?.data;
    expect(
        gap.len() == 10 && gap[0] == b'o' && gap[1..].iter().all(|&b| b == 0),
        7,
    )?;
    expect(ok(file.truncate(3, FOREVER)).is_some(), 8)?;
    expect(ok(file.read(0, 100, FOREVER)).ok_or(8)?.data == b"hel", 8)?;
    expect(
        ok(file.append(b"lo".to_vec(), FOREVER)).ok_or(8)?.offset == 3,
        8,
    )?;

    // Paths.
    expect(
        matches!(
            open(root, "a/../a", F(0), RW),
            Err(Some(Error::AccessDenied))
        ),
        9,
    )?;
    expect(
        matches!(open(root, "/a", F(0), RW), Err(Some(Error::Invalid))),
        9,
    )?;
    expect(
        matches!(
            open(root, "a/missing/f", F::CREATE, RW),
            Err(Some(Error::NotFound))
        ),
        9,
    )?;
    expect(
        matches!(open(root, "a/f/", F(0), RW), Err(Some(Error::NotDirectory))),
        9,
    )?;

    // Symbolic and hard links.
    expect(
        ok(root.create_symlink("l".into(), "a/f".into(), FOREVER)).is_some(),
        10,
    )?;
    let (through, kind, _) = open(root, "l", F(0), RW).map_err(|_| 10)?;
    expect(
        kind == NodeType::File && ok(through.read(0, 10, FOREVER)).ok_or(10)?.data == b"hello",
        10,
    )?;
    let (link, kind, _) = open(root, "l", F::NO_FOLLOW, Rights::READ).map_err(|_| 10)?;
    expect(
        kind == NodeType::Symlink && ok(link.read_link(FOREVER)).ok_or(10)?.target == "a/f",
        10,
    )?;
    expect(
        ok(root.create_symlink("abs".into(), "/etc".into(), FOREVER)).is_some(),
        11,
    )?;
    let redirect = open(root, "abs/passwd", F(0), Rights::READ)
        .map_err(|_| 11)?
        .2;
    expect(redirect.as_deref() == Some("/etc/passwd"), 11)?;
    let token = ok(root.get_token(FOREVER)).ok_or(12)?.token;
    expect(
        ok(dir.link("f".into(), token, "g".into(), FOREVER)).is_some(),
        12,
    )?;
    expect(
        ok(file.get_attributes(FOREVER)).ok_or(12)?.attributes.links == 2,
        12,
    )?;

    // Renaming, and removing a file that a connection still has open.
    let token = ok(root.get_token(FOREVER)).ok_or(13)?.token;
    expect(
        ok(dir.rename("f".into(), token, "h".into(), FOREVER)).is_some(),
        13,
    )?;
    expect(
        matches!(open(&dir, "f", F(0), RW), Err(Some(Error::NotFound))),
        13,
    )?;
    let names: Vec<String> = ok(root.read_directory(0, false, FOREVER))
        .ok_or(14)?
        .entries
        .into_iter()
        .map(|e| e.name)
        .collect();
    expect(names == ["a", "l", "abs", "g", "h"], 14)?;
    expect(ok(root.unlink("g".into(), false, FOREVER)).is_some(), 15)?;
    expect(ok(root.unlink("h".into(), false, FOREVER)).is_some(), 15)?;
    expect(
        ok(file.read(0, 10, FOREVER)).ok_or(15)?.data == b"hello",
        15,
    )?;
    expect(
        err(root.unlink("a".into(), false, FOREVER)) == Some(Error::IsDirectory),
        16,
    )?;
    expect(ok(root.unlink("a".into(), true, FOREVER)).is_some(), 16)?;

    // Rights.
    let (reader, _) = clone(root, Rights::READ).ok_or(17)?;
    expect(
        matches!(
            open(&reader, "new", F::CREATE, RW),
            Err(Some(Error::AccessDenied))
        ),
        17,
    )?;
    expect(
        err(reader.create_symlink("x".into(), "y".into(), FOREVER)) == Some(Error::AccessDenied),
        17,
    )?;
    Ok(())
}

/// Whether the memory objects of a new file behave: a shared one shows a
/// later write and writes back through a mapping, and a private one keeps
/// its changes.
fn memory(root: &NodeClient, process: sys::Handle) -> Result<(), i64> {
    use OpenFlags as F;
    let (file, _, _) = open(root, "m", F::CREATE, RW).map_err(|_| 18)?;
    expect(ok(file.write(0, b"memory".to_vec(), FOREVER)).is_some(), 18)?;
    let flags = MemoryFlags::WRITE;
    let shared = ok(file.get_memory_object(flags, FOREVER)).ok_or(18)?.memory;
    let view = map(shared.raw(), process, PERM_READ | PERM_WRITE).ok_or(18)?;
    expect(bytes(view, 6) == b"memory", 18)?;
    expect(ok(file.write(0, b"M".to_vec(), FOREVER)).is_some(), 19)?;
    expect(bytes(view, 6) == b"Memory", 19)?;
    // SAFETY: the object is mapped writable at `view` for a page.
    unsafe { core::ptr::write_volatile((view + 1) as *mut u8, b'E') };
    expect(
        ok(file.read(0, 6, FOREVER)).ok_or(20)?.data == b"MEmory",
        20,
    )?;
    let private = MemoryFlags(MemoryFlags::PRIVATE.0 | MemoryFlags::WRITE.0);
    let copy = ok(file.get_memory_object(private, FOREVER))
        .ok_or(21)?
        .memory;
    let copy = map(copy.raw(), process, PERM_READ | PERM_WRITE).ok_or(21)?;
    // SAFETY: the copy is mapped writable at `copy` for a page.
    unsafe { core::ptr::write_volatile(copy as *mut u8, b'x') };
    expect(bytes(copy, 6) == b"xEmory", 21)?;
    expect(
        ok(file.read(0, 6, FOREVER)).ok_or(21)?.data == b"MEmory",
        21,
    )?;
    let (reader, _) = clone(&file, Rights::READ).ok_or(22)?;
    let writable = reader.get_memory_object(MemoryFlags::WRITE, FOREVER);
    expect(err(writable) == Some(Error::AccessDenied), 22)?;
    Ok(())
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

/// A clone of `node` with `rights`.
fn clone(node: &NodeClient, rights: Rights) -> Option<(NodeClient, ())> {
    let (client, server) = endpoints::<Node>().ok()?;
    ok(node.clone(server, rights, FOREVER))?;
    Some((NodeClient::new(client), ()))
}
