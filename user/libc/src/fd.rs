//! The descriptor table of the process. Each descriptor refers to an open
//! description: a stream, a connection to a file or directory, or an
//! epoll instance, which `dup` shares between descriptors.

use crate::errno::{EBADF, EINVAL, EMFILE};
use crate::lock::Lock;
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::sync::atomic::AtomicU32;
use mikros_rt::interfaces::__midl::{ClientEnd, OwnedHandle};
use mikros_rt::interfaces::mikros::fs::{NodeClient, NodeType};
use mikros_rt::interfaces::mikros::process::{HandleRole, StartupHandle};

/// The descriptor kind of a file or directory.
pub const KIND_FILE: u32 = 1;
/// The descriptor kind of a stream.
pub const KIND_STREAM: u32 = 2;
/// The bits of a startup handle's `id` that hold its descriptor number.
pub const ID_FD: u32 = (1 << 24) - 1;
/// The object type of a channel.
const TYPE_CHANNEL: u32 = 4;
/// The object type of a stream.
const TYPE_STREAM: u32 = 15;
/// The rights argument that keeps a handle's rights.
const SAME_RIGHTS: u32 = 1 << 31;
/// The most descriptors a process has open.
pub const FDS_MAX: usize = 1024;

/// What a descriptor refers to.
pub enum Description {
    Stream(OwnedHandle),
    /// A connection to a file, whose cursor is the descriptor's offset.
    File(Node),
    /// A connection to a directory, with the state of its listing.
    Directory(Node, Lock<crate::dir::Listing>),
    /// An epoll instance, which stays in the process.
    Epoll(crate::epoll::Epoll),
}

/// A connection to a file system node, with the status flags it was
/// opened with.
pub struct Node {
    /// The connection, which carries one call at a time.
    pub node: Lock<NodeClient>,
    /// The absolute path it was opened with, which paths relative to the
    /// descriptor are resolved against.
    pub path: alloc::string::String,
    /// The device number of the namespace entry it was opened through.
    pub device: u64,
    /// The `O_` status flags, such as `O_APPEND`.
    pub flags: AtomicU32,
}

struct Entry {
    description: Arc<Description>,
    close_on_exec: bool,
}

/// The descriptors, by number.
static TABLE: Lock<Vec<Option<Entry>>> = Lock::new(Vec::new());

/// The open description of descriptor `fd`.
pub fn get(fd: i64) -> Option<Arc<Description>> {
    let table = TABLE.lock();
    let entry = table.get(usize::try_from(fd).ok()?)?.as_ref()?;
    Some(entry.description.clone())
}

/// Installs `description` as the lowest free descriptor from `from` on,
/// and returns it.
pub fn insert(description: Arc<Description>, close_on_exec: bool, from: usize) -> i64 {
    let mut table = TABLE.lock();
    let mut fd = from;
    while table.get(fd).is_some_and(Option::is_some) {
        fd += 1;
    }
    if fd >= FDS_MAX {
        return -EMFILE;
    }
    if table.len() <= fd {
        table.resize_with(fd + 1, || None);
    }
    table[fd] = Some(Entry {
        description,
        close_on_exec,
    });
    fd as i64
}

/// Closes descriptor `fd`. The description closes with its last
/// descriptor.
pub fn close(fd: i64) -> i64 {
    let removed = {
        let mut table = TABLE.lock();
        usize::try_from(fd)
            .ok()
            .and_then(|fd| table.get_mut(fd))
            .and_then(Option::take)
    };
    match removed {
        Some(_) => 0,
        None => -EBADF,
    }
}

/// Makes `to` a descriptor for the description of `from`, closing what
/// `to` was first.
pub fn duplicate_to(from: i64, to: i64, close_on_exec: bool) -> i64 {
    let Some(description) = get(from) else {
        return -EBADF;
    };
    let Some(slot) = usize::try_from(to).ok().filter(|&to| to < FDS_MAX) else {
        return -EBADF;
    };
    let old = {
        let mut table = TABLE.lock();
        if table.len() <= slot {
            table.resize_with(slot + 1, || None);
        }
        table[slot].replace(Entry {
            description,
            close_on_exec,
        })
    };
    drop(old);
    to
}

/// Whether descriptor `fd` closes on `exec`.
pub fn close_on_exec(fd: i64) -> Option<bool> {
    let table = TABLE.lock();
    Some(
        table
            .get(usize::try_from(fd).ok()?)?
            .as_ref()?
            .close_on_exec,
    )
}

pub fn set_close_on_exec(fd: i64, value: bool) -> i64 {
    let mut table = TABLE.lock();
    let entry = usize::try_from(fd)
        .ok()
        .and_then(|fd| table.get_mut(fd))
        .and_then(Option::as_mut);
    match entry {
        Some(entry) => {
            entry.close_on_exec = value;
            0
        }
        None => -EBADF,
    }
}

/// Takes the descriptors out of `handles`: the standard handles, and
/// the `USER` handles whose `id` names a descriptor. Each is installed
/// under the descriptor number and with the kind its `id` gives, when the
/// handle has that kind's object type; any other is closed, and its
/// descriptor stays closed. Other `USER` handles stay in `handles`.
pub fn install(handles: &mut Vec<StartupHandle>) {
    let mut i = 0;
    while i < handles.len() {
        let id = handles[i].id;
        let (fd, kind) = ((id & ID_FD) as usize, id >> 28);
        let standard = match handles[i].role {
            HandleRole::Stdin => Some(0),
            HandleRole::Stdout => Some(1),
            HandleRole::Stderr => Some(2),
            HandleRole::User if matches!(kind, KIND_FILE | KIND_STREAM) => None,
            _ => {
                i += 1;
                continue;
            }
        };
        let handle = handles.remove(i).handle;
        // Each descriptor has one handle, so `k` is 0.
        let valid = standard.is_none_or(|s| s == fd) && id >> 24 & 0xF == 0;
        let free = fd < FDS_MAX && get(fd as i64).is_none();
        if let (true, true, Some(description)) = (valid, free, inherited(handle, kind)) {
            duplicate_into(fd as i64, Arc::new(description));
        }
    }
}

