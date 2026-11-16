//! A program that the service manager starts as a boot service after the
//! temporary file system, to drive it as a client. It connects to
//! `mikros.fs.Filesystem` and `mikros.fs.TemporaryFilesystem` through its
//! directory and checks that a budget set on a directory is charged for
//! the files below it, for which it gets the root budget, and that access
//! control lists are kept, inherited and checked for the subjects that
//! `Restrict` gives connections. The system test services-tmpfs checks
//! the rest of the file system as a client of the running one. It exits with 0 when the check passed, or with the number
//! of the step that failed.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::string::String;
use mikros_rt::exec::Executor;
use mikros_rt::interfaces::__midl::{ClientEnd, OwnedHandle, endpoints};
use mikros_rt::interfaces::mikros::fs::{
    AclEntry, AclTag, Error, Filesystem, FilesystemClient, Node, NodeClient, NodeType, OpenFlags,
    Rights, TemporaryFilesystem, TemporaryFilesystemClient,
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
    match budgets(&root, &directory, budget)
        .and_then(|()| acls(&root))
        .and_then(|()| exchanges(&root))
    {
        Ok(()) => 0,
        Err(failed) => failed,
    }
}

const R: Rights = Rights::READ;
const W: Rights = Rights::WRITE;
const NONE: Rights = Rights(0);

fn entry(tag: AclTag, id: u32, rights: Rights) -> AclEntry {
    AclEntry { tag, id, rights }
}

/// A connection to the node of `node` for the subject `user` and `app`.
fn restrict(node: &NodeClient, user: u32, app: u32) -> Result<NodeClient, Option<Error>> {
    let (client, server) = endpoints::<Node>().map_err(|_| None)?;
    match node.restrict(user, app, 1, 1, 0, server, FOREVER) {
        Ok(Ok(_)) => Ok(NodeClient::new(client)),
        Ok(Err(e)) => Err(Some(e)),
        Err(_) => Err(None),
    }
}

fn get_acl(node: &NodeClient, default: bool) -> Result<alloc::vec::Vec<AclEntry>, Option<Error>> {
    match node.get_acl(default, FOREVER) {
        Ok(Ok(r)) => Ok(r.entries),
        Ok(Err(e)) => Err(Some(e)),
        Err(_) => Err(None),
    }
}

fn set_acl(node: &NodeClient, default: bool, entries: &[AclEntry]) -> Result<(), Option<Error>> {
    let entries = entries
        .iter()
        .map(|e| entry(e.tag, e.id, e.rights))
        .collect();
    match node.set_acl(default, entries, FOREVER) {
        Ok(Ok(_)) => Ok(()),
        Ok(Err(e)) => Err(Some(e)),
        Err(_) => Err(None),
    }
}

/// Whether reading and writing the file `name` in `dir` succeed.
fn access(dir: &NodeClient, name: &str) -> Result<(bool, bool), i64> {
    // Opening needs no entry of the list, only reading and writing do.
    let (file, _, _) = open(dir, name, OpenFlags(0), RW).map_err(|_| 0)?;
    let read = ok(file.read(0, 1, FOREVER)).is_some();
    let wrote = ok(file.write(0, b"x".to_vec(), FOREVER)).is_some();
    Ok((read, wrote))
}

