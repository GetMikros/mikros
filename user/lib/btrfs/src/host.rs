// SPDX-License-Identifier: GPL-2.0
//! The shim's host layer on the build host, for the tests: memory from
//! mmap, threads of std, futexes, and the host's clocks.

use core::ffi::{c_int, c_long, c_void};
use std::boxed::Box;
use std::fs::File;
use std::io::{self, Write};
use std::os::fd::AsRawFd;
use std::os::unix::fs::FileExt;

unsafe extern "C" {
    fn mmap(
        addr: *mut c_void,
        len: usize,
        prot: c_int,
        flags: c_int,
        fd: c_int,
        off: i64,
    ) -> *mut c_void;
    fn syscall(number: c_long, ...) -> c_long;
    fn clock_gettime(clock: c_int, ts: *mut Timespec) -> c_int;
    fn getrandom(buf: *mut c_void, len: usize, flags: u32) -> isize;
    fn fallocate(fd: c_int, mode: c_int, offset: i64, len: i64) -> c_int;
}

#[repr(C)]
struct Timespec {
    sec: i64,
    nsec: i64,
}

const PROT_READ_WRITE: c_int = 0x1 | 0x2;
const MAP_PRIVATE_ANONYMOUS_NORESERVE: c_int = 0x02 | 0x20 | 0x4000;
const MAP_FAILED: *mut c_void = !0 as *mut c_void;
const SYS_FUTEX: c_long = 202;
const FUTEX_WAIT_BITSET_PRIVATE: c_int = 9 | 128;
const FUTEX_WAKE_PRIVATE: c_int = 1 | 128;
const FUTEX_BITSET_MATCH_ANY: u32 = !0;
const CLOCK_REALTIME: c_int = 0;
const CLOCK_MONOTONIC: c_int = 1;
const FOREVER: i64 = -1;
/// The stack of each thread: kernel code keeps frames small, but its
/// call chains are deep.
const STACK_SIZE: usize = 4 << 20;

#[unsafe(no_mangle)]
extern "C" fn mikros_host_reserve(size: usize) -> *mut c_void {
    // SAFETY: an anonymous mapping that nothing else refers to.
    let addr = unsafe {
        mmap(
            core::ptr::null_mut(),
            size,
            PROT_READ_WRITE,
            MAP_PRIVATE_ANONYMOUS_NORESERVE,
            -1,
            0,
        )
    };
    if addr == MAP_FAILED {
        core::ptr::null_mut()
    } else {
        addr
    }
}

/// The tests' budget of memory, which tests/memory.rs reads and writes
/// more than.
pub const MEMORY: usize = 512 << 20;

#[unsafe(no_mangle)]
extern "C" fn mikros_host_memory() -> usize {
    MEMORY
}

struct Start(extern "C" fn(*mut c_void), *mut c_void);
// SAFETY: the shim passes its argument to the thread it starts.
unsafe impl Send for Start {}

#[unsafe(no_mangle)]
extern "C" fn mikros_host_thread(entry: extern "C" fn(*mut c_void), arg: *mut c_void) -> c_int {
    let start = Start(entry, arg);
    let spawned = std::thread::Builder::new()
        .stack_size(STACK_SIZE)
        .spawn(move || {
            let start = start;
            (start.0)(start.1);
        });
    match spawned {
        Ok(_) => 0,
        Err(_) => -12,
    }
}

#[unsafe(no_mangle)]
extern "C" fn mikros_host_wait(word: *mut u32, value: u32, deadline: i64) {
    let ts = Timespec {
        sec: deadline / 1_000_000_000,
        nsec: deadline % 1_000_000_000,
    };
    let timeout = if deadline == FOREVER {
        core::ptr::null()
    } else {
        &ts as *const Timespec
    };
    // SAFETY: the shim's word lives as long as anything waits on it.
    unsafe {
        syscall(
            SYS_FUTEX,
            word,
            FUTEX_WAIT_BITSET_PRIVATE,
            value,
            timeout,
            core::ptr::null::<u32>(),
            FUTEX_BITSET_MATCH_ANY,
        );
    }
}

#[unsafe(no_mangle)]
extern "C" fn mikros_host_wake(word: *mut u32, count: c_int) {
    // SAFETY: as for waiting.
    unsafe {
        syscall(SYS_FUTEX, word, FUTEX_WAKE_PRIVATE, count);
    }
}

fn clock(id: c_int) -> i64 {
    let mut ts = Timespec { sec: 0, nsec: 0 };
    // SAFETY: the pointer is valid for the call.
    unsafe { clock_gettime(id, &mut ts) };
    ts.sec * 1_000_000_000 + ts.nsec
}

#[unsafe(no_mangle)]
extern "C" fn mikros_host_monotonic() -> i64 {
    clock(CLOCK_MONOTONIC)
}

#[unsafe(no_mangle)]
extern "C" fn mikros_host_realtime() -> i64 {
    clock(CLOCK_REALTIME)
}

#[unsafe(no_mangle)]
extern "C" fn mikros_host_random(buf: *mut c_void, len: usize) {
    let mut done = 0;
    while done < len {
        // SAFETY: the shim's buffer holds `len` bytes.
        let got = unsafe { getrandom(buf.cast::<u8>().add(done).cast(), len - done, 0) };
        if got > 0 {
            done += got as usize;
        }
    }
}

/// A disk of the host: a file, which the shim's handle points to.
pub struct Disk(pub File);

/// The negative errno of a failed call.
fn errno(error: io::Error) -> c_int {
    -error.raw_os_error().unwrap_or(5)
}

/// The disk of a handle that `Disk::add()` gave the shim.
fn disk(handle: *mut c_void) -> &'static File {
    // SAFETY: the shim passes back the handle, which points to a disk
    // that stays for the rest of the program.
    unsafe { &(*handle.cast::<Disk>()).0 }
}

