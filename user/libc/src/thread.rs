//! Threads: musl's `__clone` and `__unmapself`, and the end of a thread.
//!
//! A thread is created with `thread_create` and started with
//! `thread_start` on the stack musl gives it, where the entry finds the
//! function and its argument; the entry sets the thread pointer before
//! it calls the function. Each thread's clear-on-exit address, which
//! `CLONE_CHILD_CLEARTID` and `set_tid_address` give, is kept with the
//! thread's TLS pointer. When the thread ends, it stores 0 there and
//! wakes its waiters with nothing but registers, so that its stack can
//! be freed as soon as the store is seen, and then exits.

use crate::errno::{self, ENOSYS};
use crate::lock::Lock;
use crate::syscall;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicU32, Ordering};
use mikros_rt::sys::{self, Handle};

const CLONE_VM: i32 = 0x100;
const CLONE_THREAD: i32 = 0x10000;
const CLONE_SETTLS: i32 = 0x80000;
const CLONE_PARENT_SETTID: i32 = 0x100000;
const CLONE_CHILD_CLEARTID: i32 = 0x200000;
/// The `thread_set_user_base` register of the thread pointer.
const FS_BASE: u32 = 0;

struct Thread {
    tid: u32,
    handle: Handle,
    /// Whether the table owns the handle; the startup transfer holds the
    /// first thread's.
    owned: bool,
    /// The alternate signal stack, as base and size, or zeros.
    altstack: (u64, u64),
    /// The stack pointer the thread started with, or 0 for the first.
    start: usize,
    /// The thread pointer, which is the address of musl's thread.
    tls: usize,
    /// The address to clear and wake when the thread ends, or 0.
    clear: usize,
}

static THREADS: Lock<Vec<Thread>> = Lock::new(Vec::new());
/// The handle of the first thread, from the startup transfer.
static FIRST: AtomicU32 = AtomicU32::new(0);

/// Keeps the handle of the first thread, which `set_tid_address` records.
pub fn install(first: Handle) {
    FIRST.store(first, Ordering::Relaxed);
}
/// Where the ending threads' wakes store their counts.
static mut WOKEN: u32 = 0;

/// The thread pointer of the calling thread: musl keeps its thread's
/// address at offset 0 of it.
fn tls() -> usize {
    let tp: usize;
    // SAFETY: reading the FS base through FS:0 takes no memory of ours.
    unsafe { core::arch::asm!("mov {}, fs:[0]", out(reg) tp, options(nostack, readonly)) };
    tp
}

/// `set_tid_address`: makes `clear` the calling thread's clear-on-exit
/// address, and returns its ID. The first thread is recorded when musl
/// first calls it, with `first` as its ID.
pub fn set_tid_address(clear: usize, first: u32) -> i64 {
    let tp = tls();
    let mut threads = THREADS.lock();
    if let Some(t) = threads.iter_mut().find(|t| t.tls == tp) {
        t.clear = clear;
        // The dynamic linker first calls it before the startup transfer
        // gave the first thread's ID and handle.
        if !t.owned {
            t.tid = first;
            t.handle = FIRST.load(Ordering::Relaxed);
        }
        return i64::from(t.tid);
    }
    threads.push(Thread {
        tid: first,
        handle: FIRST.load(Ordering::Relaxed),
        owned: false,
        altstack: (0, 0),
        start: 0,
        tls: tp,
        clear,
    });
    i64::from(first)
}

/// The handle of thread `tid`.
pub fn handle_of(tid: u32) -> Option<Handle> {
    THREADS
        .lock()
        .iter()
        .find(|t| t.tid == tid)
        .map(|t| t.handle)
}

/// The handle of the thread that started with a stack pointer at most a
/// page above `sp`, which a thread interrupted before its entry set its
/// thread pointer has.
pub fn starting(sp: u64) -> Option<Handle> {
    let sp = sp as usize;
    let threads = THREADS.lock();
    threads
        .iter()
        .find(|t| t.start != 0 && sp <= t.start && t.start - sp <= 4096)
        .map(|t| t.handle)
}

/// The alternate signal stack of the calling thread, as base and size.
pub fn altstack() -> (u64, u64) {
    let tp = tls();
    let threads = THREADS.lock();
    threads
        .iter()
        .find(|t| t.tls == tp)
        .map_or((0, 0), |t| t.altstack)
}

/// Records the alternate signal stack of the calling thread.
pub fn set_altstack(altstack: (u64, u64)) {
    let tp = tls();
    if let Some(t) = THREADS.lock().iter_mut().find(|t| t.tls == tp) {
        t.altstack = altstack;
    }
}

/// `gettid`: the ID of the calling thread.
pub fn gettid() -> i64 {
    let tp = tls();
    let threads = THREADS.lock();
    threads
        .iter()
        .find(|t| t.tls == tp)
        .map_or(-errno::ESRCH, |t| i64::from(t.tid))
}

