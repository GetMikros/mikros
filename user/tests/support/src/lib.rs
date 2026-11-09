//! Support for the integration tests: calling the kernel with error
//! results, finding programs in the boot image, starting them, and
//! reporting the result to QEMU.

#![no_std]

use mikros_rt::sys::{self, Handle, Status};

pub mod elf;
pub use mikros_ustar as ustar;

/// The `isa-debug-exit` port the test boot image is run with.
pub const DEBUG_EXIT_PORT: u16 = 0xF4;
/// The value written to the port when every test passed. QEMU exits with
/// twice the value plus one.
pub const PASSED: u8 = 0x10;
pub const FAILED: u8 = 0x11;

pub const PERM_READ: u32 = 1 << 0;
pub const PERM_WRITE: u32 = 1 << 1;
pub const PERM_EXECUTE: u32 = 1 << 2;
pub const SPECIFIC: u32 = 1 << 8;
pub const KIND_IOPORT: u32 = 2;
pub const SHOULD_WAIT: Status = -22;
pub const PAGE_SIZE: u64 = 4096;

/// A failed check: what was being done and the status, if a call failed.
#[derive(Clone, Copy, Debug)]
pub struct Failure {
    pub what: &'static str,
    pub status: Status,
}

pub type Result<T = ()> = core::result::Result<T, Failure>;

/// Turns the status of a call into a result.
pub fn check(what: &'static str, status: Status) -> Result {
    if status == mikros_rt::OK {
        Ok(())
    } else {
        Err(Failure { what, status })
    }
}

/// Fails with `what` unless `condition` holds.
pub fn ensure(what: &'static str, condition: bool) -> Result {
    if condition {
        Ok(())
    } else {
        Err(Failure { what, status: 0 })
    }
}

/// Reaches the `isa-debug-exit` port through `root`, the `ROOT`
/// resource, for the process `process`.
pub fn open_exit_port(root: Handle, process: Handle) -> Result {
    let mut ports = 0;
    let port = u64::from(DEBUG_EXIT_PORT);
    // SAFETY: the out pointer is valid for the call.
    let status = unsafe { sys::resource_create(root, KIND_IOPORT, port, 4, 0, &mut ports) };
    check("creating the debug exit port resource", status)?;
    // SAFETY: the call takes no pointers.
    let status = unsafe { sys::process_ioports_enable(process, ports, port, 4) };
    check("enabling the debug exit port", status)
}

/// Stops QEMU with `value`, after `open_exit_port` succeeded.
///
/// QEMU may run the machine on for a while after the write, so the
/// calling thread waits for the stop instead of ending its process: when
/// the process is init, the kernel panics as it ends, and the panic would
/// reach the serial output before the stop.
pub fn exit_qemu(value: u8) -> ! {
    // SAFETY: the process may access the port, and writing it stops the
    // virtual machine.
    unsafe {
        core::arch::asm!("out dx, al", in("dx") DEBUG_EXIT_PORT, in("al") value, options(nomem, nostack));
    }
    loop {
        // SAFETY: the call takes no pointers.
        unsafe { sys::thread_sleep(i64::MAX) };
    }
}

/// Sleeps for `ms` milliseconds.
pub fn sleep_ms(ms: i64) {
    let mut now = 0;
    // SAFETY: the out pointer is valid for the call.
    unsafe { sys::clock_get_monotonic(&mut now) };
    // SAFETY: the call takes no pointers.
    unsafe { sys::thread_sleep(now + ms * 1_000_000) };
}

/// A message read from a channel into fixed buffers.
pub struct Message {
    pub bytes: [u8; 256],
    pub len: usize,
    pub handles: [Handle; 8],
    pub count: usize,
}

/// Reads a message from `channel`, polling until one arrives or `ms`
/// milliseconds have passed.
pub fn read_message(channel: Handle, ms: i64) -> Result<Message> {
    let mut m = Message {
        bytes: [0; 256],
        len: 0,
        handles: [0; 8],
        count: 0,
    };
    for _ in 0..ms {
        let (mut len, mut count) = (0u64, 0u64);
        // SAFETY: the pointers and capacities describe the buffers in `m`.
        let status = unsafe {
            sys::channel_read(
                channel,
                m.bytes.as_mut_ptr(),
                m.bytes.len() as u64,
                m.handles.as_mut_ptr(),
                m.handles.len() as u64,
                &mut len,
                &mut count,
            )
        };
        if status == SHOULD_WAIT {
            sleep_ms(1);
            continue;
        }
        check("reading a message", status)?;
        (m.len, m.count) = (len as usize, count as usize);
        return Ok(m);
    }
    Err(Failure {
        what: "waiting for a message",
        status: SHOULD_WAIT,
    })
}
