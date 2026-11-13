//! The kernel's signals, as a system test: the frame a handler gets,
//! alternate signal stacks, resetting a handler, and the signals queued
//! for a thread that ended.

#![no_std]
#![no_main]

extern crate alloc;

use core::sync::atomic::{AtomicU64, Ordering};
use mikros_rt::Managed;
use mikros_rt::sys::{self, Handle};
use mikros_test::*;

mikros_rt::managed_main!(main);

fn main(m: &Managed) -> i64 {
    mikros_test::run(
        m,
        &[
            ("signal_frame", signal_frame),
            ("alternate_signal_stacks", alternate_signal_stacks),
            (
                "reset_handler_discards_ignored",
                reset_handler_discards_ignored,
            ),
            (
                "queued_signals_of_ended_thread",
                queued_signals_of_ended_thread,
            ),
        ],
    )
}

/// The calling thread's mask.
fn signal_mask() -> u64 {
    let mut mask = 0;
    // SAFETY: the pointer is valid; adding no signal only reads the mask.
    unsafe { sys::signal_set_mask(0, 0, &mut mask) };
    mask
}

/// A handler starts with its arguments and the stack and frame in their
/// layout: the context 64-byte aligned, as high as it fits 128 bytes
/// below the interrupted stack pointer, the restorer below the
/// information record below it, the mask before the handler and the
/// extended state size above the register state. DF is clear, and the
/// signal and the action's mask are blocked while the handler runs and
/// not afterwards.
fn signal_frame(s: &Own) -> Result {
    let old = catch_usr1(0)?;
    let before = signal_mask();
    // SAFETY: the call takes no pointers.
    check("sending the signal", unsafe {
        sys::thread_signal(s.thread, USR1, 77)
    })?;
    ensure(
        "running the handler once",
        HANDLED.load(Ordering::Acquire) == 1,
    )?;
    let [signo, info, context] = SIGNAL_ARGS.each_ref().map(|a| a.load(Ordering::Acquire));
    let [rsp, rflags] = SIGNAL_ENTRY.each_ref().map(|a| a.load(Ordering::Acquire));
    ensure("passing the signal number", signo == u64::from(USR1))?;
    ensure("aligning the context", context % 64 == 0)?;
    ensure(
        "starting below the restorer",
        rsp == context - 72 && info == context - 64,
    )?;
    ensure("clearing DF", rflags & 1 << 10 == 0)?;
    let frame = signal_frame_copy();
    ensure(
        "returning to the restorer",
        frame[0] == signal_restorer as *const () as u64,
    )?;
    ensure(
        "recording the signal",
        frame[1] as u32 == USR1 && (frame[1] >> 32) == 1,
    )?;
    ensure(
        "recording the sender and the value",
        frame[2] == koid(s.process)? && frame[3] == 77 && frame[4] == 0 && frame[5] == 0,
    )?;
    let (saved_rsp, mask, size) = (frame[9 + 7], frame[29], frame[30]);
    ensure("saving the mask", mask == before)?;
    ensure("giving the extended state size", size >= 576)?;
    ensure(
        "zeroing the reserved bytes",
        frame[31] == 0 && frame[32] == 0,
    )?;
    let end = context + 0xC0 + size;
    ensure(
        "placing the frame as high as it fits",
        end + 128 <= saved_rsp && end + 128 + 64 > saved_rsp,
    )?;
    ensure(
        "blocking the signal and the action's mask in the handler",
        SIGNAL_MASK.load(Ordering::Acquire) == before | 1 << 9 | 1 << 11,
    )?;
    ensure("restoring the mask", signal_mask() == before)?;
    restore_usr1(&old)
}

/// The stack pointer `on_alternate_stack` ran with, and what removing the
/// alternate stack from there returned.
static ALTERNATE: [AtomicU64; 2] = [AtomicU64::new(0), AtomicU64::new(0)];

extern "sysv64" fn on_alternate_stack(_: u64, _: u64, _: u64) {
    let rsp: u64;
    // SAFETY: the instruction only reads the stack pointer.
    unsafe { core::arch::asm!("mov {}, rsp", out(reg) rsp) };
    // SAFETY: the call takes no pointers.
    let status = unsafe { sys::signal_set_altstack(0, 0, core::ptr::null_mut()) };
    ALTERNATE[0].store(rsp, Ordering::Release);
    ALTERNATE[1].store(status as u64, Ordering::Release);
}

/// An alternate signal stack must be at least 16384 bytes in the user
/// range; setting one reports the one before; a handler with `ONSTACK`
/// runs on it and cannot change it there; and two zeros remove it.
fn alternate_signal_stacks(s: &Own) -> Result {
    const ONSTACK: u32 = 1 << 1;
    let size = 16384;
    let base = map(s.process, create_memory(size)?, size)?;
    let set = |base: u64, size: u64, old: &mut [u64; 2]| {
        // SAFETY: `old` holds the two values the call stores.
        unsafe { sys::signal_set_altstack(base, size, old.as_mut_ptr().cast()) }
    };
    let mut old = [u64::MAX; 2];
    ensure(
        "refusing less than 16384 bytes",
        set(base, size - 1, &mut old) == INVALID_ARGS,
    )?;
    ensure(
        "refusing the kernel half",
        set(0xFFFF_8000_0000_0000, size, &mut old) == INVALID_ARGS,
    )?;
    check("setting it", set(base, size, &mut old))?;
    ensure("having had none", old == [0, 0])?;

    let mut action = [0u8; 32];
    action[0x00..0x08].copy_from_slice(&(on_alternate_stack as *const () as u64).to_le_bytes());
    action[0x10..0x18].copy_from_slice(&(signal_restorer as *const () as u64).to_le_bytes());
    action[0x18..0x1C].copy_from_slice(&ONSTACK.to_le_bytes());
    let mut previous = [0u8; 32];
    // SAFETY: the pointers describe `action` and `previous`.
    check("setting the action", unsafe {
        sys::signal_set_action(USR1, action.as_ptr(), previous.as_mut_ptr())
    })?;
    // SAFETY: the call takes no pointers.
    let sent = unsafe { sys::thread_signal(s.thread, USR1, 0) };
    restore_usr1(&previous)?;
    check("sending the signal", sent)?;
    let [rsp, status] = ALTERNATE.each_ref().map(|a| a.load(Ordering::Acquire));
    ensure("running on it", (base..base + size).contains(&rsp))?;
    ensure("keeping it there", status as i64 == BAD_STATE)?;
    check("removing it", set(0, 0, &mut old))?;
    ensure("reporting it", old == [base, size])
}

