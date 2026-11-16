//! Users' home folders: a subvolume of the root file system at
//! `/users/<name>`, with the folders that every user has, all owned by
//! the user (MIKSPEC-0100.R17).

use alloc::format;
use mikros_rt::exec::Reactor;
use mikros_rt::interfaces::__midl::endpoints;
use mikros_rt::interfaces::mikros::base::Status;
use mikros_rt::interfaces::mikros::fs::btrfs::{Subvolumes, SubvolumesAsyncClient};
use mikros_rt::interfaces::mikros::fs::{
    Error, Node, NodeAsyncClient, NodeType, OpenFlags, Rights,
};
use mikros_rt::interfaces::mikros::service::DirectoryAsyncClient;

const SUBVOLUMES: &str = "device.subvolumes";
/// The instance of the root file system's subvolume management.
const ROOT: &str = "root";
/// The folders of a home folder.
const FOLDERS: [&str; 9] = [
    "desktop",
    "documents",
    "downloads",
    "pictures",
    "music",
    "videos",
    "developer",
    "libraries",
    "apps",
];
/// The folder of a user's apps.
const APPS: &str = "apps";
/// The mode of new folders: read, write and search for their owner.
const FOLDER_MODE: u32 = 0o700;

/// The subvolume management of the root file system, or `NOT_SUPPORTED`
/// when the root has none, as the temporary file system of
/// `root=bootfs` has not.
pub async fn subvolumes(
    directory: &DirectoryAsyncClient,
    reactor: &Reactor,
) -> Result<SubvolumesAsyncClient, Status> {
    let (client, server) = endpoints::<Subvolumes>().map_err(|_| Status::NoResources)?;
    let connected = directory
        .connect(SUBVOLUMES.into(), Some(ROOT.into()), server.into_handle())
        .await;
    match connected {
        Ok(Ok(_)) => Ok(SubvolumesAsyncClient::new(client, reactor.clone())),
        _ => Err(Status::NotSupported),
    }
}

fn status(error: Error) -> Status {
    match error {
        Error::Exists => Status::AlreadyExists,
        Error::NoSpace => Status::NoResources,
        Error::NotSupported => Status::NotSupported,
        _ => Status::Io,
    }
}

/// Makes `/users/<name>` a new subvolume through `subvolumes`, and the
/// folders in it through `homes`, the service's `/users`, all owned by
/// `owner`. When the folders cannot be made, the subvolume is deleted
/// again, so that no home folder is left that would keep the name from
/// being used.
pub async fn create(
    subvolumes: &SubvolumesAsyncClient,
    homes: &NodeAsyncClient,
    name: &str,
    owner: u32,
    reactor: &Reactor,
) -> Result<(), Status> {
    let path = format!("users/{name}");
    match subvolumes.create(path.clone()).await {
        Ok(Ok(_)) => {}
        Ok(Err(e)) => return Err(status(e)),
        Err(_) => return Err(Status::PeerClosed),
    }
    let made = folders(homes, name, owner, reactor).await;
    if made.is_err() {
        let _ = subvolumes.delete(path).await;
    }
    made
}

/// Deletes the subvolume `/users/<name>` through `subvolumes`.
pub async fn delete(subvolumes: &SubvolumesAsyncClient, name: &str) -> Result<(), Status> {
    match subvolumes.delete(format!("users/{name}")).await {
        Ok(Ok(_)) => Ok(()),
        Ok(Err(e)) => Err(status(e)),
        Err(_) => Err(Status::PeerClosed),
    }
}

/// Makes the folders of the home folder `name` in `homes`, and makes the
/// home folder and each of them `owner`'s.
async fn folders(
    homes: &NodeAsyncClient,
    name: &str,
    owner: u32,
    reactor: &Reactor,
) -> Result<(), Status> {
    let home = open(homes, name, OpenFlags::DIRECTORY, reactor).await?;
    set_owner(&home, owner).await?;
    let make = OpenFlags(OpenFlags::CREATE.0 | OpenFlags::DIRECTORY.0 | OpenFlags::EXCLUSIVE.0);
    for folder in FOLDERS {
        let made = open(&home, folder, make, reactor).await?;
        set_owner(&made, owner).await?;
    }
    Ok(())
}

/// Makes the home folder `name` in `homes`, the folders of every user in
/// it, each folder in its `apps` and the folders in each of those
/// `owner`'s, where they have another owner.
pub async fn own(
    homes: &NodeAsyncClient,
    name: &str,
    owner: u32,
    reactor: &Reactor,
) -> Result<(), Status> {
    let home = open(homes, name, OpenFlags::DIRECTORY, reactor).await?;
    claim(&home, owner).await?;
    for folder in FOLDERS {
        if let Ok(found) = open(&home, folder, OpenFlags::DIRECTORY, reactor).await {
            claim(&found, owner).await?;
        }
    }
    let Ok(apps) = open(&home, APPS, OpenFlags::DIRECTORY, reactor).await else {
        return Ok(());
    };
    for app in folder_names(&apps).await? {
        let app = open(&apps, &app, OpenFlags::DIRECTORY, reactor).await?;
        claim(&app, owner).await?;
        for inner in folder_names(&app).await? {
            let inner = open(&app, &inner, OpenFlags::DIRECTORY, reactor).await?;
            claim(&inner, owner).await?;
        }
    }
    Ok(())
}

/// Makes `node` `owner`'s when it has another owner.
async fn claim(node: &NodeAsyncClient, owner: u32) -> Result<(), Status> {
    let attributes = match node.get_attributes().await {
        Ok(Ok(got)) => got.attributes,
        Ok(Err(e)) => return Err(status(e)),
        Err(_) => return Err(Status::Io),
    };
    match attributes.owner == owner {
        true => Ok(()),
        false => set_owner(node, owner).await,
    }
}

async fn set_owner(node: &NodeAsyncClient, owner: u32) -> Result<(), Status> {
    match node.set_owner(owner).await {
        Ok(Ok(_)) => Ok(()),
        Ok(Err(e)) => Err(status(e)),
        Err(_) => Err(Status::Io),
    }
}

/// `name` in `dir`, read and write, with `flags`.
async fn open(
    dir: &NodeAsyncClient,
    name: &str,
    flags: OpenFlags,
    reactor: &Reactor,
) -> Result<NodeAsyncClient, Status> {
    let (node, server) = endpoints::<Node>().map_err(|_| Status::NoResources)?;
    let rw = Rights(Rights::READ.0 | Rights::WRITE.0);
    match dir.open(name.into(), flags, FOLDER_MODE, rw, server).await {
        Ok(Ok(_)) => Ok(NodeAsyncClient::new(node, reactor.clone())),
        Ok(Err(e)) => Err(status(e)),
        Err(_) => Err(Status::Io),
    }
}

/// The names of the folders in `dir`.
async fn folder_names(
    dir: &NodeAsyncClient,
) -> Result<alloc::vec::Vec<alloc::string::String>, Status> {
    let mut names = alloc::vec::Vec::new();
    let mut cookie = 0;
    loop {
        let listed = match dir.read_directory(cookie, false).await {
            Ok(Ok(listed)) => listed,
            Ok(Err(e)) => return Err(status(e)),
            Err(_) => return Err(Status::Io),
        };
        if listed.entries.is_empty() {
            return Ok(names);
        }
        names.extend(
            listed
                .entries
                .into_iter()
                .filter(|e| e.r#type == NodeType::Directory && e.name != "." && e.name != "..")
                .map(|e| e.name),
        );
        cookie = listed.next;
    }
}