/// The description of a handle with descriptor kind `kind`, or none,
/// closing the handle, when its object type is not that of the kind.
fn inherited(handle: OwnedHandle, kind: u32) -> Option<Description> {
    let object = mikros_rt::midl::handle_info(handle.raw()).map(|(object, _)| object);
    match (kind, object) {
        (KIND_STREAM, Some(TYPE_STREAM)) => Some(Description::Stream(handle)),
        (KIND_FILE, Some(TYPE_CHANNEL)) => {
            let node = NodeClient::new(ClientEnd::new(handle));
            let directory = matches!(
                node.get_attributes(i64::MAX),
                Ok(Ok(r)) if r.attributes.r#type == NodeType::Directory
            );
            // The path it was opened with is not passed, so paths are
            // not resolved relative to it.
            let node = Node {
                node: Lock::new(node),
                path: alloc::string::String::new(),
                device: 0,
                flags: AtomicU32::new(0),
            };
            Some(if directory {
                Description::Directory(node, Lock::new(crate::dir::Listing::default()))
            } else {
                Description::File(node)
            })
        }
        _ => None,
    }
}

/// The open descriptors, each with its description and whether it closes
/// on `exec`.
pub fn snapshot() -> Vec<(usize, Arc<Description>, bool)> {
    let table = TABLE.lock();
    table
        .iter()
        .enumerate()
        .filter_map(|(fd, e)| {
            let e = e.as_ref()?;
            Some((fd, e.description.clone(), e.close_on_exec))
        })
        .collect()
}

/// The handle of `description` that another process is given a
/// duplicate of, and its descriptor kind.
pub fn shared_handle(description: &Description) -> Option<(mikros_rt::sys::Handle, u32)> {
    match description {
        Description::Stream(stream) => Some((stream.raw(), KIND_STREAM)),
        Description::File(node) | Description::Directory(node, _) => {
            Some((node.node.lock().channel().ok()?, KIND_FILE))
        }
        Description::Epoll(_) => None,
    }
}

/// Whether `description` may be passed to another process, which an
/// epoll instance is not.
pub fn passed(description: &Description) -> bool {
    !matches!(description, Description::Epoll(_))
}

/// `mikros_fd_handle`: a new handle, which the caller owns, with the
/// rights of the descriptor's own, to the object behind descriptor `fd`:
/// the stream of a pipe, or the connection to a file or directory. Fails
/// with the negated error number.
#[unsafe(no_mangle)]
extern "C" fn mikros_fd_handle(fd: i32) -> i64 {
    let Some((handle, _)) = get(fd.into()).as_deref().and_then(shared_handle) else {
        return -EBADF;
    };
    let mut out = 0;
    // SAFETY: the out pointer is valid for the call.
    let status = unsafe { mikros_rt::sys::handle_duplicate(handle, SAME_RIGHTS, &mut out) };
    if status != mikros_rt::OK {
        return -crate::errno::from_status(status);
    }
    i64::from(out)
}

/// Installs `description` as descriptor `fd`, which is free.
fn duplicate_into(fd: i64, description: Arc<Description>) {
    let mut table = TABLE.lock();
    let slot = fd as usize;
    if table.len() <= slot {
        table.resize_with(slot + 1, || None);
    }
    table[slot] = Some(Entry {
        description,
        close_on_exec: false,
    });
}

/// The lowest descriptor of `F_DUPFD` at or above `from`.
pub fn duplicate_from(fd: i64, from: i64, close_on_exec: bool) -> i64 {
    let Some(from) = usize::try_from(from).ok().filter(|&f| f < FDS_MAX) else {
        return -EINVAL;
    };
    match get(fd) {
        Some(description) => insert(description, close_on_exec, from),
        None => -EBADF,
    }
}

/// `pipe2`: a new stream, with descriptors for reading from one end and
/// writing to the other at `fds`. Only `O_CLOEXEC` is taken in `flags`.
pub fn pipe(fds: *mut [i32; 2], flags: u32) -> i64 {
    if flags & !crate::fs::O_CLOEXEC != 0 {
        return -EINVAL;
    }
    let (mut a, mut b) = (0, 0);
    // SAFETY: the out pointers are valid for the call.
    let status = unsafe { mikros_rt::sys::stream_create(0, &mut a, &mut b) };
    if status != mikros_rt::OK {
        return -crate::errno::from_status(status);
    }
    // SAFETY: the kernel just created the handles for us.
    let (a, b) = unsafe { (OwnedHandle::from_raw(a), OwnedHandle::from_raw(b)) };
    let cloexec = flags & crate::fs::O_CLOEXEC != 0;
    let read = insert(Arc::new(Description::Stream(a)), cloexec, 0);
    if read < 0 {
        return read;
    }
    let write = insert(Arc::new(Description::Stream(b)), cloexec, 0);
    if write < 0 {
        close(read);
        return write;
    }
    // SAFETY: musl passes room for two descriptors.
    unsafe { fds.write([read as i32, write as i32]) };
    0
}
