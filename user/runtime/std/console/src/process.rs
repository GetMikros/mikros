//! The console's own process, which mapping memory objects names.

use mikros_rt::sys;

/// The console's own process handle, which the C library keeps open.
pub fn current() -> sys::Handle {
    std::os::mikros::process::current().as_raw_handle()
}
