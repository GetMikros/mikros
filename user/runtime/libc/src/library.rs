//! Where the dynamic linker looks for libraries (MIKSPEC-0101.R16,
//! MIKSPEC-0035.R21).
//!
//! A library is named by its file name, a bare name, or as
//! `<id>:<path>`, the ID of a library package and the library's path in
//! it without `.dlib`, which is the file `<id>/<path>.dlib` of a folder.
//! Either is looked for first in the `DT_RUNPATH` entries of the objects
//! that need it, of which only those that start with `$ORIGIN` and name a
//! folder within the program's package are kept, `$ORIGIN` being the
//! folder of the object, and for the main program that of its path in
//! the startup transfer. Then it is looked for in the package's
//! `libraries/`, and last a bare name in `/system/libraries` and an
//! `<id>:<path>` name in `/libraries`.

use crate::fs;
use crate::lock::Lock;
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use core::ffi::{CStr, c_char, c_int};

/// The folder of the system's libraries, for bare names, and that of
/// library packages, for `<id>:<path>` names.
const SYSTEM: &str = "/system/libraries";
const PACKAGES: &str = "/libraries";
/// The folder of a package that holds its own libraries.
const OWN: &[u8] = b"libraries";
/// What `__mikros_library_path` is asked for.
const BARE: c_int = 0;

/// The normalized paths of the program's package, when the program is
/// in one, and of the program.
static BUNDLE: Lock<Option<String>> = Lock::new(None);
static PROGRAM: Lock<Option<String>> = Lock::new(None);

/// Keeps the package and the program of the startup transfer.
pub fn install(bundle: Option<&str>, program: &str) {
    *BUNDLE.lock() = bundle.and_then(|b| fs::normalize("/", b.as_bytes()));
    *PROGRAM.lock() = fs::normalize("/", program.as_bytes());
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

/// The folder that `entry` of the runpath of the object in the folder
/// `origin` names, if it starts with `$ORIGIN` and lies within `bundle`.
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

/// The folders of `runpath`, the runpath of the object at `origin`, or of
/// the main program for `None`.
fn runpath(runpath: &str, origin: Option<&str>) -> String {
    let bundle = BUNDLE.lock();
    let Some(bundle) = bundle.as_deref() else {
        return String::new();
    };
    let program = PROGRAM.lock();
    let Some(origin) = origin.or(program.as_deref()) else {
        return String::new();
    };
    // The folder of the object, as an absolute path.
    let Some(object) = fs::normalize(&fs::cwd(), origin.as_bytes()) else {
        return String::new();
    };
    let folder = match object.rfind('/') {
        Some(0) | None => "/",
        Some(at) => &object[..at],
    };
    let dirs: Vec<String> = runpath
        .split(':')
        .filter_map(|e| runpath_entry(e, folder, bundle))
        .collect();
    dirs.join(":")
}

/// The file `<id>/<path>.dlib` that the name `<id>:<path>` stands for,
/// or `None` for a name that is not of that form or would leave the
/// folder it is looked for in.
fn package_file(name: &str) -> Option<String> {
    let (id, path) = name.split_once(':')?;
    let part = |p: &str| !p.is_empty() && p != "." && p != ".." && !p.contains(':');
    let valid = part(id) && !id.contains('/') && path.split('/').all(part);
    valid.then(|| format!("{id}/{path}.dlib"))
}

/// Writes the folders of `runpath`, the `DT_RUNPATH` of the object
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

/// Writes the folders searched after the runpaths, for a bare name when
/// `kind` is 0 and for an `<id>:<path>` name otherwise, to the `len`
/// bytes at `buf`, as `__mikros_runpath` does.
#[unsafe(no_mangle)]
extern "C" fn __mikros_library_path(kind: c_int, buf: *mut u8, len: usize) -> usize {
    let mut dirs = Vec::new();
    if let Some(bundle) = BUNDLE.lock().as_deref() {
        dirs.push(fs::normalize(bundle, OWN).unwrap_or_default());
    }
    dirs.push(String::from(if kind == BARE { SYSTEM } else { PACKAGES }));
    copy_out(&dirs.join(":"), buf, len)
}

/// Writes the file that the library name `name` of the form `<id>:<path>`
/// stands for to the `len` bytes at `buf`, as `__mikros_runpath` does,
/// and returns its size, or 0 for a name of another form.
///
/// # Safety
///
/// `name` must be a NUL-terminated string.
#[unsafe(no_mangle)]
unsafe extern "C" fn __mikros_library_file(name: *const c_char, buf: *mut u8, len: usize) -> usize {
    // SAFETY: the caller passes a NUL-terminated string.
    let name = unsafe { CStr::from_ptr(name) }.to_str().unwrap_or("");
    package_file(name).map_or(0, |file| copy_out(&file, buf, len))
}
