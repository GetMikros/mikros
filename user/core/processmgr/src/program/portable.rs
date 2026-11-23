//! Portable apps and builds (MIKSPEC-0101.R46, MIKSPEC-0101.R47): a
//! package file `<name>.mika` started as a program runs its `executable`
//! from the package's files, which the install service serves, with the
//! folder `<name>` next to it as its own; and a program whose own folder
//! holds a `manifest.yaml` runs as the app that it names, with that
//! folder as its own. Either is refused when a package with its ID is
//! installed.

use crate::program::namespace::Programs;
use crate::program::processes::Shared;
use alloc::format;
use alloc::rc::Rc;
use alloc::string::String;
use mikros_mika::format::MAGIC;
use mikros_mika::manifest::Manifest;
use mikros_rt::interfaces::__midl::endpoints;
use mikros_rt::interfaces::mikros::base::Status;
use mikros_rt::interfaces::mikros::fs::{
    Error, Node, NodeAsyncClient, NodeType, OpenFlags, Rights,
};

/// The suffix of package files, and the file of a build's manifest.
const SUFFIX: &str = ".mika";
const MANIFEST: &str = "manifest.yaml";
/// The folders of a home folder that no portable app may take
/// (MIKSPEC-0100.R13).
const HOME_FOLDERS: [&str; 9] = [
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
/// The largest manifest of a build that is read.
const MANIFEST_MAX: u32 = 1 << 15;
/// The `portable_checksum` of a build (MIKSPEC-0036.R41).
pub const BUILD: &str = "build";

/// How a portable app or a build runs, which its processes that keep its
/// app ID share.
pub struct Portable {
    pub id: String,
    /// The process manager's connection to the package's files, which the
    /// install service serves; none for a build.
    pub package: Option<Rc<NodeAsyncClient>>,
    /// The app's own folder, at its real path, and the rights it gets
    /// there.
    pub folder: String,
    pub rights: Rights,
    pub checksum: String,
    /// What `MIKA_EXECUTABLE` names: the package file or the program, in
    /// the spawner's namespace.
    pub executable: String,
}

const RX: Rights = Rights(Rights::READ.0 | Rights::EXECUTE.0);

fn status(e: Error) -> Status {
    match e {
        Error::NotFound => Status::NotFound,
        Error::AccessDenied | Error::ReadOnly => Status::AccessDenied,
        Error::Exists => Status::AlreadyExists,
        Error::Invalid | Error::NameTooLong => Status::InvalidArgs,
        _ => Status::Io,
    }
}

type Reply<T> = Result<Result<T, Error>, mikros_rt::interfaces::__midl::Error>;

fn flat<T>(reply: Reply<T>) -> Result<T, Status> {
    reply.unwrap_or(Err(Error::Io)).map_err(status)
}

/// The folder of the root file system at its real path `real`, opened with
/// `rights` for the process manager.
async fn folder(shared: &Shared, real: &str, rights: Rights) -> Result<NodeAsyncClient, Status> {
    let (entry, _) = shared.namespaces.folder(shared, real, rights).await?;
    Ok(NodeAsyncClient::new(
        entry.directory,
        shared.reactor.clone(),
    ))
}

/// `name` in `dir`, opened with `flags` and `rights`, and what it is.
async fn open(
    shared: &Shared,
    dir: &NodeAsyncClient,
    name: &str,
    flags: OpenFlags,
    mode: u32,
    rights: Rights,
) -> Result<(NodeAsyncClient, NodeType), Status> {
    let (client, server) = endpoints::<Node>().map_err(|_| Status::NoResources)?;
    let opened = flat(dir.open(name.into(), flags, mode, rights, server).await)?;
    Ok((
        NodeAsyncClient::new(client, shared.reactor.clone()),
        opened.r#type,
    ))
}

/// The folder that holds the normalized absolute `path`, and the name of
/// `path` in it.
fn split(path: &str) -> (&str, &str) {
    match path.rsplit_once('/') {
        Some(("", name)) => ("/", name),
        Some((parent, name)) => (parent, name),
        None => ("/", path),
    }
}

/// Whether a package with the ID `id` is installed for every user or for
/// `user`.
async fn installed(shared: &Shared, id: &str, user: u32) -> Result<bool, Status> {
    if shared
        .namespaces
        .folder(shared, &format!("/apps/{id}"), Rights::READ)
        .await
        .is_ok()
    {
        return Ok(true);
    }
    if user == 0 {
        return Ok(false);
    }
    let name = crate::program::users::name(shared, user).await?;
    let local = format!("/users/{name}/apps/{id}/package");
    Ok(shared
        .namespaces
        .folder(shared, &local, Rights::READ)
        .await
        .is_ok())
}

/// The portable app that the program at `path`, which `programs` resolve,
/// starts for `user` when it is a package file, and the path of its
/// `executable` in the app's namespace (MIKSPEC-0101.R46). `None` for any
/// other file.
pub async fn package(
    shared: &Shared,
    programs: &Programs,
    user: u32,
    path: &str,
) -> Result<Option<(Portable, String)>, Status> {
    let Some(normal) = mikros_manifest::path::normalize(path) else {
        return Ok(None);
    };
    let (parent, file) = split(&normal);
    let Some(name) = file.strip_suffix(SUFFIX).filter(|n| !n.is_empty()) else {
        return Ok(None);
    };
    let Some((real_parent, rights)) = programs.folder_of(parent) else {
        return Ok(None);
    };
    let dir = folder(shared, &real_parent, rights).await?;
    // Executable when the file and the entry allow it, so that the
    // package's programs can be cloned from it rather than copied.
    let mut readable = Rights(rights.0 & RX.0);
    let package = match open(shared, &dir, file, OpenFlags(0), 0, readable).await {
        Err(Status::AccessDenied) if readable != Rights::READ => {
            readable = Rights::READ;
            open(shared, &dir, file, OpenFlags(0), 0, readable).await
        }
        opened => opened,
    };
    let (package, _) = package?;
    let magic = flat(package.read(0, MAGIC.len() as u32).await)?.data;
    if magic != MAGIC {
        return Ok(None);
    }
    // The install service checks the package and serves its files.
    let (given, server) = endpoints::<Node>().map_err(|_| Status::NoResources)?;
    flat(package.clone(server, readable).await)?;
    let (files, server) = endpoints::<Node>().map_err(|_| Status::NoResources)?;
    let manifest = crate::program::users::open_package(shared, given, server).await?;
    let manifest = Manifest::parse(&manifest).map_err(|_| Status::InvalidArgs)?;
    if !manifest.portable {
        return Err(Status::NotSupported);
    }
    let executable = manifest.executable.clone().ok_or(Status::InvalidArgs)?;
    if installed(shared, &manifest.id, user).await? {
        return Err(Status::AlreadyExists);
    }
    // The app's folder, next to the package: never one of a home folder's
    // own, made when it is missing.
    if user != 0 {
        let home = format!(
            "/users/{}",
            crate::program::users::name(shared, user).await?
        );
        if real_parent == home && HOME_FOLDERS.contains(&name) {
            return Err(Status::AccessDenied);
        }
    }
    let rw = Rights(rights.0 & !Rights::EXECUTE.0);
    match open(shared, &dir, name, OpenFlags::NO_FOLLOW, 0, Rights::READ).await {
        Ok((_, NodeType::Directory)) => {}
        Ok(_) => return Err(Status::AlreadyExists),
        Err(Status::NotFound) => {
            let flags =
                OpenFlags(OpenFlags::CREATE.0 | OpenFlags::DIRECTORY.0 | OpenFlags::EXCLUSIVE.0);
            let (made, _) = open(shared, &dir, name, flags, 0o755, rw).await?;
            if user != 0 {
                let _ = made.set_owner(user).await;
            }
        }
        Err(e) => return Err(e),
    }
    let own = match real_parent.as_str() {
        "/" => format!("/{name}"),
        parent => format!("{parent}/{name}"),
    };
    let portable = Portable {
        id: manifest.id.clone(),
        package: Some(Rc::new(NodeAsyncClient::new(files, shared.reactor.clone()))),
        folder: own,
        rights: rw,
        checksum: manifest.checksum.clone().unwrap_or_default(),
        executable: normal.clone(),
    };
    let program = format!("/apps/{}/{executable}", manifest.id);
    Ok(Some((portable, program)))
}

/// The build that the program at `path`, which `programs` resolve, runs as
/// for `user` when its own folder holds a manifest with an ID
/// (MIKSPEC-0101.R47). `None` for a program in any other folder.
pub async fn build(
    shared: &Shared,
    programs: &Programs,
    user: u32,
    path: &str,
) -> Result<Option<Portable>, Status> {
    let Some(normal) = mikros_manifest::path::normalize(path) else {
        return Ok(None);
    };
    let (parent, _) = split(&normal);
    let Some((real_parent, rights)) = programs.folder_of(parent) else {
        return Ok(None);
    };
    // The system's programs are never builds.
    if real_parent == "/system" || real_parent.starts_with("/system/") {
        return Ok(None);
    }
    let Ok(dir) = folder(shared, &real_parent, rights).await else {
        return Ok(None);
    };
    let Ok((manifest, NodeType::File)) =
        open(shared, &dir, MANIFEST, OpenFlags(0), 0, Rights::READ).await
    else {
        return Ok(None);
    };
    let bytes = flat(manifest.read(0, MANIFEST_MAX).await)?.data;
    let Ok(manifest) = Manifest::parse(&bytes) else {
        return Ok(None);
    };
    if installed(shared, &manifest.id, user).await? {
        return Err(Status::AlreadyExists);
    }
    Ok(Some(Portable {
        id: manifest.id,
        package: None,
        folder: real_parent,
        rights,
        checksum: String::from(BUILD),
        executable: normal,
    }))
}
