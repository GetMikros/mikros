// SPDX-License-Identifier: GPL-2.0
//! The requests of a connection that change the entries of directories:
//! removing them, renaming and linking them through tokens
//! (MIKSPEC-0049.R8), and creating symbolic links. On a case-insensitive
//! connection, existing names are found as `Open` with `CASE_INSENSITIVE`
//! finds them, and new names may not equal existing ones under case
//! folding (MIKSPEC-0071.R17).

use crate::fs::{Fs, Open, error};
use crate::node::{Connection, NAME_MAX, TARGET_MAX};
use crate::watch::event;
use alloc::rc::Rc;
use alloc::string::String;
use mikros_btrfs::node::{Changes, Kind};
use mikros_rt::interfaces::__midl::OwnedHandle;
use mikros_rt::interfaces::mikros::fs::*;
use mikros_rt::sys;

const INFO_BASIC: u32 = 1;
/// The rights of a token: DUPLICATE, TRANSFER and INSPECT.
const TOKEN_RIGHTS: u32 = 1 << 0 | 1 << 1 | 1 << 9;

impl Connection {
    /// Makes the connection's user the owner of a node it created.
    pub(crate) fn own(&self, node: &mikros_btrfs::node::Node<'_>) -> Result<(), Error> {
        let changes = Changes {
            owner: Some(self.user),
            ..Changes::default()
        };
        node.set_attributes(&changes).map_err(error)
    }

    pub(crate) async fn unlink(&self, name: &str, directory: bool) -> Result<(), Error> {
        self.require_write()?;
        self.directory()?;
        check_component(name)?;
        let (removed, name) = match self.find(&self.open, name).await? {
            Some((open, name)) => (Some(open), name),
            None => (None, String::from(name)),
        };
        let name = name.as_str();
        if self.fs.is_usr(&self.open, name) {
            return Err(Error::AccessDenied);
        }
        self.open
            .node
            .remove(name.as_bytes(), directory)
            .map_err(error)?;
        self.fs.renamed(self.open.key, name, None);
        self.fs
            .watches
            .notify(self.open.key, &[event(WatchKind::Removed, 0, name)]);
        if let Some(removed) = removed
            && removed
                .attributes()
                .map_or(true, |a| a.links == 0 || directory)
        {
            self.fs
                .watches
                .notify(removed.key, &[event(WatchKind::SelfRemoved, 0, "")]);
        }
        Ok(())
    }

    pub(crate) fn token(&self) -> Result<NodeGetTokenResponse, Error> {
        self.require(Rights::WRITE)?;
        self.directory()?;
        Ok(NodeGetTokenResponse {
            token: self.fs.token(self.open.clone())?,
        })
    }

    pub(crate) async fn rename(&self, q: NodeRenameRequest) -> Result<(), Error> {
        self.require_write()?;
        self.directory()?;
        check_component(&q.name)?;
        check_component(&q.new_name)?;
        let to = self.fs.token_dir(&q.target).ok_or(Error::CrossDevice)?;
        let (moved, name) = self
            .find(&self.open, &q.name)
            .await?
            .ok_or(Error::NotFound)?;
        // An entry equal to the new name under case folding is replaced as
        // one of exactly that name: the moved entry first takes that
        // entry's own name, which replaces it atomically, and then the new
        // name as given. The moved entry itself only changes its case.
        let replacing = match self.find(&to, &q.new_name).await? {
            Some((found, own))
                if own != q.new_name && !(found.key == moved.key && to.key == self.open.key) =>
            {
                Some(own)
            }
            _ => None,
        };
        let q = NodeRenameRequest { name, ..q };
        let target = replacing.as_deref().unwrap_or(&q.new_name);
        let usr = self.fs.is_usr(&to, target) || self.fs.is_usr(&to, &q.new_name);
        if self.fs.is_usr(&self.open, &q.name) || usr {
            return Err(Error::AccessDenied);
        }
        let replaced = self.lookup(&to, target).await?;
        self.open
            .node
            .rename(q.name.as_bytes(), &to.node, target.as_bytes(), 0)
            .map_err(error)?;
        let renamed = match &replacing {
            Some(own) => to
                .node
                .rename(own.as_bytes(), &to.node, q.new_name.as_bytes(), 0)
                .map_err(error),
            None => Ok(()),
        };
        // Should giving the new name fail, the entry keeps the other one.
        let new_name = match renamed {
            Ok(()) => q.new_name.as_str(),
            Err(_) => target,
        };
        if let Some(replaced) = &replaced
            && replaced.key != moved.key
        {
            self.fs.renamed(to.key, target, None);
            self.fs
                .watches
                .notify(replaced.key, &[event(WatchKind::SelfRemoved, 0, "")]);
        }
        self.fs
            .renamed(self.open.key, &q.name, Some((to.key, new_name)));
        let cookie = self.fs.watches.move_cookie();
        let from = event(WatchKind::MovedFrom, cookie, &q.name);
        let into = event(WatchKind::MovedTo, cookie, new_name);
        if to.key == self.open.key {
            self.fs.watches.notify(to.key, &[from, into]);
        } else {
            self.fs.watches.notify(self.open.key, &[from]);
            self.fs.watches.notify(to.key, &[into]);
        }
        self.fs
            .watches
            .notify(moved.key, &[event(WatchKind::SelfMoved, 0, "")]);
        renamed
    }

