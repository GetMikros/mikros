//! The kernel on several processors, as a system test: the processor
//! count and affinity, a signal that reaches a thread on another
//! processor, killing a thread as it sleeps, and a sleeper beside a
//! spinning thread.

#![no_std]
#![no_main]

extern crate alloc;

use core::sync::atomic::Ordering;
use mikros_rt::Managed;
use mikros_rt::sys::{self, Handle};
use mikros_test::*;

mikros_rt::managed_main!(main);

fn main(m: &Managed) -> i64 {
    mikros_test::run(
        m,
        &[
            ("cpu_count_and_affinity", cpu_count_and_affinity),
            (
                "signal_reaches_other_processor",
                signal_reaches_other_processor,
            ),
            ("killed_as_it_sleeps", killed_as_it_sleeps),
            ("sleeper_runs_beside_spinner", sleeper_runs_beside_spinner),
        ],
    )
}

/// The processor count is between 1 and 256, a mask that selects no
/// processor the kernel uses is refused, and this thread still runs once
/// restricted to processor 0.
fn cpu_count_and_affinity(s: &Own) -> Result {
    let mut count = 0u32;
    // SAFETY: the pointer describes `count`.
    let status = unsafe { sys::system_get_cpu_count(&mut count) };
    check("reading the processor count", status)?;
    ensure("counting the processors", (1..=256).contains(&count))?;
    let set = |mask: &[u8]| {
        // SAFETY: the pointer and length describe `mask`.
        unsafe { sys::thread_set_affinity(s.thread, mask.as_ptr(), mask.len() as u64) }
    };
    ensure("refusing an empty mask", set(&[0; 4]) == -10)?;
    if count < 256 {
        let mut beyond = [0u8; 32];
        beyond[count as usize / 8] = 1 << (count % 8);
        ensure("refusing a processor not used", set(&beyond) == -10)?;
    }
    check("restricting to processor 0", set(&[1]))?;
    sleep_ms(1);
    check("allowing every processor", set(&[0xFF; 32]))
}

/// A thread that often blocked after running briefly still runs soon
/// after waking while another thread spins.
fn sleeper_runs_beside_spinner(s: &Own) -> Result {
    for _ in 0..1000 {
        // SAFETY: the call takes no pointers.
        unsafe { sys::thread_sleep(now() + 1000) };
    }
    let thread = spawn(s, spin, 0)?;
    let start = now();
    sleep_ms(5);
    let slept = now() - start;
    // SAFETY: the call takes no pointers.
    check("killing the spinner", unsafe { sys::task_kill(thread) })?;
    ensure("ending", wait(thread, TERMINATED, in_time()).0 == 0)?;
    ensure("waking in time", slept < 100_000_000)
}

