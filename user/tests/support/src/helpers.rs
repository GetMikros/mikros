//! Calls that the kernel's tests and the system tests share: the clock,
//! waits and deadlines, memory objects and mappings, and threads of the
//! test's own process.

use crate::{Context, PERM_READ, PERM_WRITE, Result, check};
use mikros_rt::sys::{self, Handle};

pub const SIGNALED: u32 = 1 << 3;
pub const TERMINATED: u32 = 1 << 4;
pub const USER0: u32 = 1 << 24;
pub const USER1: u32 = 1 << 25;
pub const TIMED_OUT: i64 = -21;
pub const CANCELED: i64 = -23;

/// How many times longer things may take in a fuzz build, whose kernel
/// checks every lock it takes and calls the coverage hook in every block,
/// which is slow under emulation.
#[cfg(mikros_fuzz)]
pub const SLOWER: i64 = 10;
#[cfg(not(mikros_fuzz))]
pub const SLOWER: i64 = 1;

pub fn now() -> i64 {
    let mut now = 0;
    // SAFETY: the pointer is valid for the call.
    unsafe { sys::clock_get_monotonic(&mut now) };
    now
}

/// A deadline for something that must happen, far enough off that a
/// slow or loaded machine still meets it.
pub fn in_time() -> i64 {
    within(1)
}

/// A deadline `seconds` from now for something that must happen, scaled
/// by `SLOWER`.
pub fn within(seconds: i64) -> i64 {
    now() + SLOWER * seconds * 1_000_000_000
}

/// Waits on `handle` for `signals` until `deadline`, returning the
/// status and the observed state.
pub fn wait(handle: Handle, signals: u32, deadline: i64) -> (i64, u32) {
    let mut observed = 0;
    // SAFETY: the pointer is valid for the call.
    let status = unsafe { sys::object_wait_one(handle, signals, deadline, &mut observed) };
    (status, observed)
}

/// Maps `len` bytes of `mem` read-write into `process` and returns the
/// address.
pub fn map(process: Handle, mem: Handle, len: u64) -> Result<u64> {
    let mut at = 0;
    // SAFETY: the out pointer is valid for the call.
    let status = unsafe { sys::vm_map(process, PERM_READ | PERM_WRITE, 0, mem, 0, len, &mut at) };
    check("mapping a memory object", status)?;
    Ok(at)
}

pub fn create_memory(len: u64) -> Result<Handle> {
    let mut mem = 0;
    // SAFETY: the out pointer is valid for the call.
    check("creating a memory object", unsafe {
        sys::mem_create(len, 0, &mut mem)
    })?;
    Ok(mem)
}

/// Starts a thread of this process at `entry` with `arg`.
pub fn spawn(
    s: &impl Context,
    entry: extern "sysv64" fn(u64, u64) -> !,
    arg: u64,
) -> Result<Handle> {
    spawn2(s, entry, arg, 0)
}

/// Starts a thread of this process at `entry` with both arguments.
pub fn spawn2(
    s: &impl Context,
    entry: extern "sysv64" fn(u64, u64) -> !,
    a: u64,
    b: u64,
) -> Result<Handle> {
    let stack_size = 16 * 1024;
    let stack = create_memory(stack_size)?;
    let at = map(s.process(), stack, stack_size)?;
    let mut thread = 0;
    let name = b"helper";
    // SAFETY: the pointers describe `name` and `thread`.
    let status =
        unsafe { sys::thread_create(s.process(), name.as_ptr(), name.len() as u64, &mut thread) };
    check("creating a thread", status)?;
    // SAFETY: the call takes no pointers; the stack is mapped and unused.
    let status =
        unsafe { sys::thread_start(thread, entry as *const () as u64, at + stack_size - 8, a, b) };
    check("starting the thread", status)?;
    Ok(thread)
}
