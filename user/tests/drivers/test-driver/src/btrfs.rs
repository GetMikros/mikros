//! As the driver of a partition of its own on the test machine's disk:
//! whether the btrfs server serves the root partition of the same disk
//! through its `filesystem` service as it should.

use crate::fat::{expect, ok, open, root};
use alloc::string::String;
use alloc::vec::Vec;
use mikros_rt::exec::Reactor;
use mikros_rt::interfaces::mikros::fs::{Error, NodeAsyncClient, NodeType, OpenFlags};
use mikros_rt::interfaces::mikros::service::DirectoryAsyncClient;

/// Whether the file system behaves: it is a writable btrfs whose root,
/// which can be synced, holds the top-level directories of the system's
/// layout, each listed with its attributes when asked; `@system/current`
/// is a link to a system version, which is followed to a directory of
/// a system image; and `..` and missing names are refused.
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