/// Lists: the entries of the mode without an extended one; a list that
/// `SetAcl` sets, with the mode from its mask, and that decides the
/// access of subjects other than user 0; default lists that new nodes
/// take; owners of the nodes that a subject makes; and `Restrict` only
/// once.
fn acls(root: &NodeClient) -> Result<(), i64> {
    use AclTag as T;
    use OpenFlags as F;
    let mkdir = flags(&[F::CREATE, F::DIRECTORY]);
    let (dir, _, _) = open(root, "acls", mkdir, RW).map_err(|_| 70)?;
    let (file, _, _) = open(&dir, "f", F::CREATE, RW).map_err(|_| 70)?;
    let of_mode = || {
        alloc::vec![
            entry(T::Owner, 0, Rights(R.0 | W.0)),
            entry(T::Group, 0, R),
            entry(T::Other, 0, NONE),
        ]
    };
    expect(get_acl(&file, false) == Ok(of_mode()), 71)?;
    expect(get_acl(&file, true) == Err(Some(Error::NotDirectory)), 72)?;
    expect(get_acl(&dir, true) == Ok(alloc::vec![]), 72)?;

    // A list of the owner, user 1001, app 7 and a mask of reading.
    let list = || {
        alloc::vec![
            entry(T::Owner, 0, Rights(R.0 | W.0)),
            entry(T::User, 1001, Rights(R.0 | W.0)),
            entry(T::Group, 0, R),
            entry(T::App, 7, Rights(R.0 | W.0)),
            entry(T::Mask, 0, R),
            entry(T::Other, 0, NONE),
        ]
    };
    let mut reversed = list();
    reversed.reverse();
    expect(set_acl(&file, false, &reversed) == Ok(()), 73)?;
    expect(get_acl(&file, false) == Ok(list()), 73)?;
    let mode = ok(file.get_attributes(FOREVER)).ok_or(74)?.attributes.mode;
    expect(mode & 0o777 == 0o640, 74)?;
    let mut no_mask = list();
    no_mask.retain(|e| e.tag != T::Mask);
    expect(
        set_acl(&file, false, &no_mask) == Err(Some(Error::Invalid)),
        75,
    )?;

    // Subjects meet the list; user 0 and connections without one do not.
    let user = |user, app| restrict(&dir, user, app).map_err(|_| 76);
    expect(access(&user(1001, 0)?, "f") == Ok((true, false)), 77)?;
    expect(access(&user(1002, 7)?, "f") == Ok((true, false)), 78)?;
    expect(access(&user(1002, 8)?, "f") == Ok((false, false)), 79)?;
    expect(access(&user(0, 8)?, "f") == Ok((true, true)), 80)?;
    expect(access(&dir, "f") == Ok((true, true)), 80)?;

    // Only once, and owners only without a subject or as user 0.
    let ada = user(1000, 0)?;
    expect(
        restrict(&ada, 1001, 0).err() == Some(Some(Error::BadState)),
        81,
    )?;
    let (mine, _, _) = open(&ada, "mine", F::CREATE, RW).map_err(|_| 82)?;
    let owner = ok(mine.get_attributes(FOREVER)).ok_or(82)?.attributes.owner;
    expect(owner == 1000, 82)?;
    expect(ok(mine.set_owner(1001, FOREVER)).is_none(), 83)?;
    // The owner sets the list of its own file; another user may not.
    expect(set_acl(&mine, false, &list()) == Ok(()), 84)?;
    let (theirs, _, _) = open(&user(1001, 0)?, "mine", OpenFlags(0), RW).map_err(|_| 85)?;
    expect(
        set_acl(&theirs, false, &of_mode()) == Err(Some(Error::AccessDenied)),
        85,
    )?;

    // New nodes take the default list, and new directories keep it.
    let (inherits, _, _) = open(&dir, "d", mkdir, RW).map_err(|_| 86)?;
    expect(set_acl(&inherits, true, &list()) == Ok(()), 86)?;
    expect(get_acl(&inherits, true) == Ok(list()), 86)?;
    let (new, _, _) = open(&inherits, "g", F::CREATE, RW).map_err(|_| 87)?;
    expect(get_acl(&new, false) == Ok(list()), 87)?;
    let (sub, _, _) = open(&inherits, "e", mkdir, RW).map_err(|_| 88)?;
    expect(get_acl(&sub, true) == Ok(list()), 88)?;
    expect(set_acl(&inherits, true, &[]) == Ok(()), 89)?;
    expect(get_acl(&inherits, true) == Ok(alloc::vec![]), 89)?;
    Ok(())
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

/// `Exchange` swaps two names in one step, also across directories, and
/// fails with `NOT_FOUND` for a name that is not there.
fn exchanges(root: &NodeClient) -> Result<(), i64> {
    use OpenFlags as F;
    let mkdir = flags(&[F::CREATE, F::DIRECTORY]);
    let (a, _, _) = open(root, "swap-a", mkdir, RW).map_err(|_| 90)?;
    let (b, _, _) = open(root, "swap-b", mkdir, RW).map_err(|_| 90)?;
    let (one, _, _) = open(&a, "one", F::CREATE, RW).map_err(|_| 90)?;
    let (two, _, _) = open(&b, "two", F::CREATE, RW).map_err(|_| 90)?;
    expect(ok(one.write(0, b"1".to_vec(), FOREVER)).is_some(), 90)?;
    expect(ok(two.write(0, b"2".to_vec(), FOREVER)).is_some(), 90)?;
    let token = ok(b.get_token(FOREVER)).ok_or(91)?.token;
    expect(
        ok(a.exchange("one".into(), token, "two".into(), FOREVER)).is_some(),
        91,
    )?;
    let read = |dir: &NodeClient, name: &str| {
        let (file, _, _) = open(dir, name, OpenFlags(0), Rights::READ).ok()?;
        ok(file.read(0, 8, FOREVER)).map(|r| r.data)
    };
    expect(read(&a, "one").as_deref() == Some(&b"2"[..]), 91)?;
    expect(read(&b, "two").as_deref() == Some(&b"1"[..]), 91)?;
    let token = ok(b.get_token(FOREVER)).ok_or(92)?.token;
    let missing = a.exchange("one".into(), token, "gone".into(), FOREVER);
    expect(matches!(missing, Ok(Err(Error::NotFound))), 92)?;
    Ok(())
}
