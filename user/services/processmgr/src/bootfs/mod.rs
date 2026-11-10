//! Serving the boot image as a read-only file system over the `Node`
//! protocol, one executor task per connection.

mod connection;
mod memory;
mod pager;

use alloc::rc::Rc;
use mikros_bootfs::{NodeKind, Tree};
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
    /// The pager of the files' paged objects, which a thread of its own
    /// answers for.
    pager: &'static pager::Pager,
}

impl Fs {
    /// The file system of the boot image `image`, which stays mapped
    /// into `process` for good. Its connections run on the executor of
    /// `reactor` and `spawner`, and a thread of `process` supplies the
    /// pages of its files' paged objects.
    pub fn new(
        process: Handle,
        image: &OwnedHandle,
        reactor: Reactor,
        spawner: Spawner,
    ) -> Result<Rc<Fs>, Status> {
        let image = map(process, image)?;
        let tree = Tree::new(image);
        let files = tree
            .nodes
            .iter()
            .map(|node| match node.kind {
                NodeKind::File { offset, size } => (offset, size),
                _ => (0, 0),
            })
            .collect();
        let pager = pager::Pager::start(process, image, files)?;
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

/// A node below a directory of the boot file system, as `subtree` gives
/// it.
pub struct Item {
    /// Its path relative to the directory.
    pub path: alloc::string::String,
    pub mode: u32,
    pub kind: ItemKind,
}

pub enum ItemKind {
    Directory,
    File(&'static [u8]),
    Symlink(alloc::vec::Vec<u8>),
}

impl Fs {
    /// Serves a connection with `rights` on `end` to the directory at
    /// `path`, relative to the root, the root itself when it is empty.
    /// Returns false, and serves nothing, without a directory there.
    pub fn serve_directory(
        self: &Rc<Self>,
        end: ServerEnd<Node>,
        path: &str,
        rights: Rights,
    ) -> bool {
        let node = match path.is_empty() {
            true => Some(0),
            false => self.node(path),
        };
        match node {
            Some(node)
                if matches!(
                    self.tree.nodes[node].kind,
                    mikros_bootfs::NodeKind::Directory(_)
                ) =>
            {
                self.serve(end, node, rights);
                true
            }
            _ => false,
        }
    }

    /// Everything below the directory at `path`, relative to the root,
    /// each directory before what it holds, without following links.
    /// None without a directory there.
    pub fn subtree(&self, path: &str) -> Option<alloc::vec::Vec<Item>> {
        let top = self.node(path)?;
        let mikros_bootfs::NodeKind::Directory(_) = &self.tree.nodes[top].kind else {
            return None;
        };
        let mut items = alloc::vec::Vec::new();
        let mut pending = alloc::vec![(alloc::string::String::new(), top)];
        while let Some((prefix, dir)) = pending.pop() {
            let mikros_bootfs::NodeKind::Directory(entries) = &self.tree.nodes[dir].kind else {
                continue;
            };
            for (name, &node) in entries {
                let name = alloc::string::String::from_utf8_lossy(name);
                let path = match prefix.is_empty() {
                    true => alloc::string::String::from(name),
                    false => alloc::format!("{prefix}/{name}"),
                };
                let n = &self.tree.nodes[node];
                let kind = match &n.kind {
                    mikros_bootfs::NodeKind::Directory(_) => {
                        pending.push((path.clone(), node));
                        ItemKind::Directory
                    }
                    mikros_bootfs::NodeKind::File { offset, size } => {
                        ItemKind::File(&self.image[*offset..offset + size])
                    }
                    mikros_bootfs::NodeKind::Symlink(target) => ItemKind::Symlink(target.clone()),
                };
                items.push(Item {
                    path,
                    mode: n.mode,
                    kind,
                });
            }
        }
        Some(items)
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

    /// The node that `path`, relative to the root, names, following
    /// links that stay within the file system.
    fn node(&self, path: &str) -> Option<usize> {
        match self.tree.resolve(0, path.as_bytes(), false) {
            Ok(mikros_bootfs::Resolved::Node(node)) => Some(node),
            _ => None,
        }
    }

    /// The bytes of the regular file at `path`, relative to the root.
    pub fn file(&self, path: &str) -> Option<&'static [u8]> {
        match self.tree.nodes[self.node(path)?].kind {
            mikros_bootfs::NodeKind::File { offset, size } => {
                Some(&self.image[offset..offset + size])
            }
            _ => None,
        }
    }

    /// Whether the file `path` names lies in the directory `dir`, both
    /// relative to the root, reached through directories only.
    pub fn lies_in(&self, path: &str, dir: &str) -> bool {
        let dir = self.tree.resolve(0, dir.as_bytes(), true);
        match (self.node(path), dir) {
            (Some(node), Ok(mikros_bootfs::Resolved::Node(dir))) => self.tree.contains(dir, node),
            _ => false,
        }
    }

    /// Whether `a` and `b`, relative to the root, name the same file.
    pub fn same_file(&self, a: &str, b: &str) -> bool {
        self.node(a).is_some_and(|node| self.node(b) == Some(node))
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
            shared: self.paged_memory(node, size, rights)?,
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
