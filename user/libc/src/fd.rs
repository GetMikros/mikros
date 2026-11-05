//! The descriptor table of the process. Each descriptor refers to an open
//! description, which `dup` shares between descriptors. Only streams
//! exist yet.

use crate::errno::{EBADF, EINVAL, EMFILE};
use crate::lock::Lock;
use alloc::sync::Arc;
use alloc::vec::Vec;
use mikros_rt::interfaces::__midl::OwnedHandle;
use mikros_rt::interfaces::mikros::process::{HandleRole, StartupHandle};

/// The descriptor kind of a stream.
const KIND_STREAM: u32 = 2;
/// The object type of a stream.
const TYPE_STREAM: u32 = 15;
/// The most descriptors a process has open.
pub const FDS_MAX: usize = 1024;

/// What a descriptor refers to.
pub enum Description {
    Stream(OwnedHandle),
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

/// Takes the standard handles out of `handles` and installs each that
/// is a stream with the `id` of its descriptor. Any other is closed
/// with the handle it holds, and its descriptor stays closed.
pub fn install(handles: &mut Vec<StartupHandle>) {
    let mut i = 0;
    while i < handles.len() {
        let fd = match handles[i].role {
            HandleRole::Stdin => 0,
            HandleRole::Stdout => 1,
            HandleRole::Stderr => 2,
            _ => {
                i += 1;
                continue;
            }
        };
        let handle = handles.remove(i);
        let id = fd | KIND_STREAM << 28;
        let is_stream = mikros_rt::midl::handle_info(handle.handle.raw())
            .is_some_and(|(kind, _)| kind == TYPE_STREAM);
        if handle.id == id && is_stream && get(i64::from(fd)).is_none() {
            let description = Arc::new(Description::Stream(handle.handle));
            duplicate_into(i64::from(fd), description);
        }
    }
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
