//! The descriptors of the process. Only descriptors 0, 1 and 2 exist
//! yet, each a stream from the startup transfer or closed.

use alloc::vec::Vec;
use core::sync::atomic::{AtomicU32, Ordering};
use mikros_rt::interfaces::mikros::process::{HandleRole, StartupHandle};
use mikros_rt::sys::Handle;

/// The descriptor kind of a stream.
const KIND_STREAM: u32 = 2;
/// The object type of a stream.
const TYPE_STREAM: u32 = 15;

/// The stream handle of descriptors 0 to 2, or 0 while one is closed.
static STANDARD: [AtomicU32; 3] = [AtomicU32::new(0), AtomicU32::new(0), AtomicU32::new(0)];

/// The stream that descriptor `fd` writes to and reads from.
pub fn stream(fd: i64) -> Option<Handle> {
    let slot = STANDARD.get(usize::try_from(fd).ok()?)?;
    let handle = slot.load(Ordering::Relaxed);
    (handle != 0).then_some(handle)
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
        let slot = &STANDARD[fd as usize];
        if handle.id == id && is_stream && slot.load(Ordering::Relaxed) == 0 {
            slot.store(handle.handle.into_raw(), Ordering::Relaxed);
        }
    }
}
