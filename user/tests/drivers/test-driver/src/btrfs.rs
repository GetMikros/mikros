//! As the driver of a partition of its own on the test machine's disk:
//! whether the btrfs server serves the root partition of the same disk
//! through its `filesystem` service as it should.

use crate::fat::{expect, ok, root};
use mikros_rt::exec::Reactor;
use mikros_rt::interfaces::mikros::fs::NodeType;
use mikros_rt::interfaces::mikros::service::DirectoryAsyncClient;

/// Whether the file system behaves: it is a writable btrfs whose root is
/// a directory, which can be synced.
pub async fn works(directory: &DirectoryAsyncClient, reactor: &Reactor) -> bool {
    check(directory, reactor).await.is_some()
}

async fn check(directory: &DirectoryAsyncClient, reactor: &Reactor) -> Option<()> {
    let root = root(directory, reactor, "btrfs").await?;
    let info = ok(root.query_filesystem().await)?.info;
    expect(!info.read_only && info.free_bytes > 0 && info.block_size == 4096)?;
    let attributes = ok(root.get_attributes().await)?.attributes;
    expect(attributes.r#type == NodeType::Directory)?;
    ok(root.sync().await)?;
    Some(())
}
