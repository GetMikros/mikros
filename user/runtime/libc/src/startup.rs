//! The startup handles that the C library did not install as
//! descriptors, which the program takes, and the process's `Directory`
//! connection and own handle, which the C library keeps.

use crate::lock::Lock;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicU32, Ordering};
use mikros_rt::interfaces::mikros::process::StartupHandle;
use mikros_rt::sys;

/// What `mikros_take_startup_handle` returns for a handle it has not.
const HANDLE_INVALID: sys::Handle = 0;

static HANDLES: Lock<Vec<StartupHandle>> = Lock::new(Vec::new());
static DIRECTORY: AtomicU32 = AtomicU32::new(HANDLE_INVALID);

/// Keeps `handles`, the startup handles left, for the program, and
/// `directory`, the process's directory connection, which stays open.
pub fn install(handles: Vec<StartupHandle>, directory: sys::Handle) {
    *HANDLES.lock() = handles;
    DIRECTORY.store(directory, Ordering::Relaxed);
}

/// The process's `Directory` connection.
pub fn directory() -> sys::Handle {
    DIRECTORY.load(Ordering::Relaxed)
}

/// `mikros_take_startup_handle`: the startup handle with `role` and `id`
/// that is left, which the caller then owns, once; `HANDLE_INVALID`
/// after that and for one the process did not get.
#[unsafe(no_mangle)]
extern "C" fn mikros_take_startup_handle(role: u32, id: u32) -> sys::Handle {
    let mut handles = HANDLES.lock();
    let Some(at) = handles
        .iter()
        .position(|h| h.role.into_raw() == role && h.id == id)
    else {
        return HANDLE_INVALID;
    };
    handles.remove(at).handle.into_raw()
}

/// `mikros_directory`: the process's `Directory` connection, which the
/// C library keeps owning.
#[unsafe(no_mangle)]
extern "C" fn mikros_directory() -> sys::Handle {
    directory()
}

/// `mikros_process_self`: the process's own handle from its startup
/// transfer, which the C library keeps owning.
#[unsafe(no_mangle)]
extern "C" fn mikros_process_self() -> sys::Handle {
    crate::syscall::process()
}
