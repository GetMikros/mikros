// SPDX-License-Identifier: GPL-2.0
//! Calls into the shim that may wait long, for the disk or for
//! writeback, made on threads of the host layer's pool, so that the
//! executor's thread goes on serving the other connections meanwhile.
//! The thread that made a call queues a packet on the executor's port
//! once it is done, which wakes the request that waits for it.

use crate::host::{self, Spin};
use alloc::boxed::Box;
use alloc::sync::Arc;
use core::ffi::c_void;
use mikros_rt::exec::Reactor;
use mikros_rt::sys;

/// What `port_queue` fails with while the port holds as many packets as
/// it takes.
const PORT_FULL: sys::Status = -22;
/// How long a thread waits before it queues its packet again.
const RETRY_NS: i64 = 1_000_000;

type Work = Box<dyn FnOnce() + Send>;

extern "C" fn run(arg: *mut c_void) {
    // SAFETY: `blocking` passes a boxed `Work`, which only this thread
    // takes.
    let work = unsafe { Box::from_raw(arg.cast::<Work>()) };
    work();
}

/// Queues a packet with `key` on `port`, waiting while the port is full.
fn wake(port: sys::Handle, key: u64) {
    let mut packet = [0u8; 48];
    packet[..8].copy_from_slice(&key.to_le_bytes());
    // SAFETY: the pointer refers to a whole packet.
    while unsafe { sys::port_queue(port, packet.as_ptr()) } == PORT_FULL {
        let mut now = 0;
        // SAFETY: the out pointer is valid for the calls.
        unsafe {
            sys::clock_get_monotonic(&mut now);
            sys::thread_sleep(now + RETRY_NS);
        }
    }
}

/// Makes `call` on a thread of the host layer's pool, or on this thread
/// when the pool has none to give, and returns what it returned once it
/// has. A request that is dropped while its call runs leaves the call to
/// finish, as it owns all it uses.
pub async fn blocking<T: Send + 'static>(
    reactor: &Reactor,
    call: impl FnOnce() -> T + Send + 'static,
) -> T {
    let packets = reactor.packets();
    let (port, key) = (reactor.port(), packets.key());
    let done = Arc::new(Spin::new(None));
    let result = done.clone();
    let work: Work = Box::new(move || {
        *result.lock() = Some(call());
        wake(port, key);
    });
    let arg = Box::into_raw(Box::new(work));
    if !host::start_thread(run, arg.cast()) {
        // SAFETY: no thread took the work, which is ours again.
        let work = unsafe { Box::from_raw(arg) };
        work();
    }
    loop {
        if let Some(value) = done.lock().take() {
            return value;
        }
        packets.next().await;
    }
}
