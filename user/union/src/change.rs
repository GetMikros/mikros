//! Changes through a view: copying directories and files up into the
//! writable layer, whiteouts, and what creates, removes and moves names.

use crate::view::{Dir, Found, OPAQUE, WHITEOUT, flat, open_in};
use alloc::format;
use alloc::rc::Rc;
use alloc::vec::Vec;
use mikros_rt::interfaces::__midl::{OwnedHandle, endpoints};
use mikros_rt::interfaces::mikros::fs::{
    AttributeMask, Error, Node, NodeAsyncClient, NodeType, OpenFlags, Rights,
};
use mikros_rt::sys;

const RW: Rights = Rights(Rights::READ.0 | Rights::WRITE.0);
/// The most bytes one read or write moves.
const CHUNK: u32 = 32768;
const INFO_BASIC: u32 = 1;
/// The rights of a token: DUPLICATE, TRANSFER and INSPECT.
const TOKEN_RIGHTS: u32 = 1 << 0 | 1 << 1 | 1 << 9;

/// Checks a name that a change gives an entry: names of whiteouts may not
/// be made through a view.
pub fn check_name(name: &str) -> Result<(), Error> {
    match name {
        "" | "." | ".." => Err(Error::Invalid),
        n if n.contains('/') || n.starts_with(WHITEOUT) => Err(Error::Invalid),
        n if n.len() > 255 => Err(Error::NameTooLong),
        _ => Ok(()),
    }
}

/// Opens `name` in `dir` of a layer with `flags` and `rights`.
async fn open_with(
    dir: &NodeAsyncClient,
    name: &str,
    flags: OpenFlags,
    mode: u32,
    rights: Rights,
    dir_of: &Dir,
) -> Result<NodeAsyncClient, Error> {
    let (client, server) = endpoints::<Node>().map_err(|_| Error::Io)?;
    flat(dir.open(name.into(), flags, mode, rights, server).await)?;
    Ok(NodeAsyncClient::new(client, dir_of.view.reactor.clone()))
}

/// The mode of the directory at `path` from the root of a layer after
/// the first, the first that has one there, or 0o755.
async fn lower_mode(dir: &Dir, path: &[alloc::string::String]) -> u32 {
    for root in dir.view.roots.iter().skip(1) {
        let mut current = root.clone();
        let mut found = true;
        for name in path {
            match open_in(&current, name, &dir.view.reactor).await {
                Ok((node, NodeType::Directory)) => current = Rc::new(node),
                _ => {
                    found = false;
                    break;
                }
            }
        }
        if found && let Ok(attrs) = flat(current.get_attributes().await) {
            return attrs.attributes.mode;
        }
    }
    0o755
}

impl Dir {
    /// This directory in the writable layer, read and write, made there
    /// with the directories above it when it is not, each with the mode
    /// of the first lower layer that has it.
    pub async fn upper(&self) -> Result<Rc<NodeAsyncClient>, Error> {
        if !self.view.writable {
            return Err(Error::ReadOnly);
        }
        let mut current = self.view.roots[0].clone();
        for (at, name) in self.path.iter().enumerate() {
            let opened = open_with(&current, name, OpenFlags::DIRECTORY, 0, RW, self).await;
            current = match opened {
                Ok(node) => Rc::new(node),
                Err(Error::NotFound) => {
                    let mode = lower_mode(self, &self.path[..=at]).await;
                    let flags = OpenFlags(
                        OpenFlags::CREATE.0 | OpenFlags::DIRECTORY.0 | OpenFlags::EXCLUSIVE.0,
                    );
                    Rc::new(open_with(&current, name, flags, mode, RW, self).await?)
                }
                Err(e) => return Err(e),
            };
        }
        Ok(current)
    }

    /// Removes the whiteout of `name` from `upper`, this directory in the
    /// writable layer, and whether there was one.
    pub async fn unwhite(&self, upper: &NodeAsyncClient, name: &str) -> Result<bool, Error> {
        match flat(upper.unlink(format!("{WHITEOUT}{name}"), false).await) {
            Ok(_) => Ok(true),
            Err(Error::NotFound) => Ok(false),
            Err(e) => Err(e),
        }
    }

