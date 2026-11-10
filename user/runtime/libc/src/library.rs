//! The directories that the dynamic linker searches for libraries.
//!
//! A library is looked for in the `DT_RUNPATH` entries of the objects
//! that need it, then in the bundle's `lib/`, `/usr/lib` and
//! `/Libraries/lib`. Only the `DT_RUNPATH` entries that start with
//! `$ORIGIN` and name a directory within the bundle are kept. The main
//! program's path is not known, so its entries are not.

use crate::fs;
use crate::lock::Lock;
use alloc::string::String;
use alloc::vec::Vec;
use core::ffi::{CStr, c_char};

/// The system directories, after the bundle's.
const SYSTEM: [&str; 2] = ["/usr/lib", "/Libraries/lib"];

/// The normalized path of the app's bundle, when the program is in one.
static BUNDLE: Lock<Option<String>> = Lock::new(None);

/// Keeps the bundle of the startup transfer.
pub fn install(bundle: Option<&str>) {
    *BUNDLE.lock() = bundle.and_then(|b| fs::normalize("/", b.as_bytes()));
}

/// Copies `path` with a NUL to the `len` bytes at `buf` when it fits, and
/// returns its size with the NUL.
fn copy_out(path: &str, buf: *mut u8, len: usize) -> usize {
    let size = path.len() + 1;
    if !buf.is_null() && size <= len {
        // SAFETY: the caller passes `len` bytes at `buf`, and the path
        // with its NUL fits in them.
        unsafe {
            core::ptr::copy_nonoverlapping(path.as_ptr(), buf, path.len());
            buf.add(path.len()).write(0);
        }
    }
    size
}

/// The directory that `entry` of the runpath of the object at `origin`
/// names, if it starts with `$ORIGIN` and lies within `bundle`.
fn runpath_entry(entry: &str, origin: &str, bundle: &str) -> Option<String> {
    let rest = entry
        .strip_prefix("${ORIGIN}")
        .or_else(|| entry.strip_prefix("$ORIGIN"))?;
    if !(rest.is_empty() || rest.starts_with('/')) || rest.contains('$') {
        return None;
    }
    let dir = fs::normalize(origin, rest.trim_start_matches('/').as_bytes())?;
    let within = dir == bundle || dir.strip_prefix(bundle).is_some_and(|r| r.starts_with('/'));
    within.then_some(dir)
}

/// The directories of the runpath of an object.
fn runpath(runpath: &str, origin: Option<&str>) -> String {
    let bundle = BUNDLE.lock();
    let (Some(bundle), Some(origin)) = (bundle.as_deref(), origin) else {
        return String::new();
    };
    // The directory of the object, as an absolute path.
    let Some(object) = fs::normalize(&fs::cwd(), origin.as_bytes()) else {
        return String::new();
    };
    let directory = match object.rfind('/') {
        Some(0) | None => "/",
        Some(at) => &object[..at],
    };
    let dirs: Vec<String> = runpath
        .split(':')
        .filter_map(|e| runpath_entry(e, directory, bundle))
        .collect();
    dirs.join(":")
}

/// Writes the directories of `runpath`, the `DT_RUNPATH` of the object
/// loaded from `origin`, or of the main program for a null `origin`, to
/// the `len` bytes at `buf`, as a NUL-terminated list separated by `:`.
/// Returns the size of the list, which is written only when it fits.
///
/// # Safety
///
/// `runpath` and a nonnull `origin` must be NUL-terminated strings.
#[unsafe(no_mangle)]
unsafe extern "C" fn __mikros_runpath(
    path: *const c_char,
    origin: *const c_char,
    buf: *mut u8,
    len: usize,
) -> usize {
    // SAFETY: the caller passes NUL-terminated strings.
    let (path, origin) = unsafe {
        (
            CStr::from_ptr(path).to_str().unwrap_or(""),
            (!origin.is_null()).then(|| CStr::from_ptr(origin).to_str().unwrap_or("")),
        )
    };
    copy_out(&runpath(path, origin), buf, len)
}

/// Writes the directories searched after the runpaths to the `len` bytes
/// at `buf`, as `__mikros_runpath` does.
#[unsafe(no_mangle)]
extern "C" fn __mikros_library_path(buf: *mut u8, len: usize) -> usize {
    let mut dirs = Vec::new();
    if let Some(bundle) = BUNDLE.lock().as_deref() {
        dirs.push(fs::normalize(bundle, b"lib").unwrap_or_default());
    }
    dirs.extend(SYSTEM.iter().map(|d| String::from(*d)));
    copy_out(&dirs.join(":"), buf, len)
}