/// `__clone` for the threads of `pthread_create`: starts `func(arg)` on
/// `stack` with the thread pointer `tls`, and returns the thread's ID.
/// Other kinds of clone are not supported.
#[unsafe(no_mangle)]
extern "C" fn __mikros_clone(
    func: usize,
    stack: usize,
    flags: i32,
    arg: usize,
    ptid: *mut i32,
    tls: usize,
    ctid: usize,
) -> i32 {
    let needed = CLONE_VM | CLONE_THREAD | CLONE_SETTLS;
    if flags & needed != needed {
        return -ENOSYS as i32;
    }
    let mut handle = 0;
    // SAFETY: the name is empty, and the out pointer is valid.
    let status =
        unsafe { sys::thread_create(syscall::process(), core::ptr::null(), 0, &mut handle) };
    if status != mikros_rt::OK {
        return -errno::from_status(status) as i32;
    }
    let tid = crate::start::object_id(handle).unwrap_or(0);
    if flags & CLONE_PARENT_SETTID != 0 {
        // SAFETY: musl passes the address of the thread's ID.
        unsafe { ptid.write_volatile(tid as i32) };
    }
    let clear = if flags & CLONE_CHILD_CLEARTID != 0 {
        ctid
    } else {
        0
    };
    // The entry pops the function and its argument.
    let sp = (stack & !15) - 16;
    THREADS.lock().push(Thread {
        tid,
        handle,
        owned: true,
        altstack: (0, 0),
        start: sp,
        tls,
        clear,
    });
    // SAFETY: musl's stack for the thread is mapped and unused below
    // `stack`.
    unsafe {
        (sp as *mut usize).write(func);
        ((sp + 8) as *mut usize).write(arg);
    }
    let entry = thread_entry as *const () as u64;
    // SAFETY: starting a thread takes no pointers of ours.
    let status = unsafe { sys::thread_start(handle, entry, sp as u64, tls as u64, 0) };
    if status != mikros_rt::OK {
        THREADS.lock().retain(|t| t.tls != tls);
        // SAFETY: closing takes no pointers.
        unsafe { sys::handle_close(handle) };
        return -errno::from_status(status) as i32;
    }
    tid as i32
}

/// The first code of a new thread, with its thread pointer in RDI and
/// its function and argument at the top of its stack, which is aligned
/// to 16 bytes.
#[unsafe(naked)]
unsafe extern "C" fn thread_entry() -> ! {
    core::arch::naked_asm!(
        "xor ebp, ebp",
        "call {begin}",
        "pop rax",
        "pop rdi",
        "call rax",
        "mov edi, eax",
        "call {end}",
        "ud2",
        begin = sym thread_begin,
        end = sym thread_return,
    )
}

extern "C" fn thread_begin(tls: u64) {
    // SAFETY: setting the FS base takes no pointers.
    unsafe { sys::thread_set_user_base(FS_BASE, tls) };
}

extern "C" fn thread_return(_code: i32) -> ! {
    exit()
}

/// Removes the calling thread from the table, closes its handle and
/// returns its clear-on-exit address.
fn forget() -> usize {
    let tp = tls();
    let mut threads = THREADS.lock();
    let Some(at) = threads.iter().position(|t| t.tls == tp) else {
        return 0;
    };
    let thread = threads.swap_remove(at);
    if thread.owned {
        // SAFETY: closing takes no pointers; the thread runs on.
        unsafe { sys::handle_close(thread.handle) };
    }
    thread.clear
}

/// Ends the calling thread.
pub fn exit() -> ! {
    let clear = forget();
    // SAFETY: nothing of the thread is used after the clear.
    unsafe { finish(0, 0, clear) }
}

/// `__unmapself`: unmaps the `size` bytes at `base`, which hold the
/// calling thread's stack, and ends the thread.
#[unsafe(no_mangle)]
extern "C" fn __mikros_unmapself(base: usize, size: usize) -> ! {
    let clear = forget();
    // SAFETY: nothing of the thread is used after the unmapping.
    unsafe { finish(base, size, clear) }
}

/// Unmaps the `size` bytes at `base` when `size` is not 0, stores 0 at
/// `clear` and wakes its waiters when it is not 0, and ends the thread,
/// all without the stack.
///
/// # Safety
///
/// Nothing may use the unmapped memory, and `clear` must be the address
/// of a futex word or 0.
unsafe fn finish(base: usize, size: usize, clear: usize) -> ! {
    // SAFETY: the system calls change only RAX, RCX and R11, and the
    // thread ends.
    unsafe {
        core::arch::asm!(
            "test rdx, rdx",
            "jz 2f",
            "mov eax, {vm_unmap}",
            "syscall",
            "2:",
            "test r8, r8",
            "jz 3f",
            "mov dword ptr [r8], 0",
            "mov rdi, r8",
            "mov esi, -1",
            "mov rdx, r9",
            "mov eax, {futex_wake}",
            "syscall",
            "3:",
            "mov eax, {thread_exit}",
            "syscall",
            "ud2",
            vm_unmap = const 21,
            futex_wake = const 67,
            thread_exit = const 13,
            in("rdi") syscall::process(),
            in("rsi") base,
            in("rdx") size,
            in("r8") clear,
            in("r9") &raw mut WOKEN,
            options(noreturn, nostack),
        )
    }
}
