//! As the driver of a partition of its own on the test machine's disk:
//! whether the btrfs server serves the root partition of the same disk
//! through its `filesystem` service as it should.

use crate::fat::{PERM_READ, PERM_WRITE, bytes, expect, failed, locks, map, ok, open, root};
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use mikros_rt::exec::Reactor;
use mikros_rt::interfaces::__midl::{ClientEnd, endpoints};
use mikros_rt::interfaces::mikros::fs::btrfs::{SubvolumeInfo, Subvolumes, SubvolumesAsyncClient};
use mikros_rt::interfaces::mikros::fs::{
    Error, Filesystem, FilesystemAsyncClient, MemoryFlags, Node, NodeAsyncClient, NodeType,
    OpenFlags, Rights, WatchKind, WatchMask, Watcher, WatcherRequest, WatcherServer,
};
use mikros_rt::interfaces::mikros::service::DirectoryAsyncClient;
use mikros_rt::sys;

const FOREVER: i64 = i64::MAX;
/// The protocols of the file system's services, and how long the second
/// may take to appear after the first.
const FILESYSTEM: &str = "device.filesystem";
const SUBVOLUMES: &str = "device.subvolumes";
const PUBLISH_LIMIT: i64 = 60_000_000_000;
/// More entries than one `ReadDirectory` returns.
const MANY: usize = 70;

