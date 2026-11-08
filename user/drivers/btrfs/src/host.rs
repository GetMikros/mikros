// SPDX-License-Identifier: GPL-2.0
//! The btrfs shim's host layer on Mikros: the `mikros_host_` functions of
//! `user/btrfs/shim/host.h`, on the system calls of the kernel.
//!
//! Each thread of the shim is a thread of this process with a block of
//! thread-local storage of its own, laid out from the program's TLS
//! segment as x86-64's variant II has it, with the FS base at its end.
//! Threads whose work is done wait for more rather than exit, so that
//! their stacks and TLS blocks are reused.

use alloc::boxed::Box;
use alloc::vec::Vec;
use core::ffi::{c_int, c_void};
use core::sync::atomic::{AtomicU32, AtomicU64, AtomicUsize, Ordering};
use mikros_rt::log::{self, Severity};
use mikros_rt::sys;

const PERM_READ: u32 = 1 << 0;
const PERM_WRITE: u32 = 1 << 1;
const PAGE: u64 = 4096;
const FOREVER: i64 = i64::MAX;
/// The shim's deadline for no deadline.
const SHIM_FOREVER: i64 = -1;
/// The `thread_set_user_base` register of the thread pointer.
const FS_BASE: u32 = 0;
/// The stack of each thread: kernel code keeps frames small, but its
/// call chains are deep.
const STACK_SIZE: u64 = 1 << 20;
const ENOMEM: c_int = 12;
const EIO: c_int = 5;

/// The process, whose address space the host maps memory into.
static PROCESS: AtomicU32 = AtomicU32::new(0);
/// The bytes of memory the shim may hold.
static MEMORY: AtomicU64 = AtomicU64::new(0);
/// The time page of the time service, 0 while there is none.
static TIME_PAGE: AtomicU64 = AtomicU64::new(0);

/// Readies the host layer for the shim on the calling thread, the
/// shim's first, with `process` to map memory into, `memory` bytes for
/// the shim, and the time page at `time_page`, or 0 without one.
pub fn init(process: sys::Handle, memory: u64, time_page: u64) {
    PROCESS.store(process, Ordering::Relaxed);
    MEMORY.store(memory, Ordering::Relaxed);
    TIME_PAGE.store(time_page, Ordering::Relaxed);
    let tp = new_tls().expect("no memory for the first thread's TLS");
    // SAFETY: setting the FS base takes no pointers; nothing of this
    // thread uses TLS yet.
    unsafe { sys::thread_set_user_base(FS_BASE, tp) };
}

/// Maps `len` bytes, a whole number of pages, of a new memory object,
/// which reads as zero and takes memory only once touched.
fn map(len: u64) -> Option<u64> {
    let mut mem = 0;
    // SAFETY: the out pointer is valid for the call.
    if unsafe { sys::mem_create(len, 0, &mut mem) } != mikros_rt::OK {
        return None;
    }
    let mut addr = 0;
    let process = PROCESS.load(Ordering::Relaxed);
    // SAFETY: the out pointer is valid; the kernel picks a free range.
    let status = unsafe { sys::vm_map(process, PERM_READ | PERM_WRITE, 0, mem, 0, len, &mut addr) };
    // The mapping keeps the object alive.
    // SAFETY: closing takes no pointers.
    unsafe { sys::handle_close(mem) };
    (status == mikros_rt::OK).then_some(addr)
}

#[unsafe(no_mangle)]
extern "C" fn mikros_host_reserve(size: u64) -> *mut c_void {
    match map(size.next_multiple_of(PAGE)) {
        Some(addr) => addr as *mut c_void,
        None => core::ptr::null_mut(),
    }
}

#[unsafe(no_mangle)]
extern "C" fn mikros_host_memory() -> u64 {
    MEMORY.load(Ordering::Relaxed)
}

/// The program's TLS segment, from its program headers.
struct TlsImage {
    /// The initial bytes, and how many of them.
    data: u64,
    file_size: u64,
    /// The block's size and alignment.
    size: u64,
    align: u64,
}

unsafe extern "C" {
    /// The program's ELF header, which the linker names.
    static __ehdr_start: [u8; 64];
}

const PT_LOAD: u32 = 1;
const PT_TLS: u32 = 7;

fn tls_image() -> TlsImage {
    // SAFETY: the linker places the ELF header and the program headers
    // in the first loaded segment, which stays mapped.
    unsafe {
        let ehdr = core::ptr::addr_of!(__ehdr_start).cast::<u8>();
        let phoff = ehdr.add(32).cast::<u64>().read_unaligned();
        let phentsize = ehdr.add(54).cast::<u16>().read_unaligned() as usize;
        let phnum = ehdr.add(56).cast::<u16>().read_unaligned() as usize;
        let phdr = |i: usize| ehdr.add(phoff as usize + i * phentsize);
        let field = |p: *const u8, at: usize| p.add(at).cast::<u64>().read_unaligned();
        let mut bias = 0;
        for i in 0..phnum {
            let p = phdr(i);
            if p.cast::<u32>().read_unaligned() == PT_LOAD && field(p, 8) == 0 {
                bias = ehdr as u64 - field(p, 16);
            }
        }
        for i in 0..phnum {
            let p = phdr(i);
            if p.cast::<u32>().read_unaligned() == PT_TLS {
                return TlsImage {
                    data: bias + field(p, 16),
                    file_size: field(p, 32),
                    size: field(p, 40),
                    align: field(p, 48).max(8),
                };
            }
        }
    }
    TlsImage {
        data: 0,
        file_size: 0,
        size: 0,
        align: 8,
    }
}

