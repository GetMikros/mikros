//! Lists as `mikros.fs` has them (MIKSPEC-0049.R29), and asking the
//! broker whether a subject may set the lists of others' nodes
//! (MIKSPEC-0049.R26, MIKSPEC-0049.R35).

use crate::{Entry, Subject, Tag};
use alloc::vec::Vec;
use mikros_rt::interfaces::mikros::fs::{AclEntry, AclTag, Error, Rights};
use mikros_rt::interfaces::mikros::permissions::{
    AppNumbersAsyncClient, BrokerAsyncClient, Decision,
};
use mikros_rt::interfaces::mikros::service::ClientIdentity;

/// The permission that setting the lists of another user's node needs.
pub const MANAGE: &str = "admin.acls.manage";
/// What the administrator is told the permission is asked for.
const REASON: &str = "change who may use a file or folder of another user";

/// Whether `subject` holds `MANAGE`, asked for with `Request` of `broker`,
/// with the app ID that `numbers` gives for its app number; not when
/// either fails.
pub async fn may_manage(
    broker: &BrokerAsyncClient,
    numbers: &AppNumbersAsyncClient,
    subject: &Subject,
) -> bool {
    // App number 0 is no app.
    let app_id = match subject.app {
        0 => alloc::string::String::new(),
        n => match numbers.app_id(n).await {
            Ok(Ok(r)) => r.app_id,
            _ => return false,
        },
    };
    let client = ClientIdentity {
        pid: subject.pid,
        user: subject.user,
        app_id,
        admin: subject.admin,
        koid: subject.koid,
        portable_checksum: subject.portable_checksum.as_str().into(),
    };
    let requested = broker.request(client, MANAGE.into(), REASON.into()).await;
    matches!(requested, Ok(Ok(r)) if r.decision == Decision::Allowed)
}

/// `entries` as `mikros.fs` entries.
pub fn to_midl(entries: &[Entry]) -> Vec<AclEntry> {
    entries
        .iter()
        .map(|e| AclEntry {
            tag: match e.tag {
                Tag::Owner => AclTag::Owner,
                Tag::User => AclTag::User,
                Tag::Group => AclTag::Group,
                Tag::App => AclTag::App,
                Tag::Mask => AclTag::Mask,
                Tag::Other => AclTag::Other,
            },
            id: e.id,
            rights: Rights(e.rights),
        })
        .collect()
}

/// The list that the `mikros.fs` entries `entries` form, checked and in
/// order, or `INVALID` when they form none.
pub fn from_midl(entries: &[AclEntry]) -> Result<Vec<Entry>, Error> {
    let mut list = Vec::with_capacity(entries.len());
    for e in entries {
        let tag = match e.tag {
            AclTag::Owner => Tag::Owner,
            AclTag::User => Tag::User,
            AclTag::Group => Tag::Group,
            AclTag::App => Tag::App,
            AclTag::Mask => Tag::Mask,
            AclTag::Other => Tag::Other,
            AclTag::Unknown(_) => return Err(Error::Invalid),
        };
        list.push(Entry::new(tag, e.id, e.rights.0));
    }
    crate::checked(&list).ok_or(Error::Invalid)
}

/// What `GetAcl` returns for a node of `mode` that has no extended list
/// and, if it is a directory, no default list: the three entries of
/// `mode`, or with `default` an empty list for a directory.
pub fn of_mode(mode: u32, directory: bool, default: bool) -> Result<Vec<AclEntry>, Error> {
    match (default, directory) {
        (false, _) => Ok(to_midl(&crate::from_mode(mode))),
        (true, true) => Ok(Vec::new()),
        (true, false) => Err(Error::NotDirectory),
    }
}
