//! The thread that serves the `Driver` channel once the volume is
//! mounted, so that requests get through while the server's thread
//! makes its calls on the partition.
//!
//! `Stop` wakes the server's thread through an event to unmount the
//! volume, and replies once it has. When the channel closes, the
//! server's thread is woken to end the program, as it did when it
//! served the channel itself.

use alloc::boxed::Box;
use core::sync::atomic::{AtomicU32, Ordering};
use mikros_rt::interfaces::mikros::base::Status;
use mikros_rt::interfaces::mikros::driver::{DriverRequest, DriverServer, DriverStopResponse};
use mikros_rt::sys;

const PERM_READ: u32 = 1 << 0;
const PERM_WRITE: u32 = 1 << 1;
const FOREVER: i64 = i64::MAX;
const STACK_SIZE: u64 = 128 << 10;
/// The event signal that wakes the server's thread when the driver ends.
pub const ENDING: u32 = 1 << 24;

/// How the driver ends.
pub const RUNNING: u32 = 0;
pub const STOPPED: u32 = 1;
pub const CLOSED: u32 = 2;

/// Where the two threads are as the driver ends: how it ends, the event
/// that tells the server's thread, and how far stopping went.
pub struct Ending {
    how: AtomicU32,
    event: AtomicU32,
    /// 1 once the volume is unmounted, 2 once `Stop` has its reply; both
    /// threads wait on it.
    stopping: AtomicU32,
}

pub static END: Ending = Ending {
    how: AtomicU32::new(RUNNING),
    event: AtomicU32::new(0),
    stopping: AtomicU32::new(0),
};

impl Ending {
    /// Creates the event that the server's thread waits on: false when
    /// there is none.
    pub fn init(&self) -> bool {
        let mut event = 0;
        // SAFETY: the out pointer is valid for the call.
        if unsafe { sys::event_create(0, &mut event) } != mikros_rt::OK {
            return false;
        }
        self.event.store(event, Ordering::Release);
        true
    }

    /// The event that is signaled with `ENDING` when the driver ends.
    pub fn event(&self) -> sys::Handle {
        self.event.load(Ordering::Acquire)
    }

    /// How the driver ends, `RUNNING` until it does.
    pub fn how(&self) -> u32 {
        self.how.load(Ordering::Acquire)
    }

    fn end(&self, how: u32) {
        self.how.store(how, Ordering::Release);
        // SAFETY: the call takes no pointers.
        unsafe { sys::object_signal(self.event(), 0, ENDING) };
    }

    /// Moves stopping on to `step`.
    fn advance(&self, step: u32) {
        self.stopping.store(step, Ordering::Release);
        let mut woken = 0;
        // SAFETY: the word stays valid while threads wait on it.
        unsafe { sys::futex_wake(self.stopping.as_ptr(), u32::MAX, &mut woken) };
    }

    /// Waits until stopping has reached `step`.
    fn wait(&self, step: u32) {
        loop {
            let at = self.stopping.load(Ordering::Acquire);
            if at >= step {
                return;
            }
            // SAFETY: the word stays valid while the thread waits on it.
            unsafe { sys::futex_wait(self.stopping.as_ptr(), at, FOREVER) };
        }
    }

    /// Says that the volume is unmounted, and waits until `Stop` has its
    /// reply, after which the program may end.
    pub fn unmounted(&self) {
        self.advance(1);
        self.wait(2);
    }
}

/// The driver's thread, made before it is needed, so that starting it
/// once the volume is mounted cannot run out of memory.
pub struct Thread {
    thread: sys::Handle,
    top: u64,
}

impl Thread {
    /// A thread of `process` that has not started: None when there is
    /// no memory for it.
    pub fn create(process: sys::Handle) -> Option<Thread> {
        let mut stack = 0;
        // SAFETY: the out pointer is valid for the call.
        if unsafe { sys::mem_create(STACK_SIZE, 0, &mut stack) } != mikros_rt::OK {
            return None;
        }
        let mut at = 0;
        // SAFETY: the out pointer is valid; the kernel picks a free range.
        let mapped = unsafe {
            sys::vm_map(
                process,
                PERM_READ | PERM_WRITE,
                0,
                stack,
                0,
                STACK_SIZE,
                &mut at,
            )
        };
        // The mapping keeps the stack's memory.
        // SAFETY: closing takes no pointers.
        unsafe { sys::handle_close(stack) };
        if mapped != mikros_rt::OK {
            return None;
        }
        let name = b"fat-driver";
        let mut thread = 0;
        // SAFETY: the name and the out pointer are valid for the call.
        let created =
            unsafe { sys::thread_create(process, name.as_ptr(), name.len() as u64, &mut thread) };
        (created == mikros_rt::OK).then_some(Thread {
            thread,
            top: at + STACK_SIZE,
        })
    }

    /// Serves the rest of the requests on `server` on the thread. False
    /// when it did not start.
    pub fn start(self, server: DriverServer) -> bool {
        let serving = Box::into_raw(Box::new(server));
        // SAFETY: the stack is mapped and unused, 8 bytes below a 16-byte
        // boundary as a function's entry expects; `serve` takes the box.
        let started = unsafe {
            sys::thread_start(
                self.thread,
                serve as *const () as u64,
                self.top - 8,
                serving as u64,
                0,
            )
        };
        // SAFETY: closing takes no pointers; the thread runs without it.
        unsafe { sys::handle_close(self.thread) };
        if started != mikros_rt::OK {
            // SAFETY: the thread did not start, so the box is still ours.
            drop(unsafe { Box::from_raw(serving) });
        }
        started == mikros_rt::OK
    }
}

extern "sysv64" fn serve(serving: u64, _: u64) -> ! {
    // SAFETY: `Thread::start` made the pointer from a box for this
    // thread alone.
    let server = *unsafe { Box::from_raw(serving as *mut DriverServer) };
    loop {
        let Ok(request) = server.next(FOREVER) else {
            END.end(CLOSED);
            break;
        };
        let _ = match request {
            DriverRequest::Start(_, responder) => responder.reply(&server, Err(Status::BadState)),
            DriverRequest::Stop(_, responder) => {
                END.end(STOPPED);
                END.wait(1);
                let _ = responder.reply(&server, DriverStopResponse {});
                END.advance(2);
                break;
            }
            // The driver does not take a new bus channel yet.
            DriverRequest::Reconnect(_, responder) => {
                responder.reply(&server, Err(Status::NotSupported))
            }
            DriverRequest::ResetChild(_, responder) => {
                responder.reply(&server, Err(Status::NotSupported))
            }
            DriverRequest::ConnectChild(_, responder) => {
                responder.reply(&server, Err(Status::NotSupported))
            }
        };
    }
    // The channel stays open until the program ends.
    core::mem::forget(server);
    // SAFETY: the call takes no pointers; nothing of this thread is used
    // after it.
    unsafe { sys::thread_exit() };
    unreachable!("thread_exit returned");
}