/// A block of thread-local storage, with the thread pointer that the FS
/// base takes: the block lies just below it, and it points to itself.
fn new_tls() -> Option<u64> {
    let image = tls_image();
    let below = image.size.next_multiple_of(image.align);
    let len = (below + 8 + image.align).next_multiple_of(PAGE);
    let start = map(len)?;
    let tp = (start + below).next_multiple_of(image.align);
    // SAFETY: the block is fresh and `len` bytes long, which holds the
    // image below `tp` and the self-pointer at it.
    unsafe {
        core::ptr::copy_nonoverlapping(
            image.data as *const u8,
            (tp - below) as *mut u8,
            image.file_size as usize,
        );
        (tp as *mut u64).write(tp);
    }
    Some(tp)
}

/// Work for a thread of the pool: run `entry(arg)`.
struct Work {
    entry: extern "C" fn(*mut c_void),
    arg: *mut c_void,
}

/// Where a thread of the pool that waits for work finds it, and the
/// word it waits on, which is 1 once the work is there.
struct Slot {
    work: AtomicUsize,
    given: AtomicU32,
}

/// The threads of the pool that wait for work.
static IDLE: Spin<Vec<&'static Slot>> = Spin::new(Vec::new());

#[unsafe(no_mangle)]
extern "C" fn mikros_host_thread(entry: extern "C" fn(*mut c_void), arg: *mut c_void) -> c_int {
    let work = Box::into_raw(Box::new(Work { entry, arg })) as usize;
    if let Some(slot) = IDLE.lock().pop() {
        slot.work.store(work, Ordering::Relaxed);
        slot.given.store(1, Ordering::Release);
        wake(&slot.given);
        return 0;
    }
    match spawn(work) {
        Some(()) => 0,
        None => {
            // SAFETY: the work was not handed to a thread.
            drop(unsafe { Box::from_raw(work as *mut Work) });
            -ENOMEM
        }
    }
}

/// Starts a thread of the pool that does `work` first.
fn spawn(work: usize) -> Option<()> {
    let tp = new_tls()?;
    let stack = map(STACK_SIZE)?;
    let mut thread = 0;
    let process = PROCESS.load(Ordering::Relaxed);
    let name = b"btrfs";
    // SAFETY: the name and the out pointer are valid for the call.
    let created =
        unsafe { sys::thread_create(process, name.as_ptr(), name.len() as u64, &mut thread) };
    if created != mikros_rt::OK {
        return None;
    }
    // The entry's `call` leaves the stack as a function's entry expects.
    let sp = stack + STACK_SIZE - 16;
    // SAFETY: the stack is mapped and the thread's alone.
    let started = unsafe {
        sys::thread_start(
            thread,
            thread_entry as *const () as u64,
            sp,
            tp,
            work as u64,
        )
    };
    // The thread runs without its handle.
    // SAFETY: closing takes no pointers.
    unsafe { sys::handle_close(thread) };
    (started == mikros_rt::OK).then_some(())
}

/// The first code of a thread, with its thread pointer and its first
/// work in RDI and RSI.
#[unsafe(naked)]
unsafe extern "C" fn thread_entry() -> ! {
    core::arch::naked_asm!(
        "xor ebp, ebp",
        "call {main}",
        "ud2",
        main = sym thread_main,
    )
}

extern "C" fn thread_main(tp: u64, mut work: usize) -> ! {
    // SAFETY: setting the FS base takes no pointers.
    unsafe { sys::thread_set_user_base(FS_BASE, tp) };
    let slot: &'static Slot = Box::leak(Box::new(Slot {
        work: AtomicUsize::new(0),
        given: AtomicU32::new(0),
    }));
    loop {
        // SAFETY: `mikros_host_thread` made the work, for one thread.
        let Work { entry, arg } = *unsafe { Box::from_raw(work as *mut Work) };
        entry(arg);
        IDLE.lock().push(slot);
        while slot.given.swap(0, Ordering::Acquire) == 0 {
            // SAFETY: the word stays valid while the thread waits on it.
            unsafe { sys::futex_wait(slot.given.as_ptr(), 0, FOREVER) };
        }
        work = slot.work.swap(0, Ordering::Relaxed);
    }
}

fn wake(word: &AtomicU32) {
    let mut woken = 0;
    // SAFETY: the word is valid for the call.
    unsafe { sys::futex_wake(word.as_ptr(), 1, &mut woken) };
}

