//! Starting programs through the launcher: their argument and
//! environment block, and a terminal as their standard input, output
//! and error.

use alloc::string::String;
use alloc::vec::Vec;
use mikros_rt::interfaces::__midl::OwnedHandle;
use mikros_rt::interfaces::mikros::process::{HandleRole, StartupHandle};
use mikros_rt::objects;

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
    let size = (bytes.len() as u64).next_multiple_of(4096);
    let mem = objects::memory(size, 0).ok()?;
    objects::write_memory(mem.raw(), 0, &bytes).ok()?;
    Some(mem)
}

/// A copy of `handle` with the same rights.
pub fn duplicate(handle: &OwnedHandle) -> Option<OwnedHandle> {
    objects::duplicate(handle.raw(), SAME_RIGHTS).ok()
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