    /// Makes a whiteout of `name` in `upper`, this directory in the
    /// writable layer, when a lower layer still shows the name.
    async fn white(&self, upper: &NodeAsyncClient, name: &str) -> Result<(), Error> {
        if matches!(self.lookup(name).await?, Found::Missing) {
            return Ok(());
        }
        let flags = OpenFlags(OpenFlags::CREATE.0 | OpenFlags::EXCLUSIVE.0);
        open_with(upper, &format!("{WHITEOUT}{name}"), flags, 0o600, RW, self)
            .await
            .map(|_| ())
    }

    /// Copies the file, link or socket `name` of layer `layer` up into
    /// the writable layer, with its mode and times, and returns that
    /// layer's directory.
    pub async fn copy_up(
        &self,
        name: &str,
        layer: usize,
        kind: NodeType,
    ) -> Result<Rc<NodeAsyncClient>, Error> {
        let upper = self.upper().await?;
        if layer == 0 {
            return Ok(upper);
        }
        let holder = self.layers[layer].as_ref().ok_or(Error::Io)?;
        let (source, _) = open_in(holder, name, &self.view.reactor).await?;
        let attrs = flat(source.get_attributes().await)?.attributes;
        match kind {
            NodeType::Symlink => {
                let target = flat(source.read_link().await)?.target;
                flat(upper.create_symlink(name.into(), target).await)?;
                return Ok(upper);
            }
            NodeType::File => {}
            _ => return Err(Error::NotSupported),
        }
        let flags = OpenFlags(OpenFlags::CREATE.0 | OpenFlags::EXCLUSIVE.0);
        let copy = open_with(&upper, name, flags, attrs.mode, RW, self).await?;
        let (clone_source, server) = endpoints::<Node>().map_err(|_| Error::Io)?;
        flat(source.clone(server, Rights::READ).await)?;
        match flat(copy.clone_range(clone_source, 0, 0, attrs.size).await) {
            Ok(_) => {}
            Err(Error::CrossDevice | Error::NotSupported) => {
                let mut at = 0;
                while at < attrs.size {
                    let data = flat(source.read(at, CHUNK).await)?.data;
                    if data.is_empty() {
                        break;
                    }
                    let n = data.len() as u64;
                    flat(copy.write(at, data).await)?;
                    at += n;
                }
            }
            Err(e) => return Err(e),
        }
        let mask = AttributeMask(AttributeMask::ACCESSED.0 | AttributeMask::MODIFIED.0);
        flat(
            copy.set_attributes(mask, 0, attrs.accessed, attrs.modified)
                .await,
        )?;
        Ok(upper)
    }

    /// Makes `name` a directory in the writable layer, with `mode`. One
    /// that replaces a whiteout hides the directories of its name in the
    /// layers after it.
    pub async fn make_directory(&self, name: &str, mode: u32) -> Result<(), Error> {
        let upper = self.upper().await?;
        let whited = self.unwhite(&upper, name).await?;
        let flags =
            OpenFlags(OpenFlags::CREATE.0 | OpenFlags::DIRECTORY.0 | OpenFlags::EXCLUSIVE.0);
        let made = open_with(&upper, name, flags, mode, RW, self).await?;
        if whited {
            let flags = OpenFlags(OpenFlags::CREATE.0 | OpenFlags::EXCLUSIVE.0);
            open_with(&made, OPAQUE, flags, 0o600, RW, self).await?;
        }
        Ok(())
    }

