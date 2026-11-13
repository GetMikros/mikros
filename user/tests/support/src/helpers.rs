//! Calls that the kernel's tests and the system tests share: the clock,
//! waits and deadlines, memory objects and mappings, and threads of the
//! test's own process.

use crate::{Context, PAGE_SIZE, PERM_READ, PERM_WRITE, Result, check, ensure, sleep_ms};
use core::sync::atomic::{AtomicU64, Ordering};
use mikros_rt::sys::{self, Handle};

pub const READABLE: u32 = 1 << 0;
pub const SIGNALED: u32 = 1 << 3;
pub const TERMINATED: u32 = 1 << 4;
pub const SUSPENDED: u32 = 1 << 5;
pub const USER0: u32 = 1 << 24;
pub const USER1: u32 = 1 << 25;
pub const INVALID_ARGS: i64 = -10;
pub const BAD_HANDLE: i64 = -11;
pub const ACCESS_DENIED: i64 = -13;
pub const OUT_OF_RANGE: i64 = -14;
pub const BUFFER_TOO_SMALL: i64 = -15;
pub const BAD_STATE: i64 = -20;
pub const TIMED_OUT: i64 = -21;
pub const CANCELED: i64 = -23;
pub const PEER_CLOSED: i64 = -24;
pub const NOT_FOUND: i64 = -25;

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

/// Makes a call with `request` after a 4-byte placeholder ID, room for
/// `cap` reply bytes and `handle`, if any, and returns the status, the
/// sizes stored, the reply and the handle received.
pub fn call(
    channel: Handle,
    request: &[u8],
    handle: Option<Handle>,
    cap: u64,
    deadline: i64,
) -> (i64, [u64; 2], [u8; 64], Handle) {
    let mut bytes = [0u8; 64];
    bytes[4..4 + request.len()].copy_from_slice(request);
    let handles = [handle.unwrap_or(0)];
    let mut reply = [0u8; 64];
    let mut reply_handles = [0 as Handle; 1];
    let fields: [u64; 8] = [
        bytes.as_ptr() as u64,
        4 + request.len() as u64,
        handles.as_ptr() as u64,
        u64::from(handle.is_some()),
        reply.as_mut_ptr() as u64,
        cap,
        reply_handles.as_mut_ptr() as u64,
        1,
    ];
    let mut out = [0u64; 2];
    // SAFETY: the structure and the buffers it points to are valid.
    let status = unsafe {
        sys::channel_call(
            channel,
            0,
            deadline,
            fields.as_ptr().cast(),
            out.as_mut_ptr().cast(),
        )
    };
    (status, out, reply, reply_handles[0])
}

/// The information record of `topic` for `handle`.
pub fn info<const N: usize>(handle: Handle, topic: u32) -> Result<[u8; N]> {
    let mut record = [0u8; N];
    let mut len = 0u64;
    // SAFETY: the pointers and length describe `record` and `len`.
    let status =
        unsafe { sys::object_get_info(handle, topic, record.as_mut_ptr(), N as u64, &mut len) };
    check("reading information", status)?;
    ensure("checking the record size", len == N as u64)?;
    Ok(record)
}

pub fn signal(event: Handle, clear: u32, set: u32) -> Result {
    // SAFETY: the call takes no pointers.
    check("signalling", unsafe {
        sys::object_signal(event, clear, set)
    })
}

/// Whether `needle` occurs in `haystack`.
pub fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack.windows(needle.len()).any(|w| w == needle)
}

pub fn soon() -> i64 {
    now() + 5_000_000
}

/// Waits for a packet on `port` until `deadline`. The buffer starts
/// filled with 0xAA, so that bytes the kernel leaves alone show.
pub fn take_packet(port: Handle, deadline: i64) -> (i64, [u8; 48]) {
    let mut packet = [0xAAu8; 48];
    // SAFETY: the pointer describes `packet`.
    let status = unsafe { sys::port_wait(port, deadline, packet.as_mut_ptr()) };
    (status, packet)
}

pub fn u32_at(record: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(record[at..at + 4].try_into().unwrap())
}

pub fn u64_at(record: &[u8], at: usize) -> u64 {
    u64::from_le_bytes(record[at..at + 8].try_into().unwrap())
}

/// The result a helper thread leaves for the test.
pub static HELPER: AtomicU64 = AtomicU64::new(0);

pub extern "sysv64" fn wait_forever(event: u64, _: u64) -> ! {
    let (status, _) = wait(event as Handle, USER0, i64::MAX);
    HELPER.store(status as u64, Ordering::Release);
    // SAFETY: the call takes no pointers.
    unsafe { sys::thread_exit() };
    unreachable!("thread_exit returned");
}

