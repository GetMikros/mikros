//! As the driver of a partition of its own on the test machine's disk:
//! whether the btrfs server serves the root partition of the same disk
//! through its `filesystem` service as it should.

use crate::fat::{expect, failed, locks, ok, open, root};
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use mikros_rt::exec::Reactor;
use mikros_rt::interfaces::__midl::{ClientEnd, endpoints};
use mikros_rt::interfaces::mikros::fs::{
    Error, Node, NodeAsyncClient, NodeType, OpenFlags, Rights, WatchKind, WatchMask, Watcher,
    WatcherRequest, WatcherServer,
};
use mikros_rt::interfaces::mikros::service::DirectoryAsyncClient;

const FOREVER: i64 = i64::MAX;
/// More entries than one `ReadDirectory` returns.
const MANY: usize = 70;

/// Whether the file system behaves: it is a writable btrfs whose root,
/// which can be synced, holds the top-level directories of the system's
/// layout and `usr`, the current system version, which cannot be written,
/// created, removed or renamed, each listed with its attributes when
/// asked; `@system/current` is a link to a system version, which is
/// followed to a directory of a system image; `..` and missing names are
/// refused; a file made in a new directory is written, appended to and
/// read back, with its attributes; a watch of the directory hears of a
/// file made, written, renamed and removed; a link within the directory
/// is followed, one that leads out of it comes back as a redirect, and
/// both read as their targets; a hard link names the same node and can be
/// renamed; a new file shares the data of a range of another; byte-range
/// locks of two connections behave; a directory of more entries than one
/// reply holds lists each once; names taken and paths through files are
/// refused; and what was made is removed again.
pub async fn works(directory: &DirectoryAsyncClient, reactor: &Reactor) -> bool {
    check(directory, reactor).await.is_some()
}

async fn check(directory: &DirectoryAsyncClient, reactor: &Reactor) -> Option<()> {
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

    watches(&dir, reactor).await?;
    links(&dir, reactor).await?;
    locks(&file, reactor).await?;
    clones(&dir, &file, reactor).await?;

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

/// A new connection to `file` that only reads, as the source of a
/// `CloneRange`.
async fn reader(file: &NodeAsyncClient) -> Option<ClientEnd<Node>> {
    let (source, server) = endpoints::<Node>().ok()?;
    ok(file.clone(server, Rights::READ).await)?;
    Some(source)
}