    /// Removes `name`, a directory when `directory`, which must be empty
    /// as the view shows it, and anything else otherwise, leaving a
    /// whiteout when a lower layer still shows it.
    pub async fn remove(&self, name: &str, directory: bool) -> Result<(), Error> {
        let found = self.lookup(name).await?;
        let upper = self.upper().await?;
        match found {
            Found::Missing => return Err(Error::NotFound),
            Found::Dir(_) if !directory => return Err(Error::IsDirectory),
            Found::Other { .. } if directory => return Err(Error::NotDirectory),
            Found::Dir(dir) => {
                if !dir.entries(false).await?.is_empty() {
                    return Err(Error::NotEmpty);
                }
                if dir.layers[0].is_some() {
                    // Its whiteouts and marker go with it.
                    let up = dir.upper().await?;
                    let mut cookie = 0;
                    let mut names = Vec::new();
                    loop {
                        let listed = flat(up.read_directory(cookie, false).await)?;
                        if listed.entries.is_empty() {
                            break;
                        }
                        cookie = listed.next;
                        names.extend(listed.entries.into_iter().map(|e| e.name));
                    }
                    for name in names {
                        flat(up.unlink(name, false).await)?;
                    }
                    flat(upper.unlink(name.into(), true).await)?;
                }
            }
            Found::Other { layer, .. } => {
                if layer == 0 {
                    flat(upper.unlink(name.into(), false).await)?;
                }
            }
        }
        self.white(&upper, name).await
    }

    /// A new token that names this directory.
    pub fn token(&self) -> Result<OwnedHandle, Error> {
        let mut raw = 0;
        // SAFETY: the out pointer is valid for the call.
        if unsafe { sys::event_create(0, &mut raw) } != mikros_rt::OK {
            return Err(Error::Io);
        }
        // SAFETY: the kernel just created the handle for us.
        let event = unsafe { OwnedHandle::from_raw(raw) };
        let koid = koid(&event).ok_or(Error::Io)?;
        let mut token = 0;
        // SAFETY: the out pointer is valid for the call.
        if unsafe { sys::handle_duplicate(event.raw(), TOKEN_RIGHTS, &mut token) } != mikros_rt::OK
        {
            return Err(Error::Io);
        }
        // The server keeps the event, so that its object ID stays its own.
        core::mem::forget(event);
        self.view.tokens.borrow_mut().insert(koid, self.clone());
        // SAFETY: the kernel just created the handle for us.
        Ok(unsafe { OwnedHandle::from_raw(token) })
    }

    /// The directory of this view that the token `handle` names.
    pub fn token_dir(&self, handle: &OwnedHandle) -> Option<Dir> {
        let koid = koid(handle)?;
        self.view.tokens.borrow().get(&koid).cloned()
    }

    /// Moves `name` of this directory to `new_name` in `to`, copying a
    /// file of a lower layer up first and whiting out the old name when a
    /// lower layer still shows it. A directory that a lower layer holds
    /// cannot move.
    pub async fn rename(&self, name: &str, to: &Dir, new_name: &str) -> Result<(), Error> {
        check_name(new_name)?;
        let upper = match self.lookup(name).await? {
            Found::Missing => return Err(Error::NotFound),
            Found::Dir(dir) if dir.layers.iter().skip(1).any(Option::is_some) => {
                return Err(Error::CrossDevice);
            }
            Found::Dir(_) => self.upper().await?,
            Found::Other { layer, kind } => self.copy_up(name, layer, kind).await?,
        };
        let target = to.upper().await?;
        to.unwhite(&target, new_name).await?;
        let token = flat(target.get_token().await)?.token;
        flat(upper.rename(name.into(), token, new_name.into()).await)?;
        self.white(&upper, name).await
    }

    /// Gives `name` of this directory the further name `new_name` in `to`,
    /// in the writable layer, copying it up first.
    pub async fn link(&self, name: &str, to: &Dir, new_name: &str) -> Result<(), Error> {
        check_name(new_name)?;
        let upper = match self.lookup(name).await? {
            Found::Missing => return Err(Error::NotFound),
            Found::Dir(_) => return Err(Error::IsDirectory),
            Found::Other { layer, kind } => self.copy_up(name, layer, kind).await?,
        };
        let target = to.upper().await?;
        to.unwhite(&target, new_name).await?;
        let token = flat(target.get_token().await)?.token;
        flat(upper.link(name.into(), token, new_name.into()).await).map(|_| ())
    }

    /// Whether `name` shows in this directory of the view.
    pub async fn shows(&self, name: &str) -> Result<bool, Error> {
        Ok(!matches!(self.lookup(name).await?, Found::Missing))
    }
}

/// The object ID of `handle`.
fn koid(handle: &OwnedHandle) -> Option<u64> {
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
    (status == mikros_rt::OK).then_some(id)
}