#[unsafe(no_mangle)]
extern "C" fn mikros_host_disk_read(
    handle: *mut c_void,
    offset: u64,
    buf: *mut u8,
    len: usize,
) -> c_int {
    // SAFETY: the shim's buffer holds `len` bytes, which nothing else
    // touches during the call.
    let buf = unsafe { core::slice::from_raw_parts_mut(buf, len) };
    match disk(handle).read_exact_at(buf, offset) {
        Ok(()) => 0,
        Err(error) => errno(error),
    }
}

#[unsafe(no_mangle)]
extern "C" fn mikros_host_disk_write(
    handle: *mut c_void,
    offset: u64,
    buf: *const u8,
    len: usize,
) -> c_int {
    match disk(handle).write_all_at(text(buf, len), offset) {
        Ok(()) => 0,
        Err(error) => errno(error),
    }
}

#[unsafe(no_mangle)]
extern "C" fn mikros_host_disk_flush(handle: *mut c_void) -> c_int {
    match disk(handle).sync_data() {
        Ok(()) => 0,
        Err(error) => errno(error),
    }
}

/// The modes of `fallocate` that free a range of a file but keep its
/// size.
const FALLOC_FL_KEEP_SIZE: c_int = 1;
const FALLOC_FL_PUNCH_HOLE: c_int = 2;

/// Discards a range of the disk by punching a hole in its file.
#[unsafe(no_mangle)]
extern "C" fn mikros_host_disk_discard(handle: *mut c_void, offset: u64, len: u64) -> c_int {
    let (Ok(offset), Ok(len)) = (i64::try_from(offset), i64::try_from(len)) else {
        return -22;
    };
    let fd = disk(handle).as_raw_fd();
    let mode = FALLOC_FL_PUNCH_HOLE | FALLOC_FL_KEEP_SIZE;
    // SAFETY: the call takes no pointers.
    match unsafe { fallocate(fd, mode, offset, len) } {
        0 => 0,
        _ => errno(io::Error::last_os_error()),
    }
}

impl Disk {
    /// Gives the shim the disk `file` as /dev/`name`, for the rest of the
    /// program. It discards by punching holes in the file.
    pub fn add(name: &core::ffi::CStr, file: File, read_only: bool) -> Result<(), i32> {
        let size = file.metadata().map_err(errno)?.len();
        let handle = Box::into_raw(Box::new(Disk(file)));
        // SAFETY: the handle points to a disk that is never freed.
        unsafe { crate::add_disk(name, handle.cast(), size, 512, read_only, true) }
    }
}

fn text(text: *const u8, len: usize) -> &'static [u8] {
    // SAFETY: the shim passes a pointer and length of its text.
    unsafe { core::slice::from_raw_parts(text, len) }
}

#[unsafe(no_mangle)]
extern "C" fn mikros_host_log(line: *const u8, len: usize) {
    let mut err = std::io::stderr().lock();
    let _ = err.write_all(text(line, len));
    let _ = err.write_all(b"\n");
}

#[unsafe(no_mangle)]
extern "C" fn mikros_host_abort(line: *const u8, len: usize) -> ! {
    mikros_host_log(line, len);
    std::process::abort();
}

/// What the tests' stand-in for the paged memory object of a shared file
/// was asked to do, as pager ops with their first page and page count,
/// and the pages that a test calls dirty, as written through a mapping.
/// Eviction fails unless the test lets it succeed: the tests' pointers to
/// lent pages stand for mappings, which eviction would take away.
#[derive(Default)]
pub struct Object {
    pub ops: std::vec::Vec<(u32, u64, u64)>,
    pub dirty: std::collections::BTreeSet<u64>,
    pub evictable: bool,
}

/// The stand-ins, by cookie.
pub static OBJECTS: std::sync::Mutex<std::collections::BTreeMap<u64, Object>> =
    std::sync::Mutex::new(std::collections::BTreeMap::new());

const OP_EVICT: u32 = 2;
const OP_WRITEBACK_END: u32 = 4;
const OP_DISCARD: u32 = 6;
const EIO: c_int = -5;

#[unsafe(no_mangle)]
extern "C" fn mikros_host_share_op(cookie: u64, op: u32, first: u64, count: u64) -> c_int {
    let mut objects = OBJECTS.lock().unwrap();
    let object = objects.entry(cookie).or_default();
    object.ops.push((op, first, count));
    // As in the kernel, ending a writeback cleans its pages, and
    // discarding removes them.
    if op == OP_WRITEBACK_END || op == OP_DISCARD {
        object
            .dirty
            .retain(|page| !(first..first + count).contains(page));
    }
    if op == OP_EVICT && !object.evictable {
        return EIO;
    }
    0
}

#[unsafe(no_mangle)]
extern "C" fn mikros_host_share_dirty(
    cookie: u64,
    first: u64,
    count: u64,
    ranges: *mut u64,
    cap: c_long,
) -> c_long {
    let objects = OBJECTS.lock().unwrap();
    let Some(object) = objects.get(&cookie) else {
        return 0;
    };
    let mut found: std::vec::Vec<(u64, u64)> = std::vec::Vec::new();
    for &page in object.dirty.range(first..first + count) {
        match found.last_mut() {
            Some((start, n)) if *start + *n == page => *n += 1,
            _ => found.push((page, 1)),
        }
    }
    found.truncate(cap as usize);
    for (i, (start, n)) in found.iter().enumerate() {
        // SAFETY: the shim's buffer holds `cap` pairs.
        unsafe {
            ranges.add(2 * i).write(*start);
            ranges.add(2 * i + 1).write(*n);
        }
    }
    found.len() as c_long
}
