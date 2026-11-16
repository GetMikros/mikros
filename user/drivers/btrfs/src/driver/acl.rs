// SPDX-License-Identifier: GPL-2.0
//! Access control lists (MIKSPEC-0049.R25, MIKSPEC-0049.R26): a node's
//! lists are its extended attributes `system.posix_acl_access` and
//! `system.posix_acl_default`, in the format of Linux, which each open
//! node reads once. An access list of three entries is not kept, as its
//! entries are those of the mode. The subjects of connections
//! (MIKSPEC-0049.R30) meet the lists, and `Restrict` gives one.

use crate::driver::fs::{Open, error};
use crate::driver::node::Connection;
use crate::linux::node::{Changes, Node as BtrfsNode};
use alloc::vec::Vec;
use core::ffi::CStr;
use mikros_acl::{Entry, Subject};
use mikros_rt::interfaces::mikros::fs::*;

const ACCESS: &CStr = c"system.posix_acl_access";
const DEFAULT: &CStr = c"system.posix_acl_default";
/// More than the value of a list of the most entries a reply holds.
const VALUE_MAX: usize = 1024;
/// The bits of a mode that hold its type.
const MODE_TYPE: u32 = 0o170000;
const MODE_DIRECTORY: u32 = 0o040000;

/// A node's lists: its access list when it is extended, and its default
/// list when it has one.
#[derive(Clone, Default)]
pub struct Lists {
    pub access: Option<Vec<Entry>>,
    pub default: Option<Vec<Entry>>,
}

/// The list that the extended attribute `name` of `node` holds; none
/// when it holds none, or one that cannot be read as a list.
fn read(node: &BtrfsNode<'_>, name: &CStr) -> Result<Option<Vec<Entry>>, Error> {
    let value = node.xattr(name, VALUE_MAX).map_err(error)?;
    Ok(value.and_then(|v| mikros_acl::decode(&v)))
}

/// Stores `list` as the extended attribute `name` of `node`, or removes
/// the attribute without one.
fn write(node: &BtrfsNode<'_>, name: &CStr, list: Option<&[Entry]>) -> Result<(), Error> {
    let value = list.map(mikros_acl::encode);
    node.set_xattr(name, value.as_deref()).map_err(error)
}

impl Open {
    /// The node's lists, read when first asked for.
    pub fn lists(&self) -> Result<Lists, Error> {
        if let Some(lists) = &*self.lists.borrow() {
            return Ok(lists.clone());
        }
        let access = read(&self.node, ACCESS)?.filter(|l| mikros_acl::is_extended(l));
        let directory = self.is_directory()?;
        let default = match directory {
            true => read(&self.node, DEFAULT)?,
            false => None,
        };
        let lists = Lists { access, default };
        *self.lists.borrow_mut() = Some(lists.clone());
        Ok(lists)
    }

    /// Replaces the access list, or with `default` the default list,
    /// which `None` removes, and sets the mode from an access list.
    fn set_list(&self, default: bool, list: Option<Vec<Entry>>) -> Result<(), Error> {
        let mut lists = self.lists()?;
        match default {
            true => {
                write(&self.node, DEFAULT, list.as_deref())?;
                lists.default = list;
            }
            false => {
                let Some(list) = list else {
                    return Err(Error::Invalid);
                };
                let mode = mikros_acl::mode_of(&list, self.attributes()?.mode);
                let extended = mikros_acl::is_extended(&list).then_some(list);
                write(&self.node, ACCESS, extended.as_deref())?;
                let changes = Changes {
                    mode: Some(mode & 0o7777),
                    ..Changes::default()
                };
                self.node.set_attributes(&changes).map_err(error)?;
                lists.access = extended;
            }
        }
        *self.lists.borrow_mut() = Some(lists);
        Ok(())
    }
}

impl Connection {
    /// Fails with `ACCESS_DENIED` unless the connection has `rights` and
    /// the access list of `open` allows them to its subject.
    pub(crate) fn allowed(&self, open: &Open, rights: Rights) -> Result<(), Error> {
        self.require(rights)?;
        let Some(subject) = self.subject.as_ref().filter(|s| s.user != 0) else {
            return Ok(());
        };
        let lists = open.lists()?;
        let owner = open.attributes()?.owner;
        match mikros_acl::permits(Some(subject), lists.access.as_deref(), owner, rights.0) {
            true => Ok(()),
            false => Err(Error::AccessDenied),
        }
    }

    /// Makes the connection's subject the owner of `node`, which it made
    /// in `parent` with `mode`, and gives it `parent`'s default list.
    pub(crate) fn own(&self, node: &BtrfsNode<'_>, parent: &Open, mode: u32) -> Result<(), Error> {
        let default = parent.lists()?.default;
        let directory = node.attributes().map_err(error)?.mode & MODE_TYPE == MODE_DIRECTORY;
        let inherited = mikros_acl::inherit(default.as_deref(), mode, directory);
        if let Some(access) = &inherited.access {
            write(node, ACCESS, Some(access))?;
        }
        if let Some(default) = &inherited.default {
            write(node, DEFAULT, Some(default))?;
        }
        let changes = Changes {
            owner: Some(mikros_acl::new_owner(self.subject.as_ref())),
            mode: default.is_some().then_some(inherited.mode),
            ..Changes::default()
        };
        node.set_attributes(&changes).map_err(error)
    }

    /// The node's access list, or with `default` a directory's default
    /// list (MIKSPEC-0049.R31).
    pub(crate) fn get_acl(&self, default: bool) -> Result<NodeGetAclResponse, Error> {
        let lists = self.open.lists()?;
        let directory = self.open.is_directory()?;
        let entries = match (default, lists.access, lists.default) {
            (false, Some(list), _) | (true, _, Some(list)) => mikros_acl::midl::to_midl(&list),
            _ => mikros_acl::midl::of_mode(self.open.attributes()?.mode, directory, default)?,
        };
        Ok(NodeGetAclResponse { entries })
    }

    /// Replaces the node's access list, setting its mode from it, or a
    /// directory's default list, which an empty list removes. A subject
    /// other than user 0 and the node's owner needs `admin.acls.manage`.
    pub(crate) async fn set_acl(&self, q: NodeSetAclRequest) -> Result<(), Error> {
        self.require_write()?;
        if q.default && !self.open.is_directory()? {
            return Err(Error::NotDirectory);
        }
        let list = match q.default && q.entries.is_empty() {
            true => None,
            false => Some(mikros_acl::midl::from_midl(&q.entries)?),
        };
        let owner = self.open.attributes()?.owner;
        if let Some(subject) = &self.subject
            && mikros_acl::needs_manage(Some(subject), owner)
            && !self.fs.may_manage(subject).await
        {
            return Err(Error::AccessDenied);
        }
        self.open.set_list(q.default, list)?;
        self.fs.changed(&self.open, WatchKind::Attributes);
        Ok(())
    }

    /// Serves `q.node` as a connection to the same node with the same
    /// rights for the subject of `q`: `BAD_STATE` when this connection
    /// has a subject already.
    pub(crate) fn restrict(&self, q: NodeRestrictRequest) -> Result<NodeRestrictResponse, Error> {
        if self.subject.is_some() {
            return Err(Error::BadState);
        }
        let subject = Subject {
            user: q.user,
            app: q.app,
            pid: q.pid,
            koid: q.koid,
            admin: q.admin,
        };
        self.fs.serve(
            q.node,
            self.open.clone(),
            self.rights,
            Some(subject),
            self.append,
            self.folded,
        );
        Ok(NodeRestrictResponse {})
    }
}
