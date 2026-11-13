//! A program that the service manager starts as a boot service after the
//! temporary file system, to drive it as a client. It connects to
//! `mikros.fs.Filesystem` and `mikros.fs.TemporaryFilesystem` through its
//! directory and checks that a budget set on a directory is charged for
//! the files below it, for which it gets the root budget. The system test
//! services-tmpfs checks the rest of the file system as a client of the
//! running one. It exits with 0 when the check passed, or with the number
//! of the step that failed.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::string::String;
use mikros_rt::exec::Executor;
use mikros_rt::interfaces::__midl::{ClientEnd, OwnedHandle, endpoints};
use mikros_rt::interfaces::mikros::fs::{
    Error, Filesystem, FilesystemClient, Node, NodeClient, NodeType, OpenFlags, Rights,
    TemporaryFilesystem, TemporaryFilesystemClient,
};
use mikros_rt::interfaces::mikros::process::HandleRole;
use mikros_rt::interfaces::mikros::service::DirectoryClient;
use mikros_rt::{PANIC_EXIT_CODE, managed, sys};

mikros_rt::raw_main!(main);

const FOREVER: i64 = i64::MAX;
const PROTOCOL: &str = "mikros.fs.Filesystem";
const TEMPORARY: &str = "mikros.fs.TemporaryFilesystem";
/// The startup handle ID of the root budget, a boot handle.
const ROOT_BUDGET_ID: u32 = 3;
const RW: Rights = Rights(Rights::READ.0 | Rights::WRITE.0);
const PAGE: u64 = 4096;

type Reply<T> = Result<Result<T, Error>, mikros_rt::interfaces::__midl::Error>;

fn ok<T>(reply: Reply<T>) -> Option<T> {
    reply.ok()?.ok()
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
    let budget = managed
        .handles
        .iter()
        .find(|h| h.role == HandleRole::User && h.id == ROOT_BUDGET_ID)
        .map(|h| h.handle.raw());
    let directory = DirectoryClient::new(managed.header.directory);
    // The probe works in a directory of its own, as other probes share
    // the file system.
    let Some(root) = root(&directory).and_then(|fs| own_directory(&fs)) else {
        return 1;
    };
    match budgets(&root, &directory, budget) {
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

/// The probe's own directory under `root`, read and write.
fn own_directory(root: &NodeClient) -> Option<NodeClient> {
    let flags = OpenFlags(OpenFlags::CREATE.0 | OpenFlags::DIRECTORY.0 | OpenFlags::EXCLUSIVE.0);
    open(root, "tmpfs-probe", flags, RW)
        .ok()
        .map(|(dir, _, _)| dir)
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

/// The pages charged to `budget`.
fn pages(budget: sys::Handle) -> Option<u64> {
    let mut record = [0u8; 0x38];
    let mut len = 0;
    // SAFETY: the pointers describe `record` and `len`.
    let status = unsafe { sys::object_get_info(budget, 6, record.as_mut_ptr(), 0x38, &mut len) };
    let pages = u64::from_le_bytes(record[0x18..0x20].try_into().ok()?);
    (status == mikros_rt::OK).then_some(pages)
}

/// A copy of `handle` with `rights`.
fn copy(handle: sys::Handle, rights: u32) -> Option<OwnedHandle> {
    let mut out = 0;
    // SAFETY: the out pointer is valid for the call.
    let status = unsafe { sys::handle_duplicate(handle, rights, &mut out) };
    // SAFETY: the kernel just created the handle for us.
    (status == mikros_rt::OK).then(|| unsafe { OwnedHandle::from_raw(out) })
}

/// `SetBudget` charges the memory of the files below a directory to a
/// budget with `WRITE`, and nothing outside it; the budget's limit holds;
/// a file that goes releases its pages.
fn budgets(
    root: &NodeClient,
    directory: &DirectoryClient,
    parent: Option<sys::Handle>,
) -> Result<(), i64> {
    use OpenFlags as F;
    const SAME_RIGHTS: u32 = 1 << 31;
    const DUPLICATE: u32 = 1 << 0;
    const TRANSFER: u32 = 1 << 1;
    const INSPECT: u32 = 1 << 9;
    let (client, server) = endpoints::<TemporaryFilesystem>().map_err(|_| 50)?;
    let connected = directory.connect(TEMPORARY.into(), None, server.into_handle(), FOREVER);
    expect(matches!(connected, Ok(Ok(_))), 50)?;
    let temporary = TemporaryFilesystemClient::new(client);
    let mut raw = 0;
    // SAFETY: the out pointer is valid for the call.
    let made = unsafe { sys::budget_create(parent.ok_or(51)?, 1 << 20, 4, &mut raw) };
    expect(made == mikros_rt::OK, 51)?;
    // SAFETY: the kernel just created the handle for us.
    let budget = unsafe { OwnedHandle::from_raw(raw) };
    let mkdir = flags(&[F::CREATE, F::DIRECTORY]);
    let (charged, _, _) = open(root, "charged", mkdir, RW).map_err(|_| 52)?;
    let (sub, _, _) = open(&charged, "sub", mkdir, RW).map_err(|_| 52)?;
    let (plain, _, _) = open(root, "plain", flags(&[F::CREATE]), RW).map_err(|_| 52)?;
    let set = |path: &str, rights: u32| {
        let budget = copy(budget.raw(), rights).ok_or(None)?;
        match temporary.set_budget(path.into(), budget, FOREVER) {
            Ok(Ok(_)) => Ok(()),
            Ok(Err(e)) => Err(Some(e)),
            Err(_) => Err(None),
        }
    };
    let no_write = DUPLICATE | TRANSFER | INSPECT;
    expect(
        set("tmpfs-probe/charged", no_write) == Err(Some(Error::AccessDenied)),
        53,
    )?;
    expect(
        set("tmpfs-probe/missing", SAME_RIGHTS) == Err(Some(Error::NotFound)),
        54,
    )?;
    expect(
        set("tmpfs-probe/plain", SAME_RIGHTS) == Err(Some(Error::NotDirectory)),
        55,
    )?;
    expect(set("tmpfs-probe/charged", SAME_RIGHTS) == Ok(()), 56)?;
    let (file, _, _) = open(&sub, "f", flags(&[F::CREATE]), RW).map_err(|_| 57)?;
    let two = alloc::vec![7u8; 2 * PAGE as usize];
    expect(ok(file.write(0, two, FOREVER)).is_some(), 57)?;
    expect(pages(budget.raw()) == Some(2), 58)?;
    expect(ok(plain.write(0, b"x".to_vec(), FOREVER)).is_some(), 59)?;
    expect(pages(budget.raw()) == Some(2), 59)?;
    let three = alloc::vec![7u8; 3 * PAGE as usize];
    expect(ok(file.write(2 * PAGE, three, FOREVER)).is_none(), 60)?;
    drop(file);
    expect(ok(sub.unlink("f".into(), false, FOREVER)).is_some(), 61)?;
    // The server lets go of the file once it sees the connection close.
    for _ in 0..100 {
        if pages(budget.raw()) == Some(0) {
            return Ok(());
        }
        let mut now = 0;
        // SAFETY: the out pointer is valid for the call.
        unsafe { sys::clock_get_monotonic(&mut now) };
        // SAFETY: the call takes no pointers.
        unsafe { sys::thread_sleep(now + 10_000_000) };
    }
    Err(62)
}
