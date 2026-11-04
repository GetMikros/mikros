//! Reading the manifest files of a directory over the `Node` protocol.

use alloc::string::String;
use alloc::vec::Vec;
use mikros_rt::exec::Reactor;
use mikros_rt::interfaces::__midl::endpoints;
use mikros_rt::interfaces::mikros::fs::{
    Error, Node, NodeAsyncClient, NodeType, OpenFlags, Rights,
};

/// The most bytes one `Read` returns.
const READ_MAX: u32 = 32768;

/// Why a directory or file could not be read.
#[derive(Debug)]
pub enum Failure {
    /// The file system refused a request.
    Fs(Error),
    /// The connection failed.
    Channel,
    /// A file is larger than a YAML file may be.
    TooLarge,
}

/// The node at `path` under `dir`, opened with `READ`.
async fn open(
    dir: &NodeAsyncClient,
    reactor: &Reactor,
    path: &str,
    flags: OpenFlags,
) -> Result<NodeAsyncClient, Failure> {
    let (client, server) = endpoints::<Node>().map_err(|_| Failure::Channel)?;
    match dir.open(path.into(), flags, 0, Rights::READ, server).await {
        Ok(Ok(_)) => Ok(NodeAsyncClient::new(client, reactor.clone())),
        Ok(Err(e)) => Err(Failure::Fs(e)),
        Err(_) => Err(Failure::Channel),
    }
}

/// The names of the regular files in `path` under `root` that end in
/// `suffix`, in name order.
pub async fn list(
    root: &NodeAsyncClient,
    reactor: &Reactor,
    path: &str,
    suffix: &str,
) -> Result<Vec<String>, Failure> {
    let dir = open(root, reactor, path, OpenFlags::DIRECTORY).await?;
    let mut names = Vec::new();
    let mut cookie = 0;
    loop {
        let listed = match dir.read_directory(cookie, false).await {
            Ok(Ok(listed)) => listed,
            Ok(Err(e)) => return Err(Failure::Fs(e)),
            Err(_) => return Err(Failure::Channel),
        };
        if listed.entries.is_empty() {
            return Ok(names);
        }
        names.extend(
            listed
                .entries
                .into_iter()
                .filter(|e| e.r#type == NodeType::File && e.name.ends_with(suffix))
                .map(|e| e.name),
        );
        cookie = listed.next;
    }
}

/// The contents of the file at `path` under `root`, up to `max` bytes.
pub async fn read(
    root: &NodeAsyncClient,
    reactor: &Reactor,
    path: &str,
    max: usize,
) -> Result<Vec<u8>, Failure> {
    let file = open(root, reactor, path, OpenFlags(0)).await?;
    let mut bytes = Vec::new();
    loop {
        let data = match file.read(bytes.len() as u64, READ_MAX).await {
            Ok(Ok(read)) => read.data,
            Ok(Err(e)) => return Err(Failure::Fs(e)),
            Err(_) => return Err(Failure::Channel),
        };
        if data.is_empty() {
            return Ok(bytes);
        }
        bytes.extend_from_slice(&data);
        if bytes.len() > max {
            return Err(Failure::TooLarge);
        }
    }
}