/// Blocks real-time signal 40, then waits for `USER0` on `event` and
/// exits with whatever is pending for it.
extern "sysv64" fn block_40_and_wait(event: u64, _: u64) -> ! {
    // SAFETY: the old mask is not wanted.
    unsafe { sys::signal_set_mask(0, 1 << 39, core::ptr::null_mut()) };
    STARTED.store(1, Ordering::Release);
    wait(event as Handle, USER0, i64::MAX);
    // SAFETY: the call takes no pointers.
    unsafe { sys::thread_exit() };
    unreachable!("thread_exit returned");
}

/// Real-time signals queued for a thread that ends stop counting toward
/// the process's limit of 1024: once the thread is gone, the process can
/// queue a signal again.
fn queued_signals_of_ended_thread(s: &Own) -> Result {
    const RT: u32 = 40;
    let bit = 1u64 << (RT - 1);
    let mut mask = 0;
    // SAFETY: the pointer is valid for the call.
    check("blocking signal 40 here", unsafe {
        sys::signal_set_mask(0, bit, &mut mask)
    })?;
    let mut event = 0;
    // SAFETY: the pointer is valid for the call.
    check("creating an event", unsafe {
        sys::event_create(0, &mut event)
    })?;
    STARTED.store(0, Ordering::Release);
    let thread = spawn2(s, block_40_and_wait, u64::from(event), 0)?;
    let deadline = in_time();
    while STARTED.load(Ordering::Acquire) == 0 {
        ensure("starting the thread", now() < deadline)?;
        sleep_ms(1);
    }
    // SAFETY: the calls take no pointers.
    let send = |to: Handle, process: bool| unsafe {
        match process {
            true => sys::process_signal(to, RT, 0),
            false => sys::thread_signal(to, RT, 0),
        }
    };
    for _ in 0..1024 {
        check("queueing for the thread", send(thread, false))?;
    }
    ensure("reaching the limit", send(s.process, true) == SHOULD_WAIT)?;
    signal(event, 0, USER0)?;
    ensure(
        "ending the thread",
        wait(thread, TERMINATED, in_time()).0 == 0,
    )?;
    check("queueing for the process", send(s.process, true))?;
    let mut info = [0u8; 40];
    // SAFETY: the pointer describes `info`.
    check("taking the signal", unsafe {
        sys::signal_wait(bit, soon(), info.as_mut_ptr())
    })?;
    // SAFETY: the pointer may be null.
    check("restoring the mask", unsafe {
        sys::signal_set_mask(2, mask, core::ptr::null_mut())
    })
}

/// When a handler of `CHLD` with `RESETHAND` runs, the action becomes
/// the default one, which ignores `CHLD`, so the `CHLD` still pending for
/// the process is discarded before the handler starts.
fn reset_handler_discards_ignored(s: &Own) -> Result {
    const CHLD: u32 = 17;
    const RESETHAND: u32 = 1 << 3;
    let bit = 1u64 << (CHLD - 1);
    let mut mask = 0;
    // SAFETY: the pointer is valid for the call.
    check("blocking CHLD", unsafe {
        sys::signal_set_mask(0, bit, &mut mask)
    })?;
    let mut action = [0u8; 32];
    action[0x00..0x08].copy_from_slice(&(signal_probe as *const () as u64).to_le_bytes());
    action[0x10..0x18].copy_from_slice(&(signal_restorer as *const () as u64).to_le_bytes());
    action[0x18..0x1C].copy_from_slice(&RESETHAND.to_le_bytes());
    // SAFETY: the pointer describes `action`.
    check("catching CHLD", unsafe {
        sys::signal_set_action(CHLD, action.as_ptr(), core::ptr::null_mut())
    })?;
    HANDLED.store(0, Ordering::Release);
    // SAFETY: the calls take no pointers.
    unsafe {
        check(
            "sending CHLD to the thread",
            sys::thread_signal(s.thread, CHLD, 0),
        )?;
        check(
            "sending CHLD to the process",
            sys::process_signal(s.process, CHLD, 0),
        )?;
    }
    // SAFETY: the pointer may be null.
    check("unblocking CHLD", unsafe {
        sys::signal_set_mask(1, bit, core::ptr::null_mut())
    })?;
    ensure(
        "running the handler once",
        HANDLED.load(Ordering::Acquire) == 1,
    )?;
    ensure(
        "discarding the other CHLD",
        SIGNAL_PENDING.load(Ordering::Acquire) & bit == 0,
    )?;
    // SAFETY: the pointer may be null.
    check("restoring the mask", unsafe {
        sys::signal_set_mask(2, mask, core::ptr::null_mut())
    })
}
