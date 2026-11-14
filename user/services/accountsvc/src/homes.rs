//! Users' home folders: a subvolume of the root file system at
//! `/Users/<name>`, with the folders that every user has.

use alloc::format;
use mikros_rt::exec::Reactor;
use mikros_rt::interfaces::__midl::endpoints;
use mikros_rt::interfaces::mikros::base::Status;
use mikros_rt::interfaces::mikros::fs::btrfs::{Subvolumes, SubvolumesAsyncClient};
use mikros_rt::interfaces::mikros::fs::{Error, Node, NodeAsyncClient, OpenFlags, Rights};
use mikros_rt::interfaces::mikros::service::DirectoryAsyncClient;

const SUBVOLUMES: &str = "device.subvolumes";
/// The instance of the root file system's subvolume management.
const ROOT: &str = "root";
/// The folders of a home folder.
const FOLDERS: [&str; 9] = [
    "Desktop",
    "Documents",
    "Downloads",
    "Pictures",
    "Music",
    "Videos",
    "Developer",
    "Libraries",
    "Apps",
];
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

/// Makes `/Users/<name>` a new subvolume through `subvolumes`, and the
/// folders in it through `homes`, the service's `/Users`. When the
/// folders cannot be made, the subvolume is deleted again, so that no
/// home folder is left that would keep the name from being used.
pub async fn create(
    subvolumes: &SubvolumesAsyncClient,
    homes: &NodeAsyncClient,
    name: &str,
    reactor: &Reactor,
) -> Result<(), Status> {
    let path = format!("Users/{name}");
    match subvolumes.create(path.clone()).await {
        Ok(Ok(_)) => {}
        Ok(Err(e)) => return Err(status(e)),
        Err(_) => return Err(Status::PeerClosed),
    }
    let made = folders(homes, name, reactor).await;
    if made.is_err() {
        let _ = subvolumes.delete(path).await;
    }
    made
}

/// Deletes the subvolume `/Users/<name>` through `subvolumes`.
pub async fn delete(subvolumes: &SubvolumesAsyncClient, name: &str) -> Result<(), Status> {
    match subvolumes.delete(format!("Users/{name}")).await {
        Ok(Ok(_)) => Ok(()),
        Ok(Err(e)) => Err(status(e)),
        Err(_) => Err(Status::PeerClosed),
    }
}

/// Makes the folders of the home folder `name` in `homes`.
async fn folders(homes: &NodeAsyncClient, name: &str, reactor: &Reactor) -> Result<(), Status> {
    let (home, server) = endpoints::<Node>().map_err(|_| Status::NoResources)?;
    let rw = Rights(Rights::READ.0 | Rights::WRITE.0);
    match homes
        .open(name.into(), OpenFlags::DIRECTORY, 0, rw, server)
        .await
    {
        Ok(Ok(_)) => {}
        Ok(Err(e)) => return Err(status(e)),
        Err(_) => return Err(Status::Io),
    }
    let home = NodeAsyncClient::new(home, reactor.clone());
    let make = OpenFlags(OpenFlags::CREATE.0 | OpenFlags::DIRECTORY.0 | OpenFlags::EXCLUSIVE.0);
    for folder in FOLDERS {
        let (_, server) = endpoints::<Node>().map_err(|_| Status::NoResources)?;
        match home
            .open(folder.into(), make, FOLDER_MODE, rw, server)
            .await
        {
            Ok(Ok(_)) => {}
            Ok(Err(e)) => return Err(status(e)),
            Err(_) => return Err(Status::Io),
        }
    }
    Ok(())
}