pub fn stream_io(stream: Handle, write: bool, buf: &mut [u8]) -> (i64, u64) {
    let mut actual = 0;
    // SAFETY: the pointers describe `buf` and `actual`.
    let status = unsafe {
        if write {
            sys::stream_write(stream, buf.as_ptr(), buf.len() as u64, &mut actual)
        } else {
            sys::stream_read(stream, buf.as_mut_ptr(), buf.len() as u64, &mut actual)
        }
    };
    (status, actual)
}

/// Maps the page of `mem` read-only into this process.
pub fn map_read_only(s: &impl Context, mem: Handle) -> Result<u64> {
    let mut at = 0;
    // SAFETY: the out pointer is valid for the call.
    let status = unsafe { sys::vm_map(s.process(), PERM_READ, 0, mem, 0, PAGE_SIZE, &mut at) };
    check("mapping read-only", status)?;
    Ok(at)
}

pub fn futex_wake(addr: u64, count: u32) -> Result<u32> {
    let mut woken = 0;
    // SAFETY: the pointers are valid for the call.
    check("waking", unsafe {
        sys::futex_wake(addr as *const u32, count, &mut woken)
    })?;
    Ok(woken)
}

/// Reads the next message of `channel` into buffers of `cap` bytes and
/// `handles_cap` handles, and returns the status and the two sizes.
pub fn read_sizes(channel: Handle, cap: usize, handles_cap: usize) -> (i64, u64, u64) {
    let (mut bytes, mut handles) = ([0u8; 16], [0 as Handle; 2]);
    let (mut len, mut count) = (0, 0);
    // SAFETY: the capacities are at most the sizes of the buffers.
    let status = unsafe {
        sys::channel_read(
            channel,
            bytes.as_mut_ptr(),
            cap.min(bytes.len()) as u64,
            handles.as_mut_ptr(),
            handles_cap.min(handles.len()) as u64,
            &mut len,
            &mut count,
        )
    };
    (status, len, count)
}

/// Waits until `thread` is blocked, or fails after `in_time`.
pub fn wait_until_blocked(thread: Handle) -> Result {
    wait_for_state(thread, STATE_BLOCKED)
}

/// Waits until `thread` is in state `state`, or fails after `in_time`.
pub fn wait_for_state(thread: Handle, state: u32) -> Result {
    let deadline = in_time();
    while thread_state(thread)? != state {
        ensure("reaching the thread state", now() < deadline)?;
        sleep_ms(1);
    }
    Ok(())
}

pub fn protect(s: &impl Context, perms: u32, addr: u64, len: u64) -> i64 {
    // SAFETY: the call takes no pointers.
    unsafe { sys::vm_protect(s.process(), perms, addr, len) }
}

pub fn seal(mem: Handle, seals: u32) -> i64 {
    // SAFETY: the call takes no pointers.
    unsafe { sys::mem_seal(mem, seals) }
}

pub fn set_size(mem: Handle, size: u64) -> i64 {
    // SAFETY: the call takes no pointers.
    unsafe { sys::mem_set_size(mem, size) }
}

pub fn mem_write_at(mem: Handle, offset: u64, bytes: &[u8]) -> Result {
    // SAFETY: the pointer and length describe `bytes`.
    check("writing", unsafe {
        sys::mem_write(mem, offset, bytes.as_ptr(), bytes.len() as u64)
    })
}

pub fn mem_read_at<const N: usize>(mem: Handle, offset: u64) -> Result<[u8; N]> {
    let mut bytes = [0u8; N];
    // SAFETY: the pointer and length describe `bytes`.
    check("reading", unsafe {
        sys::mem_read(mem, offset, bytes.as_mut_ptr(), N as u64)
    })?;
    Ok(bytes)
}

pub fn close(handle: Handle) -> Result {
    // SAFETY: the call takes no pointers.
    check("closing", unsafe { sys::handle_close(handle) })
}

pub const STATE_BLOCKED: u32 = 2;

/// The state of `thread`.
pub fn thread_state(thread: Handle) -> Result<u32> {
    Ok(u32_at(&info::<0x18>(thread, 3)?, 0))
}

/// The object ID of the object of `handle`.
pub fn koid(handle: Handle) -> Result<u64> {
    Ok(u64_at(&info::<0x18>(handle, 1)?, 0))
}

pub fn read_memory(process: Handle, addr: u64, buf: &mut [u8]) -> i64 {
    // SAFETY: the pointer and length describe `buf`.
    unsafe { sys::process_read_memory(process, addr, buf.as_mut_ptr(), buf.len() as u64) }
}

pub fn write_memory(process: Handle, addr: u64, bytes: &[u8]) -> i64 {
    // SAFETY: the pointer and length describe `bytes`.
    unsafe { sys::process_write_memory(process, addr, bytes.as_ptr(), bytes.len() as u64) }
}
