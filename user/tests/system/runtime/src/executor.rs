//! The runtime's executor, waiting on one port.

use alloc::boxed::Box;
use alloc::rc::Rc;
use core::cell::Cell;
use core::future::{Future, poll_fn};
use core::sync::atomic::{AtomicBool, Ordering};
use core::task::{Poll, Waker};
use mikros_rt::exec::Executor;
use mikros_rt::sys::{self, Handle};
use mikros_test::{Failure, Result, check, ensure};
use mikros_test::{Own, USER0, now, spawn2};

fn new_event() -> Result<Handle> {
    let mut event = 0;
    // SAFETY: the out pointer is valid for the call.
    check("creating an event", unsafe {
        sys::event_create(0, &mut event)
    })?;
    Ok(event)
}

fn close(handle: Handle) {
    // SAFETY: closing takes no pointers.
    unsafe { sys::handle_close(handle) };
}

static WOKEN_ELSEWHERE: AtomicBool = AtomicBool::new(false);

/// Wakes the boxed waker at `waker` from this thread, then ends.
extern "sysv64" fn wake_from_thread(waker: u64, _: u64) -> ! {
    // SAFETY: the test boxed the waker for this thread alone.
    let waker = unsafe { Box::from_raw(waker as *mut Waker) };
    WOKEN_ELSEWHERE.store(true, Ordering::Release);
    waker.wake();
    // SAFETY: the call takes no pointers.
    unsafe { sys::thread_exit() };
    unreachable!("thread_exit returned");
}

/// Futures for object signals, channel messages, timers and
/// timelines complete when their packets arrive; spawned tasks run
/// beside the main future; a waker used on another thread ends the
/// wait on the port; and a dropped wait is cancelled.
pub fn executor_waits_on_objects(s: &Own) -> Result {
    let mut ex = Executor::new().map_err(|status| Failure {
        what: "creating an executor",
        status,
    })?;
    let reactor = ex.reactor().clone();

    // A spawned task waits for an event that the main future signals.
    let event = new_event()?;
    let done = Rc::new(Cell::new(false));
    let (r, d) = (reactor.clone(), done.clone());
    ex.spawner().spawn(async move {
        let observed = r.signals(event, USER0).await;
        d.set(observed.is_ok_and(|o| o & USER0 != 0));
    });
    let signalled = ex.block_on(async {
        // SAFETY: the call takes no pointers.
        unsafe { sys::object_signal(event, 0, USER0) };
        reactor.signals(event, USER0).await
    });
    ensure(
        "observing the event",
        signalled.is_ok_and(|o| o & USER0 != 0),
    )?;
    ex.block_on(poll_fn(|cx| {
        if done.get() {
            Poll::Ready(())
        } else {
            cx.waker().wake_by_ref();
            Poll::Pending
        }
    }));
    ensure("running the spawned task", done.get())?;

    // A dropped wait leaves nothing behind that a later wait trips on.
    let other = new_event()?;
    drop(reactor.signals(other, USER0));
    let mut pending = Box::pin(reactor.signals(other, USER0));
    let first = ex.block_on(poll_fn(|cx| {
        Poll::Ready(pending.as_mut().poll(cx).is_pending())
    }));
    drop(pending);
    ensure("waiting before the signal", first)?;
    // SAFETY: the call takes no pointers.
    unsafe { sys::object_signal(other, 0, USER0) };
    ensure(
        "waiting after a cancelled wait",
        ex.block_on(reactor.signals(other, USER0)).is_ok(),
    )?;

    // A channel becomes readable.
    let (mut a, mut b) = (0, 0);
    // SAFETY: the out pointers are valid for the call.
    check("creating a channel", unsafe {
        sys::channel_create(0, &mut a, &mut b)
    })?;
    // SAFETY: the pointer describes one byte; no handles are sent.
    check("writing", unsafe {
        sys::channel_write(a, [1u8].as_ptr(), 1, core::ptr::null(), 0)
    })?;
    ensure(
        "reading a message",
        ex.block_on(reactor.readable(b)).is_ok(),
    )?;

    // A timer ends no earlier than its deadline.
    let deadline = now() + 20_000_000;
    ensure(
        "sleeping",
        ex.block_on(reactor.sleep_until(deadline)).is_ok(),
    )?;
    ensure("sleeping until the deadline", now() >= deadline)?;

    // A timeline reaches a value.
    let mut timeline = 0;
    // SAFETY: the out pointer is valid for the call.
    check("creating a timeline", unsafe {
        sys::timeline_create(1, 0, &mut timeline)
    })?;
    // SAFETY: the call takes no pointers.
    check("signalling the timeline", unsafe {
        sys::timeline_signal(timeline, 3)
    })?;
    let value = ex.block_on(reactor.timeline(timeline, 2));
    ensure("reaching the timeline value", value == Ok(3))?;

    // A waker used on another thread ends the wait on the port.
    WOKEN_ELSEWHERE.store(false, Ordering::Release);
    let mut started = false;
    let mut failed = None;
    ex.block_on(poll_fn(|cx| {
        if WOKEN_ELSEWHERE.load(Ordering::Acquire) {
            return Poll::Ready(());
        }
        if !started {
            started = true;
            let waker = Box::into_raw(Box::new(cx.waker().clone())) as u64;
            if let Err(e) = spawn2(s, wake_from_thread, waker, 0) {
                failed = Some(e);
                return Poll::Ready(());
            }
        }
        Poll::Pending
    }));
    if let Some(e) = failed {
        return Err(e);
    }
    ensure(
        "waking from another thread",
        WOKEN_ELSEWHERE.load(Ordering::Acquire),
    )?;

    for h in [event, other, a, b, timeline] {
        close(h);
    }
    Ok(())
}