/// A signal sent to a thread that spins in user mode on another processor
/// runs its handler at once, not only when the thread's time slice ends.
/// A slice of a thread alone lasts 4 ms. A first round is not timed: it
/// also runs the handler's code and its return for the first time, which
/// an emulator such as QEMU without KVM takes milliseconds to translate.
/// Each round starts once the spinner spins again.
///
/// The rounds are measured in the spinner's loops, not in time, so that
/// the host pausing an emulated processor does not fail them: a paused
/// spinner does not loop either. Its loop rate is the highest of
/// `WINDOWS` windows of 4 ms, since a pause only lowers a window's rate.
/// A round is quick when the spinner loops less than 2 ms' worth, half a
/// slice, or `SLOWER` times that in a fuzz build, between the signal and
/// the start of the handler. At least `QUICK` of the ten rounds after the
/// first must be quick: a spinner paused for longer than a slice may meet
/// its timer at once as it resumes, and so look quick without the signal
/// interrupting it, but not in most rounds.
///
/// The spinner ends and the action is restored whether the rounds passed
/// or not, so that a failure leaves nothing behind for later tests.
fn signal_reaches_other_processor(s: &Own) -> Result {
    const ROUNDS: u64 = 10;
    const QUICK: u64 = 8;
    const WINDOWS: usize = 5;
    const WINDOW: i64 = 4_000_000;
    const LIMIT: u64 = SLOWER as u64 * 2_000_000;
    let mut count = 0u32;
    // SAFETY: the pointer describes `count`.
    check("reading the processor count", unsafe {
        sys::system_get_cpu_count(&mut count)
    })?;
    if count < 2 {
        return Ok(());
    }
    let set = |thread: Handle, mask: &[u8]| {
        // SAFETY: the pointer and length describe `mask`.
        unsafe { sys::thread_set_affinity(thread, mask.as_ptr(), mask.len() as u64) }
    };
    check("keeping this thread on processor 0", set(s.thread, &[1]))?;
    let old = catch_usr1(0)?;
    SPINNER_LOOPS.store(0, Ordering::Release);
    let thread = spawn2(s, spin_until_handled, ROUNDS + 1, 0)?;
    let rounds = (|| -> Result {
        check("moving the spinner to processor 1", set(thread, &[2]))?;
        let deadline = in_time();
        while SPINNER_LOOPS.load(Ordering::Acquire) == 0 {
            ensure("starting the spinner", now() < deadline)?;
            sleep_ms(1);
        }
        sleep_ms(20);
        // The most loops that the spinner made in LIMIT nanoseconds.
        let mut limit = 0;
        for _ in 0..WINDOWS {
            let (start, loops) = (now(), SPINNER_LOOPS.load(Ordering::Acquire));
            while now() < start + WINDOW {
                core::hint::spin_loop();
            }
            let (end, after) = (now(), SPINNER_LOOPS.load(Ordering::Acquire));
            let elapsed = (end - start) as u64;
            limit = limit.max((after - loops) * LIMIT / elapsed);
        }
        ensure("measuring the spinner", limit > 0)?;
        let mut quick = 0;
        for round in 1..=ROUNDS + 1 {
            let before = SPINNER_LOOPS.load(Ordering::Acquire);
            // SAFETY: the call takes no pointers.
            check("signalling the spinner", unsafe {
                sys::thread_signal(thread, USR1, round)
            })?;
            let deadline = in_time();
            while HANDLED.load(Ordering::Acquire) < round {
                ensure("running the handler", now() < deadline)?;
                core::hint::spin_loop();
            }
            let loops = SPINNER_LOOPS_HANDLED
                .load(Ordering::Acquire)
                .saturating_sub(before);
            if round > 1 && loops < limit {
                quick += 1;
            }
            // The next round starts once the spinner has returned from
            // the handler and spins again; after the last it ends.
            let loops = SPINNER_LOOPS.load(Ordering::Acquire);
            let deadline = in_time();
            while round <= ROUNDS && SPINNER_LOOPS.load(Ordering::Acquire) == loops {
                ensure("spinning again", now() < deadline)?;
                core::hint::spin_loop();
            }
        }
        ensure("running the handler at once", quick >= QUICK)
    })();
    // The spinner ends once the count reaches its rounds, and the action
    // is restored only then, so that no signal pending for it meets the
    // default action.
    HANDLED.fetch_max(ROUNDS + 1, Ordering::AcqRel);
    let ended = wait(thread, TERMINATED, in_time()).0 == 0;
    restore_usr1(&old)?;
    check("allowing every processor", set(s.thread, &[0xFF; 32]))?;
    rounds?;
    ensure("ending the spinner", ended)
}

/// Sleeps for a few microseconds again and again.
extern "sysv64" fn sleep_again_and_again(_: u64, _: u64) -> ! {
    loop {
        // SAFETY: the call takes no pointers.
        unsafe { sys::thread_sleep(now() + 5_000) };
    }
}

/// A thread on processor 1 that sleeps briefly again and again is killed
/// from processor 0, many times and at moments that vary, so that some
/// kills come while it is going to sleep. Each one ends, and none runs
/// again on the stack it ended with, which would fault in the kernel.
fn killed_as_it_sleeps(s: &Own) -> Result {
    const ROUNDS: i64 = 2000;
    let mut count = 0u32;
    // SAFETY: the pointer describes `count`.
    check("reading the processor count", unsafe {
        sys::system_get_cpu_count(&mut count)
    })?;
    if count < 2 {
        return Ok(());
    }
    let set = |thread: Handle, mask: &[u8]| {
        // SAFETY: the pointer and length describe `mask`.
        unsafe { sys::thread_set_affinity(thread, mask.as_ptr(), mask.len() as u64) }
    };
    check("keeping this thread on processor 0", set(s.thread, &[1]))?;
    // Each thread ends before the next starts, so they share a stack.
    let stack_size = 16 * 1024;
    let stack = create_memory(stack_size)?;
    let at = map(s.process, stack, stack_size)?;
    let name = b"sleeper";
    for round in 0..ROUNDS {
        let mut thread = 0;
        // SAFETY: the pointers describe `name` and `thread`.
        check("creating a sleeper", unsafe {
            sys::thread_create(s.process, name.as_ptr(), name.len() as u64, &mut thread)
        })?;
        check("moving it to processor 1", set(thread, &[2]))?;
        let entry = sleep_again_and_again as *const () as u64;
        // SAFETY: the call takes no pointers; the stack is mapped and
        // unused.
        check("starting it", unsafe {
            sys::thread_start(thread, entry, at + stack_size - 8, 0, 0)
        })?;
        let until = now() + 20_000 + round % 89 * 700;
        while now() < until {
            core::hint::spin_loop();
        }
        // SAFETY: the call takes no pointers.
        check("killing it", unsafe { sys::task_kill(thread) })?;
        ensure("ending it", wait(thread, TERMINATED, in_time()).0 == 0)?;
        // SAFETY: the handle is ours and not used again.
        check("closing it", unsafe { sys::handle_close(thread) })?;
    }
    check("allowing every processor", set(s.thread, &[0xFF; 32]))
}
