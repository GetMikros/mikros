//! Serving the boot image as a read-only file system over the `Node`
//! protocol, one executor task per connection.

mod connection;

use alloc::rc::Rc;
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