/// Whether the file system behaves: it is a writable btrfs whose root,
/// which can be synced, holds the top-level directories of the system's
/// layout and `usr`, the current system version, which cannot be written,
/// created, removed or renamed, each listed with its attributes when
/// asked; `@system/current` is a link to a system version, which is
/// followed to a directory of a system image; `..` and missing names are
/// refused; a file made in a new directory is written, appended to and
/// read back, with its attributes; memory objects of the file share its
/// pages, or copy them; a watch of the directory hears of a file made,
/// written, renamed and removed; a link within the directory is followed,
/// one that leads out of it comes back as a redirect, and both read as
/// their targets; a hard link names the same node and can be renamed; a
/// new file shares the data of a range of another; names that differ in
/// case are found case-insensitively, and taken for CREATE; a connection
/// opened case-insensitively stays so; byte-range locks of two
/// connections behave; its subvolumes are listed, made, snapshotted, made
/// writable and removed; a directory of more entries than one reply holds
/// lists each once; names taken and paths through files are refused; and
/// what was made is removed again.
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
    let root = root(directory, reactor, "btrfs").await?;
    let info = ok(root.query_filesystem().await)?.info;
    expect(!info.read_only && info.free_bytes > 0 && info.block_size == 4096)?;
    let attributes = ok(root.get_attributes().await)?.attributes;
    expect(attributes.r#type == NodeType::Directory)?;
    let top = names(&root).await?;
    for dir in [
        "usr",
        "@system",
        "etc",
        "var",
        "Users",
        "Apps",
        "Libraries",
        "tmp",
        "Volumes",
    ] {
        expect(
            top.iter()
                .any(|(n, t)| n == dir && *t == NodeType::Directory),
        )?;
    }
    let listed = ok(root.read_directory(0, true).await)?.entries;
    expect(!listed.is_empty())?;
    for entry in listed {
        expect(entry.attributes.is_some_and(|a| a.r#type == entry.r#type))?;
    }

    system(&root, reactor).await?;
    usr(&root, reactor).await?;

    let (etc, _) = open(&root, "etc", OpenFlags::DIRECTORY, reactor)
        .await
        .ok()?;
    let up = open(&etc, "../var", OpenFlags(0), reactor).await;
    expect(matches!(up, Err(Some(Error::AccessDenied))))?;
    let missing = open(&etc, "missing", OpenFlags(0), reactor).await;
    expect(matches!(missing, Err(Some(Error::NotFound))))?;
    let deeper = open(&root, "missing/x", OpenFlags(0), reactor).await;
    expect(matches!(deeper, Err(Some(Error::NotFound))))?;

    let create_dir = OpenFlags(OpenFlags::CREATE.0 | OpenFlags::DIRECTORY.0);
    let (dir, kind) = open(&root, "btrfs-test", create_dir, reactor).await.ok()?;
    expect(kind == NodeType::Directory)?;
    let create = OpenFlags(OpenFlags::CREATE.0 | OpenFlags::EXCLUSIVE.0);
    let (file, _) = open(&dir, "file", create, reactor).await.ok()?;
    expect(ok(file.write(0, b"hello btrfs".to_vec()).await)?.written == 11)?;
    expect(ok(file.append(b"!".to_vec()).await)?.offset == 11)?;
    let attributes = ok(file.get_attributes().await)?.attributes;
    expect(
        attributes.size == 12 && attributes.mode == 0o644 && attributes.r#type == NodeType::File,
    )?;
    expect(ok(file.read(0, 100).await)?.data == b"hello btrfs!")?;
    expect(ok(file.read(6, 5).await)?.data == b"btrfs")?;
    ok(file.sync().await)?;
    memory(&file, process).await?;

    watches(&dir, reactor).await?;
    links(&dir, reactor).await?;
    locks(&file, reactor).await?;
    clones(&dir, &file, reactor).await?;
    case_insensitive(&root, &dir, reactor).await?;
    subvolumes(directory, reactor).await?;

    let (many, _) = open(&dir, "many", create_dir, reactor).await.ok()?;
    for i in 0..MANY {
        open(&many, &format!("f{i:02}"), create, reactor)
            .await
            .ok()?;
    }
    let listed = names(&many).await?;
    expect(listed.len() == MANY)?;
    for i in 0..MANY {
        expect(listed.iter().any(|(n, _)| *n == format!("f{i:02}")))?;
    }
    for i in 0..MANY {
        ok(many.unlink(format!("f{i:02}"), false).await)?;
    }

    let again = open(&dir, "file", create, reactor).await;
    expect(matches!(again, Err(Some(Error::Exists))))?;
    let into = open(&dir, "file/x", OpenFlags(0), reactor).await;
    expect(matches!(into, Err(Some(Error::NotDirectory))))?;
    expect(failed(dir.unlink("many".into(), false).await) == Some(Error::IsDirectory))?;

    ok(dir.unlink("many".into(), true).await)?;
    ok(dir.unlink("file".into(), false).await)?;
    ok(root.unlink("btrfs-test".into(), true).await)?;
    let gone = open(&root, "btrfs-test", OpenFlags(0), reactor).await;
    expect(matches!(gone, Err(Some(Error::NotFound))))?;
    ok(root.sync().await)?;
    Some(())
}

/// Whether `@system/current` in `root` is a link to a system version,
/// which is followed to a directory of a system image.
async fn system(root: &NodeAsyncClient, reactor: &Reactor) -> Option<()> {
    let (current, kind) = open(root, "@system/current", OpenFlags::NO_FOLLOW, reactor)
        .await
        .ok()?;
    expect(kind == NodeType::Symlink)?;
    let version = ok(current.read_link().await)?.target;
    expect(!version.is_empty() && !version.contains('/'))?;
    let (system, kind) = open(root, "@system/current", OpenFlags(0), reactor)
        .await
        .ok()?;
    expect(kind == NodeType::Directory)?;
    let listed = names(&system).await?;
    for dir in ["bin", "lib", "include", "share"] {
        expect(
            listed
                .iter()
                .any(|(n, t)| n == dir && *t == NodeType::Directory),
        )?;
    }
    let (_, kind) = open(root, "@system/current/lib/drivers", OpenFlags(0), reactor)
        .await
        .ok()?;
    expect(kind == NodeType::Directory)?;
    Some(())
}

/// Whether the root's `usr` behaves: it is the current system version,
/// with the directories of a system image, which cannot be written, and
/// it cannot be created, removed or renamed.
async fn usr(root: &NodeAsyncClient, reactor: &Reactor) -> Option<()> {
    let (usr, kind) = open(root, "usr", OpenFlags(0), reactor).await.ok()?;
    expect(kind == NodeType::Directory)?;
    let listed = names(&usr).await?;
    for dir in ["bin", "lib", "include", "share"] {
        expect(
            listed
                .iter()
                .any(|(n, t)| n == dir && *t == NodeType::Directory),
        )?;
    }
    let (_, kind) = open(root, "usr/lib/drivers", OpenFlags(0), reactor)
        .await
        .ok()?;
    expect(kind == NodeType::Directory)?;
    // The system image holds the boot image's system.
    let (manager, kind) = open(root, "usr/bin/process-manager", OpenFlags(0), reactor)
        .await
        .ok()?;
    expect(kind == NodeType::File)?;
    expect(ok(manager.get_attributes().await)?.attributes.size > 0)?;
    let create = OpenFlags(OpenFlags::CREATE.0 | OpenFlags::EXCLUSIVE.0);
    let written = open(root, "usr/new", create, reactor).await;
    expect(matches!(written, Err(Some(Error::ReadOnly))))?;
    let made = open(root, "usr", create, reactor).await;
    expect(matches!(made, Err(Some(Error::AccessDenied))))?;
    expect(failed(root.unlink("usr".into(), true).await) == Some(Error::AccessDenied))?;
    let token = ok(root.get_token().await)?.token;
    let renamed = root.rename("usr".into(), token, "old".into()).await;
    expect(failed(renamed) == Some(Error::AccessDenied))?;
    Some(())
}

/// The names and types of the entries of `dir`, over as many replies as
/// it takes.
async fn names(dir: &NodeAsyncClient) -> Option<Vec<(String, NodeType)>> {
    let mut names = Vec::new();
    let mut cookie = 0;
    loop {
        let read = ok(dir.read_directory(cookie, false).await)?;
        if read.entries.is_empty() {
            return Some(names);
        }
        for entry in read.entries {
            expect(entry.name != "." && entry.name != ".." && entry.attributes.is_none())?;
            names.push((entry.name, entry.r#type));
        }
        cookie = read.next;
    }
}

/// Whether a watch of `dir` hears of a file made, written, renamed and
/// removed, a change to a message, in order.
async fn watches(dir: &NodeAsyncClient, reactor: &Reactor) -> Option<()> {
    let (client, server) = endpoints::<Watcher>().ok()?;
    let mask =
        WatchMask::CREATED.0 | WatchMask::REMOVED.0 | WatchMask::MODIFIED.0 | WatchMask::MOVED.0;
    ok(dir.watch(WatchMask(mask), 7, client).await)?;
    let create = OpenFlags(OpenFlags::CREATE.0 | OpenFlags::EXCLUSIVE.0);
    let (x, _) = open(dir, "x", create, reactor).await.ok()?;
    ok(x.write(0, b"1".to_vec()).await)?;
    let token = ok(dir.get_token().await)?.token;
    ok(dir.rename("x".into(), token, "y".into()).await)?;
    ok(dir.unlink("y".into(), false).await)?;
    let server = WatcherServer::new(server);
    let mut changes = Vec::new();
    for _ in 0..4 {
        let Ok(WatcherRequest::OnEvents(message)) = server.next(FOREVER) else {
            return None;
        };
        expect(message.cookie == 7)?;
        let events: Vec<(WatchKind, u32, String)> = message
            .events
            .into_iter()
            .map(|e| (e.kind, e.move_cookie, e.name))
            .collect();
        changes.push(events);
    }
    let moved = changes[2].first().map_or(0, |e| e.1);
    let expected = [
        alloc::vec![(WatchKind::Created, 0, String::from("x"))],
        alloc::vec![(WatchKind::Modified, 0, String::from("x"))],
        alloc::vec![
            (WatchKind::MovedFrom, moved, String::from("x")),
            (WatchKind::MovedTo, moved, String::from("y")),
        ],
        alloc::vec![(WatchKind::Removed, 0, String::from("y"))],
    ];
    expect(moved != 0 && changes == expected)?;
    ok(dir.unwatch(7).await)?;
    expect(failed(dir.unwatch(7).await) == Some(Error::NotFound))?;
    Some(())
}

/// Whether symbolic and hard links in `dir`, which holds `file`, behave,
/// and renaming one.
async fn links(dir: &NodeAsyncClient, reactor: &Reactor) -> Option<()> {
    ok(dir.create_symlink("near".into(), "file".into()).await)?;
    ok(dir.create_symlink("far".into(), "../etc".into()).await)?;
    let (near, kind) = open(dir, "near", OpenFlags(0), reactor).await.ok()?;
    expect(kind == NodeType::File)?;
    expect(ok(near.read(0, 100).await)?.data == b"hello btrfs!")?;
    let (link, kind) = open(dir, "near", OpenFlags::NO_FOLLOW, reactor)
        .await
        .ok()?;
    expect(kind == NodeType::Symlink && ok(link.read_link().await)?.target == "file")?;
    // A link that leads out of the directory comes back for the client
    // to follow, with the rest of the path.
    let (endpoint, server) = endpoints::<mikros_rt::interfaces::mikros::fs::Node>().ok()?;
    drop(endpoint);
    let rights = mikros_rt::interfaces::mikros::fs::Rights::READ;
    let far = ok(dir
        .open("far/passwd".into(), OpenFlags(0), 0, rights, server)
        .await)?;
    expect(far.r#type == NodeType::Symlink && far.redirect.as_deref() == Some("../etc/passwd"))?;

    let token = ok(dir.get_token().await)?.token;
    ok(dir.link("file".into(), token, "hard".into()).await)?;
    let (hard, _) = open(dir, "hard", OpenFlags(0), reactor).await.ok()?;
    let a = ok(hard.get_attributes().await)?.attributes;
    expect(a.links == 2 && ok(hard.read(0, 100).await)?.data == b"hello btrfs!")?;
    let token = ok(dir.get_token().await)?.token;
    ok(dir.rename("hard".into(), token, "moved".into()).await)?;
    let old = open(dir, "hard", OpenFlags(0), reactor).await;
    expect(matches!(old, Err(Some(Error::NotFound))))?;
    let (moved, _) = open(dir, "moved", OpenFlags(0), reactor).await.ok()?;
    expect(ok(moved.read(0, 100).await)?.data == b"hello btrfs!")?;
    for name in ["near", "far", "moved"] {
        ok(dir.unlink(name.into(), false).await)?;
    }
    Some(())
}

/// Whether `CloneRange` makes a new file in `dir` share the data of
/// `file`, which holds 12 bytes, through a connection to it that only
/// reads, with a length of 12 and with 0, which reaches to the end; and
/// refuses an offset out of line with the blocks and a source that is no
/// connection of the server's.
async fn clones(dir: &NodeAsyncClient, file: &NodeAsyncClient, reactor: &Reactor) -> Option<()> {
    let create = OpenFlags(OpenFlags::CREATE.0 | OpenFlags::EXCLUSIVE.0);
    let (copy, _) = open(dir, "copy", create, reactor).await.ok()?;
    ok(copy.clone_range(reader(file).await?, 0, 0, 12).await)?;
    expect(ok(copy.read(0, 100).await)?.data == b"hello btrfs!")?;
    let (whole, _) = open(dir, "whole", create, reactor).await.ok()?;
    ok(whole.clone_range(reader(file).await?, 0, 0, 0).await)?;
    expect(ok(whole.read(0, 100).await)?.data == b"hello btrfs!")?;
    let unaligned = whole.clone_range(reader(file).await?, 0, 100, 12).await;
    expect(failed(unaligned) == Some(Error::Invalid))?;
    ok(dir.unlink("whole".into(), false).await)?;
    let (stranger, _server) = endpoints::<Node>().ok()?;
    let refused = copy.clone_range(stranger, 0, 0, 12).await;
    expect(failed(refused) == Some(Error::CrossDevice))?;
    ok(dir.unlink("copy".into(), false).await)?;
    Some(())
}

/// Whether memory objects of `file`, which holds "hello btrfs!", behave:
/// a shared one shows what is written through the connection, and a
/// writable one writes to the file what is written through a mapping,
/// which a private one does not see; a sync writes it back; and
/// `EXECUTE` needs the connection's right `EXECUTE`. `file` holds
/// "hello btrfs!" again after.
async fn memory(file: &NodeAsyncClient, process: sys::Handle) -> Option<()> {
    let shared = ok(file.get_memory_object(MemoryFlags(0)).await)?.memory;
    let view = map(shared.raw(), process, PERM_READ)?;
    expect(bytes(view, 12) == b"hello btrfs!")?;
    ok(file.write(0, b"HELLO".to_vec()).await)?;
    expect(bytes(view, 12) == b"HELLO btrfs!")?;
    let private = ok(file.get_memory_object(MemoryFlags::PRIVATE).await)?.memory;
    let copy = map(private.raw(), process, PERM_READ)?;
    let writable = ok(file.get_memory_object(MemoryFlags::WRITE).await)?.memory;
    let window = map(writable.raw(), process, PERM_READ | PERM_WRITE)?;
    // SAFETY: the object is mapped writable at `window` for a page.
    unsafe { core::ptr::write_volatile(window as *mut u8, b'J') };
    expect(ok(file.read(0, 12).await)?.data == b"JELLO btrfs!")?;
    expect(bytes(view, 12) == b"JELLO btrfs!" && bytes(copy, 12) == b"HELLO btrfs!")?;
    ok(file.sync().await)?;
    let execute = file.get_memory_object(MemoryFlags::EXECUTE).await;
    expect(failed(execute) == Some(Error::AccessDenied))?;
    for at in [view, copy, window] {
        // SAFETY: the call takes no pointers; a page is mapped at `at`.
        unsafe { sys::vm_unmap(process, at, 4096) };
    }
    ok(file.write(0, b"hello".to_vec()).await)?;
    Some(())
}

/// A new connection to `file` that only reads, as the source of a
/// `CloneRange`.
async fn reader(file: &NodeAsyncClient) -> Option<ClientEnd<Node>> {
    let (source, server) = endpoints::<Node>().ok()?;
    ok(file.clone(server, Rights::READ).await)?;
    Some(source)
}

/// Whether `CASE_INSENSITIVE` finds an entry of `dir`, a directory of
/// `root` named `btrfs-test`, whose name differs in case, also on the way
/// to another and for the root's `usr`, and of two such entries the first
/// that `ReadDirectory` lists; whether `CREATE` of such a name fails with
/// `EXISTS`; and whether names that differ more, or differ in case
/// without the flag, are not found.
async fn case_insensitive(
    root: &NodeAsyncClient,
    dir: &NodeAsyncClient,
    reactor: &Reactor,
) -> Option<()> {
    let folded = OpenFlags::CASE_INSENSITIVE;
    let create = OpenFlags(OpenFlags::CREATE.0 | OpenFlags::EXCLUSIVE.0);
    open(dir, "Straße", create, reactor).await.ok()?;
    let (found, kind) = open(dir, "STRAẞE", folded, reactor).await.ok()?;
    expect(kind == NodeType::File && ok(found.get_attributes().await)?.attributes.size == 0)?;
    let exact = open(dir, "STRAẞE", OpenFlags(0), reactor).await;
    expect(matches!(exact, Err(Some(Error::NotFound))))?;
    // A sharp s has no simple folding to two letters.
    let other = open(dir, "STRASSE", folded, reactor).await;
    expect(matches!(other, Err(Some(Error::NotFound))))?;
    let taken = open(
        dir,
        "straße",
        OpenFlags(OpenFlags::CREATE.0 | folded.0),
        reactor,
    )
    .await;
    expect(matches!(taken, Err(Some(Error::Exists))))?;
    let (_, kind) = open(root, "BTRFS-TEST/STRAßE", folded, reactor)
        .await
        .ok()?;
    expect(kind == NodeType::File)?;
    let (_, kind) = open(root, "USR/LIB", folded, reactor).await.ok()?;
    expect(kind == NodeType::Directory)?;

    for name in ["dup", "DUP"] {
        open(dir, name, create, reactor).await.ok()?;
    }
    let listed = names(dir).await?;
    let (first, _) = listed.iter().find(|(n, _)| n == "dup" || n == "DUP")?;
    let (want, _) = open(dir, first, OpenFlags(0), reactor).await.ok()?;
    let (got, _) = open(dir, "Dup", folded, reactor).await.ok()?;
    let inode =
        async |node: &NodeAsyncClient| Some(ok(node.get_attributes().await)?.attributes.inode);
    expect(inode(&got).await? == inode(&want).await?)?;
    for name in ["dup", "DUP"] {
        ok(dir.unlink(name.into(), false).await)?;
    }

    // A connection opened with the flag, and its clones, stay
    // case-insensitive for the names of its other requests.
    let directory = OpenFlags(OpenFlags::DIRECTORY.0 | folded.0);
    let (ci, _) = open(root, "BTRFS-TEST", directory, reactor).await.ok()?;
    let symlink = ci.create_symlink("STRAẞE".into(), "x".into()).await;
    expect(failed(symlink) == Some(Error::Exists))?;
    let token = ok(ci.get_token().await)?.token;
    let linked = ci.link("STRAẞE".into(), token, "LINKED".into()).await;
    ok(linked)?;
    let token = ok(ci.get_token().await)?.token;
    let taken = ci.link("strasse".into(), token, "Linked".into()).await;
    expect(failed(taken) == Some(Error::NotFound))?;
    let token = ok(ci.get_token().await)?.token;
    let taken = ci.link("straße".into(), token, "linked".into()).await;
    expect(failed(taken) == Some(Error::Exists))?;
    ok(ci.unlink("linked".into(), false).await)?;
    // Renaming to another case of its own name changes the case; onto an
    // entry that differs in case, it replaces that entry, under the name
    // given.
    open(dir, "one", create, reactor).await.ok()?;
    let token = ok(ci.get_token().await)?.token;
    ok(ci.rename("ONE".into(), token, "One".into()).await)?;
    let (two, _) = open(dir, "two", create, reactor).await.ok()?;
    let moved = inode(&two).await?;
    let token = ok(ci.get_token().await)?.token;
    ok(ci.rename("TWO".into(), token, "one".into()).await)?;
    let listed = names(dir).await?;
    expect(
        listed.iter().any(|(n, _)| n == "one")
            && !listed
                .iter()
                .any(|(n, _)| n == "One" || n == "two" || n == "ONE"),
    )?;
    let (one, _) = open(dir, "one", OpenFlags(0), reactor).await.ok()?;
    expect(inode(&one).await? == moved)?;
    let (clone, server) = endpoints::<Node>().ok()?;
    ok(ci
        .clone(server, Rights(Rights::READ.0 | Rights::WRITE.0))
        .await)?;
    let clone = NodeAsyncClient::new(clone, reactor.clone());
    for name in ["ONE", "straße"] {
        ok(clone.unlink(name.into(), false).await)?;
    }
    let gone = open(dir, "Straße", OpenFlags(0), reactor).await;
    expect(matches!(gone, Err(Some(Error::NotFound))))?;
    Some(())
}

/// A connection to the `subvolumes` service of the btrfs file system,
/// whose instance is named as the file system's `filesystem` service is,
/// once it appears.
async fn connect_subvolumes(
    directory: &DirectoryAsyncClient,
    reactor: &Reactor,
) -> Option<SubvolumesAsyncClient> {
    let deadline = crate::now() + PUBLISH_LIMIT;
    let Ok(Ok(listed)) = directory.list(FILESYSTEM.into()).await else {
        return None;
    };
    for instance in listed.instances {
        let (client, server) = endpoints::<Filesystem>().ok()?;
        let connected = directory
            .connect(
                FILESYSTEM.into(),
                Some(instance.clone()),
                server.into_handle(),
            )
            .await;
        if !matches!(connected, Ok(Ok(_))) {
            continue;
        }
        let fs = FilesystemAsyncClient::new(ClientEnd::new(client.into_handle()), reactor.clone());
        if !ok(fs.query_filesystem().await).is_some_and(|q| q.info.r#type == "btrfs") {
            continue;
        }
        loop {
            let (client, server) = endpoints::<Subvolumes>().ok()?;
            let connected = directory
                .connect(
                    SUBVOLUMES.into(),
                    Some(instance.clone()),
                    server.into_handle(),
                )
                .await;
            if matches!(connected, Ok(Ok(_))) {
                return Some(SubvolumesAsyncClient::new(
                    ClientEnd::new(client.into_handle()),
                    reactor.clone(),
                ));
            }
            if crate::now() > deadline {
                return None;
            }
            let _ = reactor.sleep_until(crate::now() + 20_000_000).await;
        }
    }
    None
}

/// Every subvolume, over as many replies as it takes.
async fn list_all(subvolumes: &SubvolumesAsyncClient) -> Option<Vec<SubvolumeInfo>> {
    let mut all = Vec::new();
    let mut cursor = 0;
    loop {
        let listed = ok(subvolumes.list(cursor).await)?;
        if listed.subvolumes.is_empty() {
            return Some(all);
        }
        all.extend(listed.subvolumes);
        cursor = listed.next;
    }
}

/// Whether the `subvolumes` service of the file system behaves: it lists
/// the subvolumes of the system's layout with their paths from the top,
/// and the current system version read-only; a subvolume made is listed,
/// and a read-only snapshot of it, which can be made writable; both can
/// be removed; and bad paths are refused.
async fn subvolumes(directory: &DirectoryAsyncClient, reactor: &Reactor) -> Option<()> {
    let subvolumes = connect_subvolumes(directory, reactor).await?;
    let has = |all: &[SubvolumeInfo], path: &str, read_only: bool| {
        all.iter()
            .any(|s| s.path == path && s.read_only == read_only)
    };
    let listed = list_all(&subvolumes).await?;
    for path in ["@system", "etc", "var", "Libraries"] {
        expect(has(&listed, path, false))?;
    }
    expect(
        listed
            .iter()
            .any(|s| s.path.starts_with("@system/") && s.read_only),
    )?;

    ok(subvolumes.create("btrfs-subvol".into()).await)?;
    let snapshot = subvolumes.snapshot("btrfs-subvol".into(), "btrfs-snap".into(), true);
    ok(snapshot.await)?;
    let listed = list_all(&subvolumes).await?;
    expect(has(&listed, "btrfs-subvol", false) && has(&listed, "btrfs-snap", true))?;
    ok(subvolumes.set_read_only("btrfs-snap".into(), false).await)?;
    expect(has(&list_all(&subvolumes).await?, "btrfs-snap", false))?;
    expect(failed(subvolumes.create("../x".into()).await) == Some(Error::Invalid))?;
    expect(failed(subvolumes.delete("missing".into()).await) == Some(Error::NotFound))?;
    for path in ["btrfs-snap", "btrfs-subvol"] {
        ok(subvolumes.delete(path.into()).await)?;
    }
    let listed = list_all(&subvolumes).await?;
    expect(!listed.iter().any(|s| s.path.starts_with("btrfs-")))?;
    Some(())
}
