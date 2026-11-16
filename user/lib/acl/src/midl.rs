//! Lists as `mikros.fs` has them (MIKSPEC-0049.R28).

use crate::{Entry, Tag};
use alloc::vec::Vec;
use mikros_rt::interfaces::mikros::fs::{AclEntry, AclTag, Error, Rights};

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