#[unsafe(no_mangle)]
extern "C" fn mikros_host_wait(word: *mut u32, value: u32, deadline: i64) {
    let deadline = if deadline == SHIM_FOREVER {
        FOREVER
    } else {
        deadline
    };
    // SAFETY: the shim's word lives as long as anything waits on it.
    unsafe { sys::futex_wait(word, value, deadline) };
}

#[unsafe(no_mangle)]
extern "C" fn mikros_host_wake(word: *mut u32, count: c_int) {
    let mut woken = 0;
    // SAFETY: as for waiting.
    unsafe { sys::futex_wake(word, count.max(0) as u32, &mut woken) };
}

#[unsafe(no_mangle)]
extern "C" fn mikros_host_monotonic() -> i64 {
    let mut now = 0;
    // SAFETY: the out pointer is valid for the call.
    unsafe { sys::clock_get_monotonic(&mut now) };
    now
}

/// UTC from the time page, whose sequence count is even while its
/// offset from the monotonic clock is whole; the monotonic clock while
/// there is no time service.
#[unsafe(no_mangle)]
extern "C" fn mikros_host_realtime() -> i64 {
    let page = TIME_PAGE.load(Ordering::Relaxed);
    let monotonic = mikros_host_monotonic();
    if page == 0 {
        return monotonic;
    }
    let read = |at: u64| {
        // SAFETY: the page is mapped read-only for as long as the
        // process runs.
        unsafe { core::ptr::read_volatile(at as *const u64) }
    };
    let offset = loop {
        let before = read(page) as u32;
        core::sync::atomic::fence(Ordering::Acquire);
        let offset = read(page + 8) as i64;
        core::sync::atomic::fence(Ordering::Acquire);
        if before % 2 == 0 && read(page) as u32 == before {
            break offset;
        }
        core::hint::spin_loop();
    };
    offset.saturating_add(monotonic)
}

#[unsafe(no_mangle)]
extern "C" fn mikros_host_random(buf: *mut u8, len: u64) {
    // SAFETY: the shim's buffer holds `len` bytes.
    unsafe { sys::system_get_random(buf, len) };
}

fn text<'a>(text: *const u8, len: u64) -> &'a str {
    // SAFETY: the shim passes a pointer and length of its text.
    let bytes = unsafe { core::slice::from_raw_parts(text, len as usize) };
    core::str::from_utf8(bytes).unwrap_or("(a line that is not UTF-8)")
}

#[unsafe(no_mangle)]
extern "C" fn mikros_host_log(line: *const u8, len: u64) {
    log::write(Severity::Info, format_args!("{}", text(line, len)));
}

#[unsafe(no_mangle)]
extern "C" fn mikros_host_abort(line: *const u8, len: u64) -> ! {
    log::write(Severity::Error, format_args!("{}", text(line, len)));
    mikros_rt::exit(mikros_rt::PANIC_EXIT_CODE)
}

/// The server cannot read partitions yet: every disk call fails.
#[unsafe(no_mangle)]
extern "C" fn mikros_host_disk_read(_: *mut c_void, _: u64, _: *mut u8, _: usize) -> c_int {
    -EIO
}

#[unsafe(no_mangle)]
extern "C" fn mikros_host_disk_write(_: *mut c_void, _: u64, _: *const u8, _: usize) -> c_int {
    -EIO
}

#[unsafe(no_mangle)]
extern "C" fn mikros_host_disk_flush(_: *mut c_void) -> c_int {
    -EIO
}

/// A spinning lock for what the host layer keeps between threads, which
/// it holds only for a few instructions.
pub struct Spin<T> {
    locked: AtomicU32,
    value: core::cell::UnsafeCell<T>,
}

// SAFETY: the value is only touched with the lock held.
unsafe impl<T: Send> Sync for Spin<T> {}

impl<T> Spin<T> {
    pub const fn new(value: T) -> Self {
        Spin {
            locked: AtomicU32::new(0),
            value: core::cell::UnsafeCell::new(value),
        }
    }

    pub fn lock(&self) -> SpinGuard<'_, T> {
        while self
            .locked
            .compare_exchange_weak(0, 1, Ordering::Acquire, Ordering::Relaxed)
            .is_err()
        {
            core::hint::spin_loop();
        }
        SpinGuard { spin: self }
    }
}

pub struct SpinGuard<'a, T> {
    spin: &'a Spin<T>,
}

impl<T> core::ops::Deref for SpinGuard<'_, T> {
    type Target = T;
    fn deref(&self) -> &T {
        // SAFETY: the guard holds the lock.
        unsafe { &*self.spin.value.get() }
    }
}

impl<T> core::ops::DerefMut for SpinGuard<'_, T> {
    fn deref_mut(&mut self) -> &mut T {
        // SAFETY: the guard holds the lock.
        unsafe { &mut *self.spin.value.get() }
    }
}

impl<T> Drop for SpinGuard<'_, T> {
    fn drop(&mut self) {
        self.spin.locked.store(0, Ordering::Release);
    }
}
