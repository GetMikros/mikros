//! The kernel's threads, as a system test: starting threads of the
//! test's own process, their FS and GS bases, the thread list of a
//! process, and futexes.

#![no_std]
#![no_main]

extern crate alloc;

use core::sync::atomic::{AtomicU64, Ordering};
use mikros_rt::Managed;
use mikros_rt::sys;
use mikros_test::*;

mikros_rt::managed_main!(main);

fn main(m: &Managed) -> i64 {
    mikros_test::run(
        m,
        &[
            ("threads", threads),
            ("user_bases_set_and_kept", user_bases_set_and_kept),
            ("thread_lists", thread_lists),
            ("futexes", futexes),
        ],
    )
}

/// A second thread of this process runs and exits.
fn threads(s: &Own) -> Result {
    let stack_size = 16 * 1024;
    let stack = create_memory(stack_size)?;
    let at = map(s.process, stack, stack_size)?;
    let mut thread = 0;
    let name = b"worker";
    // SAFETY: the pointers describe `name` and `thread`.
    let status =
        unsafe { sys::thread_create(s.process, name.as_ptr(), name.len() as u64, &mut thread) };
    check("creating a thread", status)?;
    // The entry is called as if by a call instruction, which leaves the
    // stack 8 bytes below a 16-byte boundary.
    let entry = thread_entry as *const () as u64;
    let top = 0x7FFF_FFFF_F000;
    // SAFETY: the calls take no pointers; the stack is mapped and unused.
    unsafe {
        // Addresses outside the user range are refused and leave the
        // thread NEW; unmapped ones inside it are not checked.
        ensure(
            "refusing entry 0",
            sys::thread_start(thread, 0, at, 0, 0) == -10,
        )?;
        ensure(
            "refusing a stack past the user range",
            sys::thread_start(thread, entry, top + PAGE_SIZE, 0, 0) == -10,
        )?;
        ensure(
            "refusing a kernel entry",
            sys::thread_start(thread, 0xFFFF_8000_0000_0000, at, 0, 0) == -10,
        )?;
        let status = sys::thread_start(thread, entry, at + stack_size - 8, 77, 0);
        check("starting the thread", status)?;
    }
    for _ in 0..1000 {
        if THREAD_RAN.load(Ordering::Acquire) == 77 {
            return Ok(());
        }
        sleep_ms(1);
    }
    ensure("waiting for the thread", false)
}

/// The futex word of the futex test and the order its waiters woke in.
static FUTEX_ORDER: [AtomicU64; 3] = [AtomicU64::new(0), AtomicU64::new(0), AtomicU64::new(0)];

static FUTEX_WOKEN: AtomicU64 = AtomicU64::new(0);

extern "sysv64" fn futex_waiter(addr: u64, index: u64) -> ! {
    // SAFETY: `addr` is a mapped, aligned word.
    let status = unsafe { sys::futex_wait(addr as *const u32, 0, i64::MAX) };
    if status == 0 {
        let at = FUTEX_WOKEN.fetch_add(1, Ordering::AcqRel) as usize;
        FUTEX_ORDER[at].store(index, Ordering::Release);
    }
    // SAFETY: the call takes no pointers.
    unsafe { sys::thread_exit() };
    unreachable!("thread_exit returned");
}

/// The index of the waiter that `futex_waiter` recorded as woken
/// `at`-th, once one has, or a failure after `in_time`.
fn futex_woken(at: usize) -> Result<u64> {
    let deadline = in_time();
    loop {
        let index = FUTEX_ORDER[at].load(Ordering::Acquire);
        if index != 0 {
            return Ok(index);
        }
        ensure("recording the woken waiter", now() < deadline)?;
        sleep_ms(1);
    }
}

/// Futexes compare before blocking, time out, wake in arrival order and
/// are keyed by memory object and offset.
fn futexes(s: &Own) -> Result {
    let mem = create_memory(PAGE_SIZE)?;
    let first = map(s.process, mem, PAGE_SIZE)?;
    let second = map(s.process, mem, PAGE_SIZE)?;
    let word = first + 64;
    // SAFETY: the calls read the mapped word or check the address.
    unsafe {
        ensure(
            "comparing",
            sys::futex_wait(word as *const u32, 1, i64::MAX) == -20,
        )?;
        ensure(
            "needing alignment",
            sys::futex_wait((word + 2) as *const u32, 0, i64::MAX) == -10,
        )?;
        ensure(
            "needing a mapping",
            sys::futex_wait(0x1000 as *const u32, 0, i64::MAX) == -10,
        )?;
        ensure(
            "timing out",
            sys::futex_wait(word as *const u32, 0, soon()) == TIMED_OUT,
        )?;
    }
    // Each waiter is blocked before the next starts, so that they wait
    // in the order of their indices.
    for index in 1..=3 {
        let waiter = spawn2(s, futex_waiter, word, index)?;
        wait_until_blocked(waiter)?;
    }
    ensure("waking one", futex_wake(word, 1)? == 1)?;
    ensure("waking the oldest", futex_woken(0)? == 1)?;
    // The second mapping reaches the same futex. Waiters are woken one
    // at a time, as threads woken together may run in either order.
    ensure("waking the next", futex_wake(second + 64, 1)? == 1)?;
    ensure("waking in order", futex_woken(1)? == 2)?;
    ensure("waking the rest", futex_wake(word, u32::MAX)? == 1)?;
    ensure("waking the last", futex_woken(2)? == 3)?;
    ensure("waking nobody", futex_wake(word, 1)? == 0)
}

