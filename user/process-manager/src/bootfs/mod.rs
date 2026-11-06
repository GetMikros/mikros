//! Serving the boot image as a read-only file system over the `Node`
//! protocol, one executor task per connection.

mod connection;
mod memory;

use alloc::collections::BTreeMap;
use alloc::rc::Rc;
use core::cell::RefCell;
use mikros_bootfs::Tree;
use mikros_rt::exec::{Reactor, Spawner};
use mikros_rt::interfaces::__midl::{OwnedHandle, ServerEnd};
use mikros_rt::interfaces::mikros::fs::{Node, Rights};
use mikros_rt::sys::{self, Handle, Status};

const PAGE_SIZE: u64 = 4096;
const PERM_READ: u32 = 1 << 0;
/// The `object_get_info` topic of memory objects, and its record size.
const INFO_MEMORY: u32 = 4;
const INFO_MEMORY_SIZE: usize = 24;

/// The boot file system.
pub struct Fs {
    tree: Tree,
    image: &'static [u8],
    reactor: Reactor,
    spawner: Spawner,
    pager: OwnedHandle,
    /// The paged memory objects of files, by node.
    paged: RefCell<BTreeMap<usize, OwnedHandle>>,
}

impl Fs {
    /// The file system of the boot image `image`, which stays mapped
    /// into `process` for good. Its connections run on the executor of
    /// `reactor` and `spawner`.
    pub fn new(
        process: Handle,
        image: &OwnedHandle,
        reactor: Reactor,
        spawner: Spawner,
    ) -> Result<Rc<Fs>, Status> {
        let image = map(process, image)?;
        let mut pager = 0;
        // SAFETY: the out pointer is valid for the call.
        let status = unsafe { sys::pager_create(0, &mut pager) };
        if status != mikros_rt::OK {
            return Err(status);
        }
        // SAFETY: the kernel just created the handle for us.
        let pager = unsafe { OwnedHandle::from_raw(pager) };
        let tree = Tree::new(image);
        for (path, flag) in &tree.skipped {
            mikros_rt::log!(
                Warning,
                "the boot file system leaves out {} (type {:?})",
                alloc::string::String::from_utf8_lossy(path),
                char::from(*flag)
            );
        }
        Ok(Rc::new(Fs {
            tree,
            image,
            reactor,
            spawner,
            pager,
            paged: RefCell::new(BTreeMap::new()),
        }))
    }

    /// Serves a connection to the root directory with `rights` on `end`.
    pub fn serve_root(self: &Rc<Self>, end: ServerEnd<Node>, rights: Rights) {
        self.serve(end, 0, rights);
    }

    fn serve(self: &Rc<Self>, end: ServerEnd<Node>, node: usize, rights: Rights) {
        let fs = self.clone();
        self.spawner
            .spawn(async move { connection::serve(fs, end, node, rights).await });
    }
}

/// An executable file of the boot file system, for the loader.
pub struct Executable {
    /// The file's bytes.
    pub file: &'static [u8],
    /// The file's paged memory object, readable and executable.
    pub shared: OwnedHandle,
    /// Whether the file lies under `/usr` in the boot image, which makes
    /// it a system component, whatever path named it.
    pub system: bool,
    /// Where the file starts in the image.
    offset: usize,
}

impl Fs {
    /// Whether `path`, relative to the root, names a node.
    pub fn exists(&self, path: &str) -> bool {
        self.tree.resolve(0, path.as_bytes(), false).is_ok()
    }

    /// The regular file at `path`, relative to the root and resolved
    /// without leaving the file system. Fails with `NOT_FOUND` when
    /// there is no node, and `ACCESS_DENIED` when it is not a regular
    /// file.
    pub fn executable(self: &Rc<Self>, path: &str) -> Result<Executable, Status> {
        const NOT_FOUND: Status = -25;
        const ACCESS_DENIED: Status = -13;
        let node = match self.tree.resolve(0, path.as_bytes(), false) {
            Ok(mikros_bootfs::Resolved::Node(node)) => node,
            _ => return Err(NOT_FOUND),
        };
        let mikros_bootfs::NodeKind::File { offset, size } = self.tree.nodes[node].kind else {
            return Err(ACCESS_DENIED);
        };
        let rights = memory::rights(false, true);
        let usr = self.tree.resolve(0, b"usr", true);
        let system =
            matches!(usr, Ok(mikros_bootfs::Resolved::Node(usr)) if self.tree.contains(usr, node));
        Ok(Executable {
            file: &self.image[offset..offset + size],
            shared: self.paged_memory(node, offset, size, rights)?,
            system,
            offset,
        })
    }

    /// A new memory object of `len` bytes, a page multiple, private to
    /// its holder, with the bytes of `executable` from `from` and zeros
    /// after the end of the file.
    pub fn private_range(
        &self,
        executable: &Executable,
        from: u64,
        len: u64,
    ) -> Result<OwnedHandle, Status> {
        let size = executable.file.len();
        let start = usize::try_from(from).map_or(size, |f| f.min(size));
        let count = usize::try_from(len).map_or(size - start, |l| l.min(size - start));
        memory::copy(self, executable.offset + start, count, len)
    }
}

/// Maps the memory object `image` read-only into `process` and returns
/// its bytes.
fn map(process: Handle, image: &OwnedHandle) -> Result<&'static [u8], Status> {
    let mut record = [0u8; INFO_MEMORY_SIZE];
    let mut len = 0;
    // SAFETY: the pointers describe `record` and `len`.
    let status = unsafe {
        sys::object_get_info(
            image.raw(),
            INFO_MEMORY,
            record.as_mut_ptr(),
            record.len() as u64,
            &mut len,
        )
    };
    if status != mikros_rt::OK {
        return Err(status);
    }
    let size = u64::from_le_bytes(record[..8].try_into().expect("8 bytes"));
    let mapped = size.next_multiple_of(PAGE_SIZE);
    let mut at = 0;
    // SAFETY: the out pointer is valid; the kernel picks a free range.
    let status = unsafe { sys::vm_map(process, PERM_READ, 0, image.raw(), 0, mapped, &mut at) };
    if status != mikros_rt::OK {
        return Err(status);
    }
    // SAFETY: the image is mapped readable at `at` for `size` bytes, and
    // is never unmapped.
    Ok(unsafe { core::slice::from_raw_parts(at as *const u8, size as usize) })
}
