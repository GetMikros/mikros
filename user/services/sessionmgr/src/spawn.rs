//! Starting programs through the launcher: their argument and
//! environment block, and a terminal as their standard input, output
//! and error.

use alloc::string::String;
use alloc::vec::Vec;
use mikros_rt::interfaces::__midl::OwnedHandle;
use mikros_rt::interfaces::mikros::process::{HandleRole, StartupHandle};
use mikros_rt::sys;

/// The descriptor kind of a slave terminal, in the top bits of a
/// standard handle's `id`.
const TERMINAL_KIND: u32 = 8;
const SAME_RIGHTS: u32 = 1 << 31;

/// A memory object of `arguments` and `environment`: their counts, and
/// then each of them ended by a zero byte, the arguments first.
pub fn block(arguments: &[&str], environment: &[String]) -> Option<OwnedHandle> {
    let mut bytes = Vec::new();
    bytes.extend_from_slice(&(arguments.len() as u32).to_le_bytes());
    bytes.extend_from_slice(&(environment.len() as u32).to_le_bytes());
    for a in arguments
        .iter()
        .copied()
        .chain(environment.iter().map(String::as_str))
    {
        bytes.extend_from_slice(a.as_bytes());
        bytes.push(0);
    }
    let mut mem = 0;
    let size = (bytes.len() as u64).next_multiple_of(4096);
    // SAFETY: the out pointer is valid for the call.
    if unsafe { sys::mem_create(size, 0, &mut mem) } != mikros_rt::OK {
        return None;
    }
    // SAFETY: the kernel just created the handle for us.
    let mem = unsafe { OwnedHandle::from_raw(mem) };
    // SAFETY: the pointer and length describe `bytes`.
    let written = unsafe { sys::mem_write(mem.raw(), 0, bytes.as_ptr(), bytes.len() as u64) };
    (written == mikros_rt::OK).then_some(mem)
}

/// A copy of `handle` with the same rights.
pub fn duplicate(handle: &OwnedHandle) -> Option<OwnedHandle> {
    let mut out = 0;
    // SAFETY: the out pointer is valid for the call.
    let status = unsafe { sys::handle_duplicate(handle.raw(), SAME_RIGHTS, &mut out) };
    // SAFETY: the kernel just created the handle for us.
    (status == mikros_rt::OK).then(|| unsafe { OwnedHandle::from_raw(out) })
}

/// The terminal of `token` as descriptors 0, 1 and 2.
pub fn standard(token: &OwnedHandle) -> Option<Vec<StartupHandle>> {
    [HandleRole::Stdin, HandleRole::Stdout, HandleRole::Stderr]
        .into_iter()
        .enumerate()
        .map(|(fd, role)| {
            duplicate(token).map(|handle| StartupHandle {
                role,
                id: TERMINAL_KIND << 28 | fd as u32,
                handle,
            })
        })
        .collect()
}
