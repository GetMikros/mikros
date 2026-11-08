//! As the driver of a partition of its own on the test machine's disk:
//! whether the btrfs server serves the root partition of the same disk
//! through its `filesystem` service as it should.

use crate::fat::{expect, failed, ok, open, root};
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use mikros_rt::exec::Reactor;
use mikros_rt::interfaces::__midl::endpoints;
use mikros_rt::interfaces::mikros::fs::{Error, NodeAsyncClient, NodeType, OpenFlags};
use mikros_rt::interfaces::mikros::service::DirectoryAsyncClient;

/// More entries than one `ReadDirectory` returns.
const MANY: usize = 70;

/// Whether the file system behaves: it is a writable btrfs whose root,
/// which can be synced, holds the top-level directories of the system's
/// layout, each listed with its attributes when asked; `@system/current`
/// is a link to a system version, which is followed to a directory of
/// a system image; `..` and missing names are refused; a file made in a
/// new directory is written, appended to and read back, with its
/// attributes; a link within the directory is followed, one that leads
/// out of it comes back as a redirect, and both read as their targets; a
/// hard link names the same node and can be renamed; a directory of more
/// entries than one reply holds lists each once; names taken and paths
/// through files are refused; and what was made is removed again.
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

    links(&dir, reactor).await?;

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
