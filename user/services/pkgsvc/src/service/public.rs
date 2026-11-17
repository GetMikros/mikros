//! `CreatePublic` and `OpenPublic` (MIKSPEC-0101.R48, MIKSPEC-0101.R53):
//! an installed app makes its public folder, `public/<id>`, a subvolume
//! of user 0 whose lists let the app write and everyone else read; and a
//! process that holds `admin.public.write` gets a connection to write an
//! app's public folder.

use super::files::{Root, user_name};
use super::install::installed;
use super::{BROKER_NUMBERS, FOREVER, Shared, request};
use crate::place::Scope;
use mikros_mika::manifest::Kind;
use mikros_rt::interfaces::__midl::{ServerEnd, endpoints};
use mikros_rt::interfaces::mikros::base::Status;
use mikros_rt::interfaces::mikros::fs::{AclEntry, AclTag, Node, OpenFlags, Rights};
use mikros_rt::interfaces::mikros::permissions::{AppNumbers, AppNumbersClient};
use mikros_rt::interfaces::mikros::service::ClientIdentity;

const WRITE: &str = "admin.public.write";
const RW: Rights = Rights(Rights::READ.0 | Rights::WRITE.0);
const RX: Rights = Rights(Rights::READ.0 | Rights::EXECUTE.0);
const RWX: Rights = Rights(Rights::READ.0 | Rights::WRITE.0 | Rights::EXECUTE.0);

/// The public folder of `id`, from the root.
fn folder(id: &str) -> String {
    format!("public/{id}")
}

/// Whether the app `id` is installed for every user or for `user`.
fn installed_for(shared: &Shared, root: &Root, id: &str, user: u32) -> Result<bool, Status> {
    if installed(root, Kind::App, id, &Scope::Global).is_some() {
        return Ok(true);
    }
    if user == 0 {
        return Ok(false);
    }
    let directory = shared.directory.lock().map_err(|_| Status::Internal)?;
    let name = user_name(&directory, user)?;
    drop(directory);
    Ok(installed(root, Kind::App, id, &Scope::Local(name)).is_some())
}

/// The broker's number of the app ID `id`.
fn app_number(shared: &Shared, id: &str) -> Result<u32, Status> {
    let (client, server) = endpoints::<AppNumbers>().map_err(|_| Status::NoResources)?;
    {
        let directory = shared.directory.lock().map_err(|_| Status::Internal)?;
        match directory.connect(BROKER_NUMBERS.into(), None, server.into_handle(), FOREVER) {
            Ok(Ok(_)) => {}
            Ok(Err(status)) => return Err(status),
            Err(_) => return Err(Status::Io),
        }
    }
    match AppNumbersClient::new(client).number(id.into(), FOREVER) {
        Ok(Ok(r)) => Ok(r.number),
        Ok(Err(status)) => Err(status),
        Err(_) => Err(Status::Io),
    }
}

/// The result of a call to a file system server.
fn done<T>(
    reply: Result<
        Result<T, mikros_rt::interfaces::mikros::fs::Error>,
        mikros_rt::interfaces::__midl::Error,
    >,
) -> Result<(), Status> {
    match reply {
        Ok(Ok(_)) => Ok(()),
        Ok(Err(e)) => Err(super::files::status(e)),
        Err(_) => Err(Status::Io),
    }
}

/// Makes the public folder of the caller's app, which needs no permission
/// (MIKSPEC-0101.R53).
pub fn create(shared: &Shared, caller: &ClientIdentity) -> Result<(), Status> {
    let id = caller.app_id.as_str();
    let portable = !caller.portable_checksum.is_empty();
    if portable || id.starts_with("system.") || !mikros_mika::id::is_valid(id) {
        return Err(Status::AccessDenied);
    }
    let root = shared.root()?;
    if !installed_for(shared, &root, id, caller.user)? {
        return Err(Status::AccessDenied);
    }
    let at = folder(id);
    if root
        .open(&at, OpenFlags::DIRECTORY, 0, Rights::READ)
        .is_ok()
    {
        return Err(Status::AlreadyExists);
    }
    let number = app_number(shared, id)?;
    shared.subvolumes()?.create(&at)?;
    let made = (|| {
        let dir = root.open(&at, OpenFlags::DIRECTORY, 0, RW)?;
        done(dir.set_owner(0, FOREVER))?;
        let entries = || {
            vec![
                AclEntry {
                    tag: AclTag::Owner,
                    id: 0,
                    rights: RWX,
                },
                AclEntry {
                    tag: AclTag::Group,
                    id: 0,
                    rights: RX,
                },
                AclEntry {
                    tag: AclTag::App,
                    id: number,
                    rights: RWX,
                },
                AclEntry {
                    tag: AclTag::Mask,
                    id: 0,
                    rights: RWX,
                },
                AclEntry {
                    tag: AclTag::Other,
                    id: 0,
                    rights: RX,
                },
            ]
        };
        done(dir.set_acl(false, entries(), FOREVER))?;
        done(dir.set_acl(true, entries(), FOREVER))
    })();
    if made.is_err() {
        let _ = shared.subvolumes().map(|s| s.delete(&at));
    }
    made
}

/// Gives `server` a connection to write the public folder of `id`, to a
/// caller that holds `admin.public.write` (MIKSPEC-0101.R48).
pub fn open(
    shared: &Shared,
    caller: &ClientIdentity,
    id: &str,
    server: ServerEnd<Node>,
) -> Result<(), Status> {
    if !mikros_mika::id::is_valid(id) {
        return Err(Status::InvalidArgs);
    }
    if !request(
        shared,
        caller,
        WRITE,
        &format!("write to the public folder of {id}"),
    )? {
        return Err(Status::AccessDenied);
    }
    let root = shared.root()?;
    root.open_on(&folder(id), OpenFlags::DIRECTORY, RW, server)
        .map(|_| ())
}