/// A process lists its threads that are not dead, and hands out handles
/// to them by object ID.
fn thread_lists(s: &Own) -> Result {
    let mut event = 0;
    // SAFETY: the pointer is valid for the call.
    check("creating an event", unsafe {
        sys::event_create(0, &mut event)
    })?;
    let thread = spawn(s, wait_forever, u64::from(event))?;
    let id = koid(thread)?;
    let (koids, count) = thread_list(s.process)?;
    ensure(
        "listing this thread",
        koids[..count].contains(&koid(s.thread)?),
    )?;
    ensure("listing the new thread", koids[..count].contains(&id))?;
    let (status, got) = get_thread(s.process, id);
    check("getting the thread", status)?;
    let (basic, created) = (info::<0x18>(got, 1)?, info::<0x18>(thread, 1)?);
    ensure(
        "returning the thread with the default rights",
        u64_at(&basic, 0) == id && u32_at(&basic, 0x14) == u32_at(&created, 0x14),
    )?;
    let mut short = [0u8; 8];
    let mut len = 0;
    // SAFETY: the pointers and length describe `short` and `len`.
    let status = unsafe { sys::object_get_info(s.process, 7, short.as_mut_ptr(), 8, &mut len) };
    ensure(
        "giving the size to a short buffer",
        status == -15 && len == 8 * count as u64,
    )?;
    // SAFETY: as above.
    let status = unsafe { sys::object_get_info(thread, 7, short.as_mut_ptr(), 8, &mut len) };
    ensure("listing only for processes", status == -12)?;
    // SAFETY: the call takes no pointers.
    check("killing", unsafe { sys::task_kill(thread) })?;
    ensure("ending", wait(thread, TERMINATED, in_time()).0 == 0)?;
    let (koids, count) = thread_list(s.process)?;
    ensure("leaving out dead threads", !koids[..count].contains(&id))?;
    ensure(
        "not finding a dead thread",
        get_thread(s.process, id).0 == NOT_FOUND,
    )?;
    ensure(
        "not finding another object",
        get_thread(s.process, koid(event)?).0 == NOT_FOUND,
    )
}

/// The calling thread's FS and GS bases.
fn user_bases() -> (u64, u64) {
    let (fs, gs);
    // SAFETY: the kernel enables FSGSBASE, which QEMU reports, so the
    // instructions only read the bases.
    unsafe {
        core::arch::asm!(
            "rdfsbase {fs}",
            "rdgsbase {gs}",
            fs = out(reg) fs,
            gs = out(reg) gs,
            options(nomem, nostack, preserves_flags),
        )
    };
    (fs, gs)
}

/// `thread_set_user_base` sets the FS and GS bases, which keep their
/// values while the thread is switched out, and refuses another register
/// or a base outside the user address range.
fn user_bases_set_and_kept(_: &Own) -> Result {
    let before = user_bases();
    let set = |which, value| {
        // SAFETY: the call takes no pointers.
        check("setting a base", unsafe {
            sys::thread_set_user_base(which, value)
        })
    };
    let wanted = (0x1234_5000, 0x2345_6000);
    set(0, wanted.0)?;
    set(1, wanted.1)?;
    ensure("setting the bases", user_bases() == wanted)?;
    sleep_ms(2);
    ensure("keeping the bases", user_bases() == wanted)?;
    // SAFETY: the calls take no pointers.
    unsafe {
        ensure(
            "refusing another register",
            sys::thread_set_user_base(2, 0) == INVALID_ARGS,
        )?;
        ensure(
            "refusing a kernel address",
            sys::thread_set_user_base(1, 0xFFFF_8000_0000_0000) == INVALID_ARGS,
        )?;
    }
    ensure("keeping the bases when refusing", user_bases() == wanted)?;
    set(0, before.0)?;
    set(1, before.1)
}