    pub(crate) async fn link(&self, q: NodeLinkRequest) -> Result<(), Error> {
        self.require_write()?;
        self.directory()?;
        check_component(&q.name)?;
        check_component(&q.new_name)?;
        let to = self.fs.token_dir(&q.target).ok_or(Error::CrossDevice)?;
        if self.fs.is_usr(&to, &q.new_name) {
            return Err(Error::AccessDenied);
        }
        let (node, _) = self
            .find(&self.open, &q.name)
            .await?
            .ok_or(Error::NotFound)?;
        self.free_folded(&to, &q.new_name).await?;
        node.node
            .link(&to.node, q.new_name.as_bytes())
            .map_err(error)?;
        self.fs
            .watches
            .notify(to.key, &[event(WatchKind::Created, 0, &q.new_name)]);
        self.fs.changed(&node, WatchKind::Attributes);
        Ok(())
    }

    pub(crate) async fn create_symlink(&self, name: &str, target: &str) -> Result<(), Error> {
        self.require_write()?;
        self.directory()?;
        check_component(name)?;
        if self.fs.is_usr(&self.open, name) {
            return Err(Error::AccessDenied);
        }
        if target.is_empty() || target.len() > TARGET_MAX || target.contains('\0') {
            return Err(Error::Invalid);
        }
        self.free_folded(&self.open, name).await?;
        let target = alloc::ffi::CString::new(target).map_err(|_| Error::Invalid)?;
        let node = self
            .open
            .node
            .create(name.as_bytes(), Kind::Symlink(target.as_c_str()), 0o777)
            .map_err(error)?;
        self.own(&node)?;
        self.fs
            .watches
            .notify(self.open.key, &[event(WatchKind::Created, 0, name)]);
        Ok(())
    }
}

/// Checks a name that `Unlink`, `Rename`, `Link` or `CreateSymlink`
/// gets.
fn check_component(name: &str) -> Result<(), Error> {
    match name {
        "" | "." | ".." => Err(Error::Invalid),
        n if n.contains('/') || n.contains('\0') => Err(Error::Invalid),
        n if n.len() > NAME_MAX => Err(Error::NameTooLong),
        _ => Ok(()),
    }
}

impl Fs {
    /// A new token that names the directory `dir`.
    pub fn token(&self, dir: Rc<Open>) -> Result<OwnedHandle, Error> {
        let mut raw = 0;
        // SAFETY: the out pointer is valid for the call.
        if unsafe { sys::event_create(0, &mut raw) } != mikros_rt::OK {
            return Err(Error::Io);
        }
        // SAFETY: the kernel just created the handle for us.
        let event = unsafe { OwnedHandle::from_raw(raw) };
        let (koid, _) = koids(&event).ok_or(Error::Io)?;
        let mut token = 0;
        // SAFETY: the out pointer is valid for the call.
        if unsafe { sys::handle_duplicate(event.raw(), TOKEN_RIGHTS, &mut token) } != mikros_rt::OK
        {
            return Err(Error::Io);
        }
        // The server keeps the event, so that its object ID stays its own.
        core::mem::forget(event);
        self.tokens.borrow_mut().insert(koid, dir);
        // SAFETY: the kernel just created the handle for us.
        Ok(unsafe { OwnedHandle::from_raw(token) })
    }

    /// The directory that the token `handle` names, when it is one of
    /// this file system's.
    pub fn token_dir(&self, handle: &OwnedHandle) -> Option<Rc<Open>> {
        let (koid, _) = koids(handle)?;
        self.tokens.borrow().get(&koid).cloned()
    }
}

/// The object ID of `handle`, and that of the object related to it, such
/// as the other end of a channel.
pub(crate) fn koids(handle: &OwnedHandle) -> Option<(u64, u64)> {
    let mut record = [0u8; 24];
    let mut len = 0;
    // SAFETY: the pointers describe `record` and `len`.
    let status = unsafe {
        sys::object_get_info(
            handle.raw(),
            INFO_BASIC,
            record.as_mut_ptr(),
            record.len() as u64,
            &mut len,
        )
    };
    let id = u64::from_le_bytes(record[..8].try_into().ok()?);
    let related = u64::from_le_bytes(record[8..16].try_into().ok()?);
    (status == mikros_rt::OK).then_some((id, related))
}
