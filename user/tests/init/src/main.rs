//! The init of the test boot image: it runs the integration tests,
//! logs each result, and stops QEMU through `isa-debug-exit` with the
//! overall result.

#![no_std]
#![no_main]

use core::sync::atomic::{AtomicU64, Ordering};
use mikros_rt::sys::{self, Handle};
use mikros_rt::{Startup, log};
use mikros_test::{
    FAILED, PAGE_SIZE, PASSED, PERM_READ, PERM_WRITE, Result, check, elf, ensure, exit_qemu,
    open_exit_port, read_message, sleep_ms, ustar,
};

mikros_rt::main!(main);

/// Maps `len` bytes of `mem` read-write into `process` and returns the
/// address.
fn map(process: Handle, mem: Handle, len: u64) -> Result<u64> {
    let mut at = 0;
    // SAFETY: the out pointer is valid for the call.
    let status = unsafe { sys::vm_map(process, PERM_READ | PERM_WRITE, 0, mem, 0, len, &mut at) };
    check("mapping a memory object", status)?;
    Ok(at)
}

fn create_memory(len: u64) -> Result<Handle> {
    let mut mem = 0;
    // SAFETY: the out pointer is valid for the call.
    check("creating a memory object", unsafe {
        sys::mem_create(len, 0, &mut mem)
    })?;
    Ok(mem)
}

/// A memory object mapped into this process shows the same bytes
/// through the mapping and through `mem_read`.
fn memory_objects(s: &Startup) -> Result {
    let mem = create_memory(2 * PAGE_SIZE)?;
    let at = map(s.process, mem, 2 * PAGE_SIZE)?;
    // SAFETY: the mapping is 8192 bytes long, readable and writable.
    unsafe { ((at + PAGE_SIZE + 8) as *mut u64).write_volatile(0x1234_5678_9ABC) };
    let mut back = [0u8; 8];
    // SAFETY: the pointer and length describe `back`.
    let status = unsafe { sys::mem_read(mem, PAGE_SIZE + 8, back.as_mut_ptr(), 8) };
    check("reading the object", status)?;
    ensure(
        "comparing the bytes",
        u64::from_le_bytes(back) == 0x1234_5678_9ABC,
    )?;
    // SAFETY: the call takes no pointers.
    check("unmapping", unsafe {
        sys::vm_unmap(s.process, at, 2 * PAGE_SIZE)
    })?;
    // SAFETY: the call takes no pointers.
    check("closing the object", unsafe { sys::handle_close(mem) })
}

/// Two mappings of one object stay in step when a page read through one
/// as zeros gets a frame through the other, is decommitted, and is
/// written by the kernel.
fn shared_mappings(s: &Startup) -> Result {
    let mem = create_memory(PAGE_SIZE)?;
    let a = map(s.process, mem, PAGE_SIZE)? as *mut u64;
    let b = map(s.process, mem, PAGE_SIZE)? as *mut u64;
    // SAFETY: both mappings are a readable and writable page of `mem`.
    unsafe {
        ensure("reading zeros through the first", a.read_volatile() == 0)?;
        b.write_volatile(0x5eed);
        ensure(
            "seeing the write through the first",
            a.read_volatile() == 0x5eed,
        )?;
    }
    // SAFETY: the call takes no pointers.
    check("decommitting", unsafe { sys::mem_op(mem, 2, 0, PAGE_SIZE) })?;
    // SAFETY: as above.
    unsafe {
        ensure("reading zeros after decommit", a.read_volatile() == 0)?;
        ensure("reading zeros through both", b.read_volatile() == 0)?;
    }
    mem_write_at(mem, 0, &7u64.to_le_bytes())?;
    // SAFETY: as above.
    unsafe {
        ensure(
            "seeing the kernel write",
            a.read_volatile() == 7 && b.read_volatile() == 7,
        )?;
    }
    for at in [a, b] {
        // SAFETY: the call takes no pointers.
        check("unmapping", unsafe {
            sys::vm_unmap(s.process, at as u64, PAGE_SIZE)
        })?;
    }
    // SAFETY: the call takes no pointers.
    check("closing the object", unsafe { sys::handle_close(mem) })
}

/// `system_get_random` fills buffers up to 4096 bytes and refuses more.
fn random_bytes(_: &Startup) -> Result {
    let mut a = [0u8; 4096];
    let mut b = [0u8; 64];
    // SAFETY: the pointers and lengths describe the arrays.
    unsafe {
        check(
            "filling 4096 bytes",
            sys::system_get_random(a.as_mut_ptr(), 4096),
        )?;
        check(
            "filling 64 bytes",
            sys::system_get_random(b.as_mut_ptr(), 64),
        )?;
        ensure(
            "refusing more than 4096 bytes",
            sys::system_get_random(a.as_mut_ptr(), 4097) == -14,
        )?;
        ensure(
            "filling nothing",
            sys::system_get_random(b.as_mut_ptr(), 0) == 0,
        )?;
    }
    // Two draws of 64 bytes, or a draw of all zeros, mean no generator.
    ensure("drawing different bytes", a[..64] != b && b != [0; 64])?;
    ensure("filling every byte", a[4032..] != [0; 64])
}

/// Writable and executable mappings are refused unless the process has
/// the `ALLOW_WRITE_EXECUTE` policy, which only a `NEW` process can get.
fn write_execute_policy(s: &Startup) -> Result {
    const RWX: u32 = PERM_READ | PERM_WRITE | 4;
    let name = b"policy";
    let mut process = 0;
    // SAFETY: the pointers describe `name` and `process`.
    check("creating a process", unsafe {
        sys::process_create(s.budget, name.as_ptr(), name.len() as u64, &mut process)
    })?;
    let mem = create_memory(PAGE_SIZE)?;
    let mut at = 0;
    // SAFETY: the out pointer is valid for each call; the others take no
    // pointers.
    unsafe {
        let map = |p: Handle, at: &mut u64| sys::vm_map(p, RWX, 0, mem, 0, PAGE_SIZE, at);
        ensure(
            "refusing a policy bit 2",
            sys::process_set_policy(process, s.root_resource, 4) == -10,
        )?;
        ensure(
            "refusing 32-bit code",
            sys::process_set_policy(process, s.root_resource, 2) == -2,
        )?;
        ensure(
            "refusing W+X without the policy",
            map(process, &mut at) == -10,
        )?;
        check(
            "setting the policy",
            sys::process_set_policy(process, s.root_resource, 1),
        )?;
        check("mapping W+X with the policy", map(process, &mut at))?;
        check(
            "protecting W+X with the policy",
            sys::vm_protect(process, RWX, at, PAGE_SIZE),
        )?;
        ensure(
            "refusing W+X in this process",
            map(s.process, &mut at) == -10,
        )?;
        ensure(
            "refusing a policy once started",
            sys::process_set_policy(s.process, s.root_resource, 1) == -20,
        )?;
        check("closing the process", sys::handle_close(process))?;
        check("closing the object", sys::handle_close(mem))
    }
}

static THREAD_RAN: AtomicU64 = AtomicU64::new(0);

extern "sysv64" fn thread_entry(value: u64, _arg2: u64) -> ! {
    THREAD_RAN.store(value, Ordering::Release);
    // SAFETY: the call takes no pointers.
    unsafe { sys::thread_exit() };
    unreachable!("thread_exit returned");
}

/// A second thread of this process runs and exits.
fn threads(s: &Startup) -> Result {
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

/// A second process started from the boot image answers a message and
/// sends back the memory object handle that came with it.
/// Starts the program `name` of the boot image, under `tests/`, as a new
/// process with one end of a new channel, and returns the process and
/// the other end.
fn start_program(s: &Startup, name: &str) -> Result<(Handle, Handle)> {
    start_program_with(s, name, 0, |_| Ok(()))
}

/// Like `start_program`, passing `arg2` to the program and calling
/// `setup` with the process handle before the process starts.
fn start_program_with(
    s: &Startup,
    name: &str,
    arg2: u64,
    setup: impl FnOnce(Handle) -> Result,
) -> Result<(Handle, Handle)> {
    let image_size = s.boot_image_size.next_multiple_of(PAGE_SIZE);
    let mut at = 0;
    // SAFETY: the out pointer is valid for the call.
    let status = unsafe {
        sys::vm_map(
            s.process,
            PERM_READ,
            0,
            s.boot_image,
            0,
            image_size,
            &mut at,
        )
    };
    check("mapping the boot image", status)?;
    // SAFETY: the boot image is mapped readable at `at`.
    let image = unsafe { core::slice::from_raw_parts(at as *const u8, s.boot_image_size as usize) };
    let mut path = [0u8; 64];
    path[..6].copy_from_slice(b"tests/");
    path[6..6 + name.len()].copy_from_slice(name.as_bytes());
    let program = ustar::find(image, &path[..6 + name.len()]);
    ensure("finding the program", program.is_some())?;

    let (mut process, mut thread) = (0, 0);
    let name = name.as_bytes();
    // SAFETY: the pointers describe `name` and `process`.
    let status =
        unsafe { sys::process_create(s.budget, name.as_ptr(), name.len() as u64, &mut process) };
    check("creating a process", status)?;
    // SAFETY: the pointers describe `name` and `thread`.
    let status =
        unsafe { sys::thread_create(process, name.as_ptr(), name.len() as u64, &mut thread) };
    check("creating its thread", status)?;
    let loaded = elf::load(process, program.unwrap_or_default())?;
    // SAFETY: the call takes no pointers.
    check("unmapping the boot image", unsafe {
        sys::vm_unmap(s.process, at, image_size)
    })?;
    let (mut ours, mut theirs) = (0, 0);
    // SAFETY: the out pointers are valid for the call.
    check("creating a channel", unsafe {
        sys::channel_create(0, &mut ours, &mut theirs)
    })?;
    setup(process)?;
    // SAFETY: the call takes no pointers.
    let status =
        unsafe { sys::process_start(process, thread, loaded.entry, loaded.stack, theirs, arg2) };
    check("starting the process", status)?;
    Ok((process, ours))
}

fn processes_and_channels(s: &Startup) -> Result {
    let (process, ours) = start_program(s, "channel-echo")?;

    let mem = create_memory(PAGE_SIZE)?;
    let text = b"shared bytes 123";
    // SAFETY: the pointer and length describe `text`.
    check("filling the object", unsafe {
        sys::mem_write(mem, 0, text.as_ptr(), 16)
    })?;
    let request = b"hello";
    // SAFETY: the pointers and lengths describe `request` and `mem`.
    let status = unsafe { sys::channel_write(ours, request.as_ptr(), 5, &mem, 1) };
    check("sending the request", status)?;
    let reply = read_message(ours, 5000)?;
    ensure(
        "checking the reply",
        reply.bytes[..reply.len] == *b"echo: shared bytes 123",
    )?;
    ensure("checking the returned handle count", reply.count == 1)?;
    let mut back = [0u8; 16];
    // SAFETY: the pointer and length describe `back`.
    let status = unsafe { sys::mem_read(reply.handles[0], 0, back.as_mut_ptr(), 16) };
    check("reading the returned object", status)?;
    ensure("checking the returned object", back == *text)?;
    // The process ends once it has answered.
    for _ in 0..1000 {
        let info = info::<0x20>(process, 2)?;
        if u32_at(&info, 0x08) == 2 {
            ensure(
                "checking the exit code",
                i64::from_le_bytes(info[..8].try_into().unwrap()) == 0,
            )?;
            return ensure("checking that no thread is left", u32_at(&info, 0x0C) == 0);
        }
        sleep_ms(1);
    }
    ensure("waiting for the process to end", false)
}

/// The information record of `topic` for `handle`.
fn info<const N: usize>(handle: Handle, topic: u32) -> Result<[u8; N]> {
    let mut record = [0u8; N];
    let mut len = 0u64;
    // SAFETY: the pointers and length describe `record` and `len`.
    let status =
        unsafe { sys::object_get_info(handle, topic, record.as_mut_ptr(), N as u64, &mut len) };
    check("reading information", status)?;
    ensure("checking the record size", len == N as u64)?;
    Ok(record)
}

fn u32_at(record: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(record[at..at + 4].try_into().unwrap())
}

/// The process and thread records describe the running test.
fn info_topics(s: &Startup) -> Result {
    let process = info::<0x20>(s.process, 2)?;
    ensure("checking the process state", u32_at(&process, 0x08) == 1)?;
    ensure("checking the thread count", u32_at(&process, 0x0C) >= 1)?;
    ensure(
        "checking the process CPU time",
        i64::from_le_bytes(process[0x18..0x20].try_into().unwrap()) > 0,
    )?;
    let thread = info::<0x18>(s.thread, 3)?;
    ensure("checking the thread state", u32_at(&thread, 0x00) == 1)?;
    ensure("checking the priority", u32_at(&thread, 0x04) == 16)?;
    ensure("checking the exception type", u32_at(&thread, 0x08) == 0)?;
    ensure(
        "checking the thread CPU time",
        i64::from_le_bytes(thread[0x10..0x18].try_into().unwrap()) > 0,
    )
}

const SIGNALED: u32 = 1 << 3;
const TERMINATED: u32 = 1 << 4;
const USER0: u32 = 1 << 24;
const USER1: u32 = 1 << 25;
const TIMED_OUT: i64 = -21;
const CANCELED: i64 = -23;

fn now() -> i64 {
    let mut now = 0;
    // SAFETY: the pointer is valid for the call.
    unsafe { sys::clock_get_monotonic(&mut now) };
    now
}

/// Waits on `handle` for `signals` until `deadline`, returning the
/// status and the observed state.
fn wait(handle: Handle, signals: u32, deadline: i64) -> (i64, u32) {
    let mut observed = 0;
    // SAFETY: the pointer is valid for the call.
    let status = unsafe { sys::object_wait_one(handle, signals, deadline, &mut observed) };
    (status, observed)
}

/// The result a helper thread leaves for the test.
static HELPER: AtomicU64 = AtomicU64::new(0);

extern "sysv64" fn signal_later(event: u64, _: u64) -> ! {
    sleep_ms(20);
    // SAFETY: the call takes no pointers.
    unsafe { sys::object_signal(event as Handle, 0, USER1) };
    // SAFETY: the call takes no pointers.
    unsafe { sys::thread_exit() };
    unreachable!("thread_exit returned");
}

extern "sysv64" fn wait_forever(event: u64, _: u64) -> ! {
    let (status, _) = wait(event as Handle, USER0, i64::MAX);
    HELPER.store(status as u64, Ordering::Release);
    // SAFETY: the call takes no pointers.
    unsafe { sys::thread_exit() };
    unreachable!("thread_exit returned");
}

/// Starts a thread of this process at `entry` with `arg`.
fn spawn(s: &Startup, entry: extern "sysv64" fn(u64, u64) -> !, arg: u64) -> Result<Handle> {
    let stack_size = 16 * 1024;
    let stack = create_memory(stack_size)?;
    let at = map(s.process, stack, stack_size)?;
    let mut thread = 0;
    let name = b"helper";
    // SAFETY: the pointers describe `name` and `thread`.
    let status =
        unsafe { sys::thread_create(s.process, name.as_ptr(), name.len() as u64, &mut thread) };
    check("creating a thread", status)?;
    let entry = entry as *const () as u64;
    // SAFETY: the call takes no pointers; the stack is mapped and unused.
    let status = unsafe { sys::thread_start(thread, entry, at + stack_size - 8, arg, 0) };
    check("starting the thread", status)?;
    Ok(thread)
}

/// Events, waits on them and on threads, and cancelled waits.
fn events_and_waits(s: &Startup) -> Result {
    let mut event = 0;
    // SAFETY: the pointer is valid for the call.
    check("creating an event", unsafe {
        sys::event_create(0, &mut event)
    })?;
    let (status, observed) = wait(event, SIGNALED, now() + 5_000_000);
    ensure("timing out", status == TIMED_OUT && observed == 0)?;
    // SAFETY: the call takes no pointers.
    check("signalling", unsafe {
        sys::object_signal(event, 0, SIGNALED | USER0)
    })?;
    let (status, observed) = wait(event, SIGNALED, i64::MAX);
    ensure(
        "seeing the signal",
        status == 0 && observed == SIGNALED | USER0,
    )?;
    // SAFETY: the call takes no pointers.
    let bad = unsafe { sys::object_signal(event, 0, 1) };
    ensure("refusing other bits", bad == -10)?;
    // SAFETY: the call takes no pointers.
    let wrong = unsafe { sys::object_signal(s.process, 0, USER0) };
    ensure("refusing other types", wrong == -12)?;
    // SAFETY: the call takes no pointers.
    check("clearing", unsafe {
        sys::object_signal(event, SIGNALED | USER0, 0)
    })?;

    let helper = spawn(s, signal_later, u64::from(event))?;
    let (status, observed) = wait(event, USER1, now() + 2_000_000_000);
    ensure(
        "being woken by another thread",
        status == 0 && observed == USER1,
    )?;
    let (status, _) = wait(helper, TERMINATED, now() + 2_000_000_000);
    ensure("seeing the thread end", status == 0)?;

    let mut copy = 0;
    // SAFETY: the pointer is valid for the call.
    let status = unsafe { sys::handle_duplicate(event, 0x8000_0000, &mut copy) };
    check("duplicating the event", status)?;
    let waiter = spawn(s, wait_forever, u64::from(copy))?;
    sleep_ms(20);
    // SAFETY: the call takes no pointers.
    check("closing the handle", unsafe { sys::handle_close(copy) })?;
    let (status, _) = wait(waiter, TERMINATED, now() + 2_000_000_000);
    ensure("seeing the waiter end", status == 0)?;
    ensure(
        "cancelling the wait",
        HELPER.load(Ordering::Acquire) as i64 == CANCELED,
    )
}

/// Timers set SIGNALED at their deadline, at once for a past one, and
/// not after being cancelled.
fn timers(_: &Startup) -> Result {
    let mut timer = 0;
    // SAFETY: the pointer is valid for the call.
    check("creating a timer", unsafe {
        sys::timer_create(0, &mut timer)
    })?;
    let start = now();
    // SAFETY: the call takes no pointers.
    check("setting the timer", unsafe {
        sys::timer_set(timer, start + 10_000_000)
    })?;
    let (status, _) = wait(timer, SIGNALED, start + 2_000_000_000);
    ensure("firing", status == 0 && now() - start >= 10_000_000)?;
    // SAFETY: the call takes no pointers.
    check("setting a past deadline", unsafe {
        sys::timer_set(timer, 0)
    })?;
    let (status, _) = wait(timer, SIGNALED, 0);
    ensure("firing at once", status == 0)?;
    // SAFETY: the call takes no pointers.
    check("setting again", unsafe {
        sys::timer_set(timer, now() + 5_000_000)
    })?;
    // SAFETY: the call takes no pointers.
    check("cancelling", unsafe { sys::timer_cancel(timer) })?;
    let (status, observed) = wait(timer, SIGNALED, now() + 30_000_000);
    ensure(
        "staying clear after cancel",
        status == TIMED_OUT && observed == 0,
    )
}

const KIND_PRIORITY: u32 = 5;
const INVALID: Handle = 0;

fn set_priority(thread: Handle, resource: Handle, priority: u32) -> i64 {
    // SAFETY: the call takes no pointers.
    unsafe { sys::thread_set_priority(thread, resource, priority) }
}

/// Priorities up to 16 need no resource, higher ones a resource that
/// covers them.
fn priorities(s: &Startup) -> Result {
    check("lowering", set_priority(s.thread, INVALID, 10))?;
    let thread = info::<0x18>(s.thread, 3)?;
    ensure("reading the priority", u32_at(&thread, 0x04) == 10)?;
    ensure(
        "needing a resource",
        set_priority(s.thread, INVALID, 20) == -13,
    )?;
    ensure(
        "refusing 32",
        set_priority(s.thread, s.root_resource, 32) == -10,
    )?;
    check(
        "raising with the root resource",
        set_priority(s.thread, s.root_resource, 31),
    )?;
    let mut range = 0;
    // SAFETY: the pointer is valid for the call.
    let status =
        unsafe { sys::resource_create(s.root_resource, KIND_PRIORITY, 0, 19, 0, &mut range) };
    check("creating a priority resource", status)?;
    check(
        "raising within the range",
        set_priority(s.thread, range, 18),
    )?;
    ensure(
        "refusing above the range",
        set_priority(s.thread, range, 19) == -13,
    )?;
    check("going back to 16", set_priority(s.thread, INVALID, 16))
}

/// Waits for a packet on `port` until `deadline`.
fn take_packet(port: Handle, deadline: i64) -> (i64, [u8; 48]) {
    let mut packet = [0u8; 48];
    // SAFETY: the pointer describes `packet`.
    let status = unsafe { sys::port_wait(port, deadline, packet.as_mut_ptr()) };
    (status, packet)
}

fn soon() -> i64 {
    now() + 5_000_000
}

/// A deadline for something that must happen, far enough off that a
/// slow or loaded machine still meets it.
fn in_time() -> i64 {
    now() + 1_000_000_000
}

/// Waits until `thread` is blocked, or fails after `in_time`.
fn wait_until_blocked(thread: Handle) -> Result {
    wait_for_state(thread, STATE_BLOCKED)
}

/// Waits until `thread` is in state `state`, or fails after `in_time`.
fn wait_for_state(thread: Handle, state: u32) -> Result {
    let deadline = in_time();
    while thread_state(thread)? != state {
        ensure("reaching the thread state", now() < deadline)?;
        sleep_ms(1);
    }
    Ok(())
}

fn signal(event: Handle, clear: u32, set: u32) -> Result {
    // SAFETY: the call takes no pointers.
    check("signalling", unsafe {
        sys::object_signal(event, clear, set)
    })
}

fn wait_async(object: Handle, port: Handle, key: u64, signals: u32, options: u32) -> Result {
    // SAFETY: the call takes no pointers.
    let status = unsafe { sys::object_wait_async(object, port, key, signals, options) };
    check("registering a wait", status)
}

/// Ports with user packets, one-shot and edge waits, cancelling and
/// closing.
fn ports(_: &Startup) -> Result {
    let (mut port, mut event) = (0, 0);
    // SAFETY: the pointers are valid for the calls.
    check("creating a port", unsafe { sys::port_create(0, &mut port) })?;
    // SAFETY: the pointer is valid for the call.
    check("creating an event", unsafe {
        sys::event_create(0, &mut event)
    })?;
    ensure(
        "timing out on an empty port",
        take_packet(port, soon()).0 == TIMED_OUT,
    )?;

    let mut user = [0u8; 48];
    user[..8].copy_from_slice(&7u64.to_le_bytes());
    user[8..16].copy_from_slice(&[9; 8]);
    user[16..20].copy_from_slice(b"ping");
    // SAFETY: the pointer describes `user`.
    check("queueing", unsafe { sys::port_queue(port, user.as_ptr()) })?;
    let (status, p) = take_packet(port, soon());
    ensure(
        "reading a user packet",
        status == 0 && u64_at(&p, 0) == 7 && u32_at(&p, 8) == 2,
    )?;
    ensure(
        "clearing the status",
        u32_at(&p, 12) == 0 && &p[16..20] == b"ping",
    )?;

    wait_async(event, port, 1, USER0, 0)?;
    ensure("not firing early", take_packet(port, soon()).0 == TIMED_OUT)?;
    signal(event, 0, USER0)?;
    let (status, p) = take_packet(port, soon());
    ensure(
        "firing",
        status == 0 && u64_at(&p, 0) == 1 && u32_at(&p, 8) == 1,
    )?;
    ensure(
        "reporting the signals",
        u32_at(&p, 16) == USER0 && u32_at(&p, 20) == USER0,
    )?;
    signal(event, USER0, 0)?;
    signal(event, 0, USER0)?;
    ensure("firing only once", take_packet(port, soon()).0 == TIMED_OUT)?;
    wait_async(event, port, 2, USER0, 0)?;
    ensure("firing at once", take_packet(port, soon()).0 == 0)?;
    signal(event, USER0, 0)?;

    wait_async(event, port, 3, USER1, 1)?;
    signal(event, 0, USER1)?;
    signal(event, USER1, 0)?;
    signal(event, 0, USER1)?;
    let (status, p) = take_packet(port, soon());
    ensure("firing on an edge", status == 0 && u64_at(&p, 0) == 3)?;
    ensure(
        "queueing one packet at a time",
        take_packet(port, soon()).0 == TIMED_OUT,
    )?;
    signal(event, USER1, 0)?;
    signal(event, 0, USER1)?;
    ensure("firing on the next edge", take_packet(port, soon()).0 == 0)?;
    signal(event, USER1, 0)?;
    signal(event, 0, USER1)?;
    // SAFETY: the call takes no pointers.
    check("cancelling", unsafe { sys::port_cancel(port, event, 3) })?;
    ensure(
        "dropping the cancelled packet",
        take_packet(port, soon()).0 == TIMED_OUT,
    )?;
    signal(event, USER1, 0)?;

    // A one-shot registration that fired has ended, but its unread packet
    // is still cancelled.
    wait_async(event, port, 5, USER0, 0)?;
    signal(event, 0, USER0)?;
    // A new registration removes the ended one from the kernel's list.
    wait_async(event, port, 6, USER1, 0)?;
    // SAFETY: the calls take no pointers.
    unsafe {
        check("cancelling a fired wait", sys::port_cancel(port, event, 5))?;
        check("cancelling the other", sys::port_cancel(port, event, 6))?;
    }
    ensure(
        "dropping its packet",
        take_packet(port, soon()).0 == TIMED_OUT,
    )?;
    signal(event, USER0, 0)?;

    let mut copy = 0;
    // SAFETY: the pointer is valid for the call.
    check("duplicating", unsafe {
        sys::handle_duplicate(event, 0x8000_0000, &mut copy)
    })?;
    wait_async(copy, port, 4, USER1, 0)?;
    // SAFETY: the call takes no pointers.
    check("closing", unsafe { sys::handle_close(copy) })?;
    signal(event, 0, USER1)?;
    ensure(
        "ending with the handle",
        take_packet(port, soon()).0 == TIMED_OUT,
    )?;

    for _ in 0..4096 {
        // SAFETY: the pointer describes `user`.
        check("filling the port", unsafe {
            sys::port_queue(port, user.as_ptr())
        })?;
    }
    // SAFETY: the pointer describes `user`.
    let full = unsafe { sys::port_queue(port, user.as_ptr()) };
    ensure("limiting user packets", full == -22)
}

fn u64_at(record: &[u8], at: usize) -> u64 {
    u64::from_le_bytes(record[at..at + 8].try_into().unwrap())
}

const READABLE: u32 = 1 << 0;
const SHOULD_WAIT: i64 = -22;

/// Reads the next record of `reader`.
fn read_record(reader: Handle) -> (i64, [u8; 288]) {
    let mut record = [0u8; 288];
    // SAFETY: the pointer describes `record`.
    let status = unsafe { sys::log_reader_read(reader, record.as_mut_ptr()) };
    (status, record)
}

/// A log reader reads every record in order, and READABLE follows
/// whether one is left.
fn log_readers(s: &Startup) -> Result {
    let mut reader = 0;
    // SAFETY: the pointer is valid for the call.
    check("creating a reader", unsafe {
        sys::log_reader_create(s.log, 0, &mut reader)
    })?;
    ensure("starting readable", wait(reader, READABLE, 0).0 == 0)?;
    let mut last = 0;
    loop {
        let (status, record) = read_record(reader);
        if status == SHOULD_WAIT {
            break;
        }
        check("reading a record", status)?;
        let sequence = u64_at(&record, 0);
        ensure("reading in order", sequence > last)?;
        last = sequence;
    }
    ensure("reading some records", last > 0)?;
    ensure(
        "clearing READABLE",
        wait(reader, READABLE, soon()).0 == TIMED_OUT,
    )?;

    let text = b"log reader test";
    // SAFETY: the pointer and length describe `text`.
    check("writing", unsafe {
        sys::log_write(s.log, 2, text.as_ptr(), text.len() as u64)
    })?;
    ensure("becoming readable", wait(reader, READABLE, soon()).0 == 0)?;
    let (status, record) = read_record(reader);
    check("reading the new record", status)?;
    ensure("reading the next sequence", u64_at(&record, 0) == last + 1)?;
    ensure("reading the fields", record[0x18] == 2 && record[0x19] == 1)?;
    let basic = info::<24>(s.process, 1)?;
    ensure(
        "naming the writer",
        u64_at(&record, 0x10) == u64_at(&basic, 0),
    )?;
    ensure(
        "reading the message",
        &record[0x20..0x20 + text.len()] == text,
    )?;
    ensure("reading to the end", read_record(reader).0 == SHOULD_WAIT)
}

fn free_pages() -> Result<u64> {
    let mut record = [0u8; 16];
    // SAFETY: the pointer describes `record`.
    let status = unsafe { sys::system_get_memory_info(record.as_mut_ptr()) };
    check("reading memory information", status)?;
    ensure("counting frames", u64_at(&record, 0) >= u64_at(&record, 8))?;
    Ok(u64_at(&record, 8))
}

/// The pages charged to the test's budget.
fn budget_pages(s: &Startup) -> Result<u64> {
    let record = info::<0x38>(s.budget, 6)?;
    Ok(u64_at(&record, 0x18))
}

/// The kernel memory charged to this process's budget.
fn budget_kernel(s: &Startup) -> Result<u64> {
    let record = info::<0x38>(s.budget, 6)?;
    Ok(u64_at(&record, 0x08))
}

/// The kernel charges nothing it made at boot, such as the boot image,
/// init's segments and stack, and the clock page, to the root budget that
/// init belongs to: no page is charged before init creates a memory
/// object. This test runs first, before any test creates one.
fn boot_memory_uncharged(s: &Startup) -> Result {
    ensure("charging no page", budget_pages(s)? == 0)
}

fn set_kernel_limit(s: &Startup, limit: u64) -> Result {
    // SAFETY: the call takes no pointers.
    check("setting the limits", unsafe {
        sys::budget_set_limits(s.budget, limit, u64::MAX)
    })
}

/// Writes to the 64 KiB of this thread's stack below the caller's frame,
/// so that the calls it makes next find those pages and their page tables
/// in place.
#[inline(never)]
fn touch_stack() {
    let mut area = [0u8; 64 * 1024];
    core::hint::black_box(&mut area);
}

/// Mapping and splitting regions is charged to the caller's budget, and
/// a call refused at the limit changes nothing.
fn region_charges_reach_the_limit(s: &Startup) -> Result {
    const NO_RESOURCES: i64 = -3;
    let len = 3 * PAGE_SIZE;
    let mem = create_memory(len)?;
    let mut mapped = [0u64; 1000];
    // The first mapping's middle page, and the stack below this frame,
    // are touched before the limit is set: a fault that needs kernel
    // memory above it, such as for a page table where the stack crosses
    // a 2 MiB boundary, would fail and end the process.
    mapped[0] = map(s.process, mem, len)?;
    // SAFETY: the mapping is writable.
    unsafe { ((mapped[0] + PAGE_SIZE) as *mut u64).write_volatile(1) };
    touch_stack();
    let mut count = 1;
    set_kernel_limit(s, budget_kernel(s)? + 4096)?;
    let refused = loop {
        let mut at = 0;
        // SAFETY: the out pointer is valid for the call.
        let status =
            unsafe { sys::vm_map(s.process, PERM_READ | PERM_WRITE, 0, mem, 0, len, &mut at) };
        if status != 0 || count == mapped.len() {
            break status;
        }
        mapped[count] = at;
        count += 1;
    };
    let result = (|| {
        ensure(
            "mapping until the limit",
            count > 1 && refused == NO_RESOURCES,
        )?;
        let used = budget_kernel(s)?;
        let mut at = 0;
        // SAFETY: the out pointer is valid for the call.
        let status =
            unsafe { sys::vm_map(s.process, PERM_READ | PERM_WRITE, 0, mem, 0, len, &mut at) };
        ensure("refusing another mapping", status == NO_RESOURCES)?;
        ensure("charging nothing for it", budget_kernel(s)? == used)?;
        // Protecting the middle page splits the region in three.
        let first = mapped[0];
        ensure(
            "refusing a split",
            protect(s, PERM_READ, first + PAGE_SIZE, PAGE_SIZE) == NO_RESOURCES,
        )?;
        ensure("charging nothing for the split", budget_kernel(s)? == used)?;
        // SAFETY: the page is still mapped writable, and present.
        unsafe { ((first + PAGE_SIZE) as *mut u64).write_volatile(2) };
        for &at in &mapped[count - 2..count] {
            // SAFETY: the call takes no pointers.
            check("unmapping", unsafe { sys::vm_unmap(s.process, at, len) })?;
        }
        count -= 2;
        check(
            "splitting with room",
            protect(s, PERM_READ, first + PAGE_SIZE, PAGE_SIZE),
        )
    })();
    set_kernel_limit(s, u64::MAX)?;
    for &at in &mapped[..count] {
        // SAFETY: the call takes no pointers.
        check("unmapping", unsafe { sys::vm_unmap(s.process, at, len) })?;
    }
    result
}

fn protect(s: &Startup, perms: u32, addr: u64, len: u64) -> i64 {
    // SAFETY: the call takes no pointers.
    unsafe { sys::vm_protect(s.process, perms, addr, len) }
}

/// Committing, decommitting and shrinking away a range of many pages
/// reaches every page and frees every frame.
fn large_memory_operations(s: &Startup) -> Result {
    let pages = 1300;
    let mem = create_memory(pages * PAGE_SIZE)?;
    let before = budget_pages(s)?;
    let at = map(s.process, mem, pages * PAGE_SIZE)?;
    let last = at + (pages - 1) * PAGE_SIZE;
    // SAFETY: the mapping is readable and writable.
    unsafe { (last as *mut u64).write_volatile(0x77) };
    // SAFETY: the call takes no pointers.
    check("committing", unsafe {
        sys::mem_op(mem, 1, 0, pages * PAGE_SIZE)
    })?;
    ensure("taking every frame", budget_pages(s)? == before + pages)?;
    // SAFETY: the mapping is readable.
    ensure("keeping the data", unsafe {
        (last as *const u64).read_volatile() == 0x77
    })?;
    // SAFETY: the call takes no pointers.
    check("decommitting", unsafe {
        sys::mem_op(mem, 2, 0, pages * PAGE_SIZE)
    })?;
    ensure("freeing every frame", budget_pages(s)? == before)?;
    // SAFETY: the mapping is readable.
    ensure("reading zero", unsafe {
        (last as *const u64).read_volatile() == 0
    })?;
    // SAFETY: the calls take no pointers.
    check("unmapping", unsafe {
        sys::vm_unmap(s.process, at, pages * PAGE_SIZE)
    })?;
    // SAFETY: as above.
    check("closing", unsafe { sys::handle_close(mem) })?;

    // Shrinking frees the frames past the new size, and pages that come
    // back with a larger size read as zero.
    let mut mem = 0;
    // SAFETY: the pointer is valid for the call.
    check("creating a resizable object", unsafe {
        sys::mem_create(pages * PAGE_SIZE, 1, &mut mem)
    })?;
    let at = map(s.process, mem, pages * PAGE_SIZE)?;
    let last = at + (pages - 1) * PAGE_SIZE;
    // SAFETY: the call takes no pointers.
    check("committing", unsafe {
        sys::mem_op(mem, 1, 0, pages * PAGE_SIZE)
    })?;
    // SAFETY: the mapping is readable and writable.
    unsafe { (last as *mut u64).write_volatile(0x77) };
    check("shrinking", set_size(mem, PAGE_SIZE))?;
    ensure("freeing the tail", budget_pages(s)? == before + 1)?;
    check("growing", set_size(mem, pages * PAGE_SIZE))?;
    // SAFETY: the mapping is readable.
    ensure("reading zero after growing", unsafe {
        (last as *const u64).read_volatile() == 0
    })?;
    // SAFETY: the calls take no pointers.
    check("unmapping", unsafe {
        sys::vm_unmap(s.process, at, pages * PAGE_SIZE)
    })?;
    // SAFETY: as above.
    check("closing", unsafe { sys::handle_close(mem) })
}

/// Committing takes frames and decommitting frees them and zeroes the
/// pages; mapping, unmapping and protection lengths are rounded up to
/// whole pages; protection changes stay within the mapping handle's
/// rights.
fn memory_operations(s: &Startup) -> Result {
    let pages = 8;
    let mem = create_memory(pages * PAGE_SIZE)?;
    // The budget's page count moves by exactly the object's frames; the
    // free count also moves with page tables and kernel memory.
    let before = budget_pages(s)?;
    free_pages()?;
    // SAFETY: the call takes no pointers.
    check("committing", unsafe {
        sys::mem_op(mem, 1, 0, pages * PAGE_SIZE)
    })?;
    ensure("taking frames", budget_pages(s)? == before + pages)?;
    let at = map(s.process, mem, pages * PAGE_SIZE)?;
    // SAFETY: the mapping is readable and writable.
    unsafe { (at as *mut u64).write_volatile(0x55) };
    // SAFETY: the call takes no pointers.
    check("decommitting", unsafe {
        sys::mem_op(mem, 2, 0, pages * PAGE_SIZE)
    })?;
    ensure("freeing frames", budget_pages(s)? == before)?;
    // SAFETY: the mapping is readable.
    ensure(
        "reading zero",
        unsafe { (at as *const u64).read_volatile() } == 0,
    )?;
    // SAFETY: the call takes no pointers.
    ensure(
        "refusing other ops",
        unsafe { sys::mem_op(mem, 3, 0, PAGE_SIZE) } == -10,
    )?;
    // The sizes of operations and objects are whole numbers of pages.
    // SAFETY: the calls take no pointers.
    unsafe {
        ensure(
            "refusing an unaligned op",
            sys::mem_op(mem, 1, 0, PAGE_SIZE + 1) == -10,
        )?;
        ensure(
            "refusing an unaligned size",
            sys::mem_set_size(mem, PAGE_SIZE + 1) == -10,
        )?;
    }
    // The lengths of mappings, unmappings and protection changes are
    // rounded up to whole pages, after a mapping's length as given is
    // checked against the object's size.
    let mut small = 0;
    // SAFETY: the out pointer is valid for the call.
    let status = unsafe { sys::vm_map(s.process, PERM_READ, 0, mem, 0, 100, &mut small) };
    check("mapping part of a page", status)?;
    let end = small + PAGE_SIZE - 8;
    check(
        "mapping the whole page",
        read_memory(s.process, end, &mut [0u8; 8]),
    )?;
    check("protecting part of a page", protect(s, 0, small, 100))?;
    ensure(
        "protecting the whole page",
        read_memory(s.process, end, &mut [0u8; 8]) == NOT_FOUND,
    )?;
    check(
        "making it readable again",
        protect(s, PERM_READ, small, PAGE_SIZE - 1),
    )?;
    // SAFETY: the call takes no pointers.
    check("unmapping part of a page", unsafe {
        sys::vm_unmap(s.process, small, 1)
    })?;
    ensure(
        "unmapping the whole page",
        read_memory(s.process, small, &mut [0u8; 1]) == NOT_FOUND
            && read_memory(s.process, end, &mut [0u8; 8]) == NOT_FOUND,
    )?;
    // SAFETY: the out pointer is valid for the call.
    let status = unsafe {
        sys::vm_map(
            s.process,
            PERM_READ,
            0,
            mem,
            PAGE_SIZE,
            (pages - 1) * PAGE_SIZE + 1,
            &mut small,
        )
    };
    ensure("checking the length as given", status == OUT_OF_RANGE)?;

    check("making it read-only", protect(s, PERM_READ, at, PAGE_SIZE))?;
    // SAFETY: the page is readable.
    ensure(
        "still reading",
        unsafe { (at as *const u64).read_volatile() } == 0,
    )?;
    check(
        "making it writable again",
        protect(s, PERM_READ | PERM_WRITE, at, PAGE_SIZE),
    )?;
    // SAFETY: the page is writable again.
    unsafe { (at as *mut u64).write_volatile(9) };
    ensure(
        "refusing write and execute",
        protect(s, 7, at, PAGE_SIZE) == -10,
    )?;
    ensure(
        "needing mapped pages",
        protect(s, PERM_READ, at, (pages + 1) * PAGE_SIZE) == -25,
    )?;

    let mut read_only = 0;
    // SAFETY: the pointer is valid for the call.
    let status = unsafe { sys::handle_duplicate(mem, (1 << 2) | (1 << 5), &mut read_only) };
    check("making a read-only handle", status)?;
    let mut ro = 0;
    // SAFETY: the pointer is valid for the call.
    let status = unsafe { sys::vm_map(s.process, PERM_READ, 0, read_only, 0, PAGE_SIZE, &mut ro) };
    check("mapping it", status)?;
    ensure(
        "staying within the rights",
        protect(s, PERM_READ | PERM_WRITE, ro, PAGE_SIZE) == -13,
    )
}

fn seal(mem: Handle, seals: u32) -> i64 {
    // SAFETY: the call takes no pointers.
    unsafe { sys::mem_seal(mem, seals) }
}

fn set_size(mem: Handle, size: u64) -> i64 {
    // SAFETY: the call takes no pointers.
    unsafe { sys::mem_set_size(mem, size) }
}

/// Seals stop resizing, every way of writing, and further sealing.
fn seals(s: &Startup) -> Result {
    let mut mem = 0;
    // SAFETY: the pointer is valid for the call.
    check("creating", unsafe {
        sys::mem_create(2 * PAGE_SIZE, 1, &mut mem)
    })?;
    ensure("refusing other bits", seal(mem, 1 << 4) == -10)?;
    check("sealing the size", seal(mem, 2 | 4))?;
    ensure("refusing to shrink", set_size(mem, PAGE_SIZE) == -13)?;
    ensure("refusing to grow", set_size(mem, 3 * PAGE_SIZE) == -13)?;
    check("keeping the size", set_size(mem, 2 * PAGE_SIZE))?;

    let at = map(s.process, mem, 2 * PAGE_SIZE)?;
    ensure("refusing while mapped writable", seal(mem, 8) == -20)?;
    // SAFETY: the call takes no pointers.
    check("unmapping", unsafe {
        sys::vm_unmap(s.process, at, 2 * PAGE_SIZE)
    })?;
    check("sealing writes", seal(mem, 8))?;
    let byte = [1u8];
    // SAFETY: the pointer and length describe `byte`.
    ensure(
        "refusing mem_write",
        unsafe { sys::mem_write(mem, 0, byte.as_ptr(), 1) } == -13,
    )?;
    // SAFETY: the call takes no pointers.
    ensure(
        "refusing decommit",
        unsafe { sys::mem_op(mem, 2, 0, PAGE_SIZE) } == -13,
    )?;
    let mut out = 0;
    // SAFETY: the pointer is valid for the call.
    let writable = unsafe {
        sys::vm_map(
            s.process,
            PERM_READ | PERM_WRITE,
            0,
            mem,
            0,
            PAGE_SIZE,
            &mut out,
        )
    };
    ensure("refusing writable mappings", writable == -13)?;
    // SAFETY: the pointer is valid for the call.
    let status = unsafe { sys::vm_map(s.process, PERM_READ, 0, mem, 0, PAGE_SIZE, &mut out) };
    check("mapping read-only", status)?;
    ensure(
        "refusing to make it writable",
        protect(s, PERM_READ | PERM_WRITE, out, PAGE_SIZE) == -13,
    )?;
    let record = info::<0x18>(mem, 4)?;
    ensure("reporting the seals", u32_at(&record, 0x10) == 2 | 4 | 8)?;
    check("sealing sealing", seal(mem, 1))?;
    ensure("refusing more seals", seal(mem, 0) == -13)
}

/// Makes a call with `request` after a 4-byte placeholder ID, room for
/// `cap` reply bytes and `handle`, if any, and returns the status, the
/// sizes stored, the reply and the handle received.
fn call(
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

/// Calls deliver replies to their callers, also with handles, and end
/// on a too-small buffer, a deadline and a closed peer; a late reply is
/// discarded.
fn channel_calls(s: &Startup) -> Result {
    let (_, server) = start_program(s, "call-server")?;
    let mem = create_memory(PAGE_SIZE)?;
    let text = b"call bytes";
    // SAFETY: the pointer and length describe `text`.
    check("filling", unsafe {
        sys::mem_write(mem, 0, text.as_ptr(), text.len() as u64)
    })?;
    let (status, out, reply, handle) = call(server, b"hello", Some(mem), 64, now() + 2_000_000_000);
    check("calling", status)?;
    ensure("storing the sizes", out == [9, 1])?;
    ensure("using a call ID", u32_at(&reply, 0) & 0x8000_0000 != 0)?;
    ensure("returning the bytes", &reply[4..9] == b"hello")?;
    let mut back = [0u8; 10];
    // SAFETY: the pointer and length describe `back`.
    check("reading the handle", unsafe {
        sys::mem_read(handle, 0, back.as_mut_ptr(), 10)
    })?;
    ensure("returning the handle", back == *text)?;

    let (status, out, _, _) = call(server, b"small", None, 64, now() + 2_000_000_000);
    ensure("refusing a large reply", status == -15 && out == [200, 0])?;
    let mut short = [0u8; 3];
    let fields: [u64; 8] = [short.as_mut_ptr() as u64, 3, 0, 0, 0, 0, 0, 0];
    let mut out = [0u64; 2];
    // SAFETY: the structure and buffers are valid.
    let status = unsafe {
        sys::channel_call(
            server,
            0,
            i64::MAX,
            fields.as_ptr().cast(),
            out.as_mut_ptr().cast(),
        )
    };
    ensure("needing an ID", status == -10)?;

    let (status, _, _, _) = call(server, b"late", None, 64, now() + 5_000_000);
    ensure("timing out", status == TIMED_OUT)?;
    sleep_ms(80);
    let mut probe = [0u8; 64];
    let (mut len, mut count) = (0u64, 0u64);
    // SAFETY: the pointers describe `probe`, `len` and `count`.
    let status = unsafe {
        sys::channel_read(
            server,
            probe.as_mut_ptr(),
            64,
            core::ptr::null_mut(),
            0,
            &mut len,
            &mut count,
        )
    };
    ensure("discarding the late reply", status == SHOULD_WAIT)?;
    let (status, _, reply, _) = call(server, b"again", None, 64, now() + 2_000_000_000);
    ensure("calling again", status == 0 && &reply[4..9] == b"again")?;

    let (status, _, _, _) = call(server, b"quit", None, 64, now() + 2_000_000_000);
    ensure("ending on a closed peer", status == -24)
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

/// Starts a thread of this process at `entry` with both arguments.
fn spawn2(s: &Startup, entry: extern "sysv64" fn(u64, u64) -> !, a: u64, b: u64) -> Result<Handle> {
    let stack_size = 16 * 1024;
    let stack = create_memory(stack_size)?;
    let at = map(s.process, stack, stack_size)?;
    let mut thread = 0;
    let name = b"helper";
    // SAFETY: the pointers describe `name` and `thread`.
    let status =
        unsafe { sys::thread_create(s.process, name.as_ptr(), name.len() as u64, &mut thread) };
    check("creating a thread", status)?;
    // SAFETY: the call takes no pointers; the stack is mapped and unused.
    let status =
        unsafe { sys::thread_start(thread, entry as *const () as u64, at + stack_size - 8, a, b) };
    check("starting the thread", status)?;
    Ok(thread)
}

fn futex_wake(addr: u64, count: u32) -> Result<u32> {
    let mut woken = 0;
    // SAFETY: the pointers are valid for the call.
    check("waking", unsafe {
        sys::futex_wake(addr as *const u32, count, &mut woken)
    })?;
    Ok(woken)
}

/// Futexes compare before blocking, time out, wake in arrival order and
/// are keyed by memory object and offset.
fn futexes(s: &Startup) -> Result {
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
    for index in 1..=3 {
        spawn2(s, futex_waiter, word, index)?;
        sleep_ms(5);
    }
    ensure("waking one", futex_wake(word, 1)? == 1)?;
    sleep_ms(5);
    ensure(
        "waking the oldest",
        FUTEX_ORDER[0].load(Ordering::Acquire) == 1,
    )?;
    // The second mapping reaches the same futex. Waiters are woken one
    // at a time, as threads woken together may run in either order.
    ensure("waking the next", futex_wake(second + 64, 1)? == 1)?;
    sleep_ms(5);
    ensure(
        "waking in order",
        FUTEX_ORDER[1].load(Ordering::Acquire) == 2,
    )?;
    ensure("waking the rest", futex_wake(word, u32::MAX)? == 1)?;
    sleep_ms(5);
    ensure(
        "waking the last",
        FUTEX_ORDER[2].load(Ordering::Acquire) == 3,
    )?;
    ensure("waking nobody", futex_wake(word, 1)? == 0)
}

extern "sysv64" fn raise_later(timeline: u64, _: u64) -> ! {
    sleep_ms(20);
    // SAFETY: the call takes no pointers.
    unsafe { sys::timeline_signal(timeline as Handle, 10) };
    // SAFETY: the call takes no pointers.
    unsafe { sys::thread_exit() };
    unreachable!("thread_exit returned");
}

/// Timelines only grow, wake waiters that their value reaches and queue
/// TIMELINE packets.
fn timelines(s: &Startup) -> Result {
    let (mut timeline, mut port) = (0, 0);
    // SAFETY: the pointers are valid for the calls.
    unsafe {
        check("creating", sys::timeline_create(3, 0, &mut timeline))?;
        check("creating a port", sys::port_create(0, &mut port))?;
    }
    let mut value = 0;
    // SAFETY: the pointer is valid for the call.
    check("reading", unsafe {
        sys::timeline_get(timeline, &mut value)
    })?;
    ensure("starting at the initial value", value == 3)?;
    // SAFETY: the calls take no pointers.
    unsafe {
        ensure(
            "refusing to go back",
            sys::timeline_signal(timeline, 2) == -10,
        )?;
        check("raising", sys::timeline_signal(timeline, 5))?;
        check("reached already", sys::timeline_wait(timeline, 4, 0))?;
        ensure(
            "timing out",
            sys::timeline_wait(timeline, 6, soon()) == TIMED_OUT,
        )?;
        check(
            "waiting asynchronously",
            sys::timeline_wait_async(timeline, port, 21, 8),
        )?;
        check(
            "waiting for a reached value",
            sys::timeline_wait_async(timeline, port, 22, 1),
        )?;
    }
    let (status, p) = take_packet(port, soon());
    ensure(
        "queueing at once",
        status == 0 && u64_at(&p, 0) == 22 && u32_at(&p, 8) == 4,
    )?;
    ensure("carrying the value", u64_at(&p, 16) == 5)?;
    spawn(s, raise_later, u64::from(timeline))?;
    // SAFETY: the call takes no pointers.
    check("waiting for 9", unsafe {
        sys::timeline_wait(timeline, 9, now() + 2_000_000_000)
    })?;
    let (status, p) = take_packet(port, soon());
    ensure(
        "queueing when reached",
        status == 0 && u64_at(&p, 0) == 21 && u64_at(&p, 16) == 10,
    )
}

fn stream_io(stream: Handle, write: bool, buf: &mut [u8]) -> (i64, u64) {
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

/// Streams carry bytes without boundaries, fill up, keep small writes
/// whole and keep queued bytes after the peer closes.
fn streams(_: &Startup) -> Result {
    let (mut a, mut b) = (0, 0);
    // SAFETY: the pointers are valid for the call.
    check("creating", unsafe { sys::stream_create(0, &mut a, &mut b) })?;
    ensure("writing", stream_io(a, true, &mut { *b"hello " }) == (0, 6))?;
    ensure(
        "writing more",
        stream_io(a, true, &mut { *b"world" }) == (0, 5),
    )?;
    let mut buf = [0u8; 8];
    ensure(
        "reading across writes",
        stream_io(b, false, &mut buf) == (0, 8) && &buf == b"hello wo",
    )?;
    ensure("being readable", wait(b, READABLE, 0).0 == 0)?;
    let mut rest = [0u8; 8];
    ensure(
        "reading the rest",
        stream_io(b, false, &mut rest) == (0, 3) && &rest[..3] == b"rld",
    )?;
    ensure(
        "being empty",
        stream_io(b, false, &mut rest).0 == SHOULD_WAIT,
    )?;

    let mut chunk = [7u8; 4096];
    for _ in 0..15 {
        ensure("filling", stream_io(a, true, &mut chunk) == (0, 4096))?;
    }
    let mut big = [1u8; 4097];
    let (status, written) = stream_io(a, true, &mut big[..4000]);
    ensure(
        "keeping a small write whole",
        status == 0 && written == 4000,
    )?;
    let (status, written) = stream_io(a, true, &mut big);
    ensure(
        "taking what fits of a large write",
        status == 0 && written == 96,
    )?;
    ensure(
        "being full",
        stream_io(a, true, &mut chunk[..1]).0 == SHOULD_WAIT,
    )?;
    ensure("clearing WRITABLE", wait(a, 1 << 1, 0).0 == TIMED_OUT)?;

    // SAFETY: the call takes no pointers.
    check("closing the writer", unsafe { sys::handle_close(a) })?;
    ensure("setting PEER_CLOSED", wait(b, 1 << 2, 0).0 == 0)?;
    let (status, got) = stream_io(b, false, &mut chunk);
    ensure("keeping queued bytes", status == 0 && got == 4096)
}

const INVALID_ARGS: i64 = -10;

/// Waits on the word at `addr` until woken, then exits.
extern "sysv64" fn word_waiter(addr: u64, _: u64) -> ! {
    // SAFETY: `addr` is a mapped, aligned word.
    unsafe { sys::futex_wait(addr as *const u32, 0, i64::MAX) };
    // SAFETY: the call takes no pointers.
    unsafe { sys::thread_exit() };
    unreachable!("thread_exit returned");
}

/// A call whose output lies on a read-only page fails before changing
/// any state: written bytes are not queued, and read bytes and packets
/// stay queued.
fn failed_outputs(s: &Startup) -> Result {
    let page = create_memory(PAGE_SIZE)?;
    let mut ro = 0;
    // SAFETY: the out pointer is valid for the call.
    check("mapping read-only", unsafe {
        sys::vm_map(s.process, PERM_READ, 0, page, 0, PAGE_SIZE, &mut ro)
    })?;
    let ro_u64 = ro as *mut u64;

    let (mut a, mut b) = (0, 0);
    // SAFETY: the pointers are valid for the call.
    check("creating a stream", unsafe {
        sys::stream_create(0, &mut a, &mut b)
    })?;
    // SAFETY: the pointers are valid; the count lies on a read-only page.
    let status = unsafe { sys::stream_write(a, b"lost".as_ptr(), 4, ro_u64) };
    ensure("failing a write", status == INVALID_ARGS)?;
    let mut buf = [0u8; 8];
    ensure(
        "queueing nothing",
        stream_io(b, false, &mut buf).0 == SHOULD_WAIT,
    )?;
    ensure("writing", stream_io(a, true, &mut { *b"kept" }) == (0, 4))?;
    // SAFETY: the buffer is valid; the count lies on a read-only page.
    let status = unsafe { sys::stream_read(b, buf.as_mut_ptr(), 8, ro_u64) };
    ensure("failing a read", status == INVALID_ARGS)?;
    let mut actual = 0;
    // SAFETY: the buffer lies on a read-only page; the count is valid.
    let status = unsafe { sys::stream_read(b, ro as *mut u8, 8, &mut actual) };
    ensure(
        "failing a read into read-only bytes",
        status == INVALID_ARGS,
    )?;
    ensure(
        "keeping read bytes",
        stream_io(b, false, &mut buf) == (0, 4) && &buf[..4] == b"kept",
    )?;

    let mut port = 0;
    // SAFETY: the pointer is valid for the call.
    check("creating a port", unsafe { sys::port_create(0, &mut port) })?;
    let mut user = [0u8; 48];
    user[..8].copy_from_slice(&5u64.to_le_bytes());
    // SAFETY: the pointer describes `user`.
    check("queueing", unsafe { sys::port_queue(port, user.as_ptr()) })?;
    // SAFETY: the packet output lies on a read-only page.
    let status = unsafe { sys::port_wait(port, soon(), ro as *mut u8) };
    ensure("failing a wait", status == INVALID_ARGS)?;
    let (status, p) = take_packet(port, soon());
    ensure("keeping the packet", status == 0 && u64_at(&p, 0) == 5)?;

    let word = create_memory(PAGE_SIZE)?;
    let addr = map(s.process, word, PAGE_SIZE)?;
    spawn2(s, word_waiter, addr, 0)?;
    // SAFETY: the call takes no pointers.
    unsafe { sys::thread_sleep(now() + 20_000_000) };
    // SAFETY: the word is mapped; the count lies on a read-only page.
    let status = unsafe { sys::futex_wake(addr as *const u32, 1, ro as *mut u32) };
    ensure("failing a wake", status == INVALID_ARGS)?;
    ensure("leaving the waiter waiting", futex_wake(addr, 1)? == 1)?;

    // A blocked real-time signal stays pending through a failed wait.
    let rt = 1u64 << (40 - 1);
    // SAFETY: the calls take no pointers or a null one.
    unsafe {
        check(
            "blocking a signal",
            sys::signal_set_mask(0, rt, core::ptr::null_mut()),
        )?;
        check("sending a signal", sys::thread_signal(s.thread, 40, 77))?;
    }
    // SAFETY: the information output lies on a read-only page.
    let status = unsafe { sys::signal_wait(rt, soon(), ro as *mut u8) };
    ensure("failing a signal wait", status == INVALID_ARGS)?;
    let mut info = [0u8; 64];
    // SAFETY: the pointer describes `info`, which holds a record.
    let status = unsafe { sys::signal_wait(rt, soon(), info.as_mut_ptr()) };
    // SAFETY: the call takes a null pointer.
    unsafe { sys::signal_set_mask(1, rt, core::ptr::null_mut()) };
    ensure(
        "keeping the signal",
        status == 0 && u32_at(&info, 0) == 40 && u64_at(&info, 0x10) == 77,
    )
}

/// Calls that return a handle refuse an output outside the user address
/// range, also one that wraps around the end of the address space, and
/// a refused `handle_replace` keeps the handle it was given.
fn outputs_outside_user_range(s: &Startup) -> Result {
    let mem = create_memory(PAGE_SIZE)?;
    let name = b"refused";
    let thread = koid(s.thread)?;
    for out in [u64::MAX - 3, 0xFFFF_8000_0000_0000, 0x7FFF_FFFF_EFFE] {
        let out = out as *mut Handle;
        let len = name.len() as u64;
        // SAFETY: `name` is valid; every call refuses `out` before
        // writing to it.
        let statuses = unsafe {
            [
                ("duplicating", sys::handle_duplicate(mem, 0x8000_0000, out)),
                ("replacing", sys::handle_replace(mem, 0x8000_0000, out)),
                (
                    "creating a memory object",
                    sys::mem_create(PAGE_SIZE, 0, out),
                ),
                (
                    "creating a budget",
                    sys::budget_create(s.budget, u64::MAX, u64::MAX, out),
                ),
                (
                    "creating a process",
                    sys::process_create(s.budget, name.as_ptr(), len, out),
                ),
                (
                    "creating a thread",
                    sys::thread_create(s.process, name.as_ptr(), len, out),
                ),
                (
                    "getting a thread",
                    sys::process_get_thread(s.process, thread, out),
                ),
            ]
        };
        for (what, status) in statuses {
            ensure(what, status == INVALID_ARGS)?;
        }
    }
    mem_read_at::<1>(mem, 0)?;
    // SAFETY: the call takes no pointers.
    check("closing the object", unsafe { sys::handle_close(mem) })
}

/// Calls refuse an input that does not lie in readable user memory: one
/// that wraps around the end of the address space, one in the kernel
/// half, one on a page that is no longer mapped and one on a page mapped
/// without access. They change nothing.
fn inputs_outside_user_memory(s: &Startup) -> Result {
    let mem = create_memory(PAGE_SIZE)?;
    let (mut reserved, mut a, mut b, mut port, mut thread) = (0, 0, 0, 0, 0);
    // SAFETY: the out pointers are valid for the calls.
    unsafe {
        check(
            "reserving a page",
            sys::vm_map(s.process, 0, 0, mem, 0, PAGE_SIZE, &mut reserved),
        )?;
        check("creating a channel", sys::channel_create(0, &mut a, &mut b))?;
        check("creating a port", sys::port_create(0, &mut port))?;
    }
    let gone = map(s.process, create_memory(PAGE_SIZE)?, PAGE_SIZE)?;
    // SAFETY: the call takes no pointers.
    check("unmapping a page", unsafe {
        sys::vm_unmap(s.process, gone, PAGE_SIZE)
    })?;
    for addr in [u64::MAX - 3, 0xFFFF_8000_0000_0000, gone, reserved] {
        let p = addr as *const u8;
        // SAFETY: every call refuses `p` before it reads from it; the
        // other pointers are valid.
        let statuses = unsafe {
            [
                ("writing an object", sys::mem_write(mem, 0, p, 8)),
                (
                    "writing a message",
                    sys::channel_write(a, p, 8, core::ptr::null(), 0),
                ),
                (
                    "sending handles",
                    sys::channel_write(a, b"x".as_ptr(), 1, p.cast(), 1),
                ),
                ("logging", sys::log_write(s.log, 1, p, 8)),
                (
                    "naming a thread",
                    sys::thread_create(s.process, p, 8, &mut thread),
                ),
                ("queueing a packet", sys::port_queue(port, p)),
            ]
        };
        for (what, status) in statuses {
            ensure(what, status == INVALID_ARGS)?;
        }
    }
    ensure("queueing no message", read_sizes(b, 16, 2).0 == SHOULD_WAIT)?;
    ensure(
        "queueing no packet",
        take_packet(port, soon()).0 == TIMED_OUT,
    )?;
    ensure("writing nothing", mem_read_at::<8>(mem, 0)? == [0; 8])?;
    // SAFETY: the calls take no pointers.
    unsafe {
        check("unmapping", sys::vm_unmap(s.process, reserved, PAGE_SIZE))?;
        for h in [mem, a, b, port] {
            check("closing", sys::handle_close(h))?;
        }
    }
    Ok(())
}

/// Calls refuse `options` with a bit they do not define, and write no
/// output.
fn undefined_options_refused(s: &Startup) -> Result {
    let mem = create_memory(PAGE_SIZE)?;
    let (mut pager, mut port) = (0, 0);
    // SAFETY: the out pointers are valid for the calls.
    unsafe {
        check("creating a pager", sys::pager_create(0, &mut pager))?;
        check("creating a port", sys::port_create(0, &mut port))?;
    }
    let mut outs = [Handle::MAX; 2];
    let mut addr = u64::MAX;
    let (out, out1) = (outs.as_mut_ptr(), outs[1..].as_mut_ptr());
    let undefined = 1 << 31;
    // SAFETY: the out pointers are valid for the calls.
    let statuses = unsafe {
        [
            ("creating an event", sys::event_create(1, out)),
            ("creating a timer", sys::timer_create(1, out)),
            ("creating a port", sys::port_create(1, out)),
            ("creating a pager", sys::pager_create(1, out)),
            ("creating a timeline", sys::timeline_create(0, 1, out)),
            ("creating a channel", sys::channel_create(1, out, out1)),
            ("creating a stream", sys::stream_create(1, out, out1)),
            (
                "creating a memory object",
                sys::mem_create(PAGE_SIZE, undefined, out),
            ),
            ("cloning", sys::mem_clone(mem, undefined, 0, PAGE_SIZE, out)),
            (
                "creating a paged object",
                sys::pager_create_memory(pager, port, 0, PAGE_SIZE, undefined, out),
            ),
            (
                "creating an exception channel",
                sys::exception_channel_create(s.process, undefined, out),
            ),
            (
                "mapping",
                sys::vm_map(
                    s.process,
                    PERM_READ | 1 << 3,
                    0,
                    mem,
                    0,
                    PAGE_SIZE,
                    &mut addr,
                ),
            ),
        ]
    };
    for (what, status) in statuses {
        ensure(what, status == INVALID_ARGS)?;
    }
    ensure(
        "writing no output",
        outs == [Handle::MAX; 2] && addr == u64::MAX,
    )?;
    for h in [mem, pager, port] {
        // SAFETY: the call takes no pointers.
        check("closing", unsafe { sys::handle_close(h) })?;
    }
    Ok(())
}

/// `resource_create` refuses ranges that overlap hardware the kernel
/// keeps: the ports of the PICs, the PIT, its gate and the serial port,
/// the register page of the I/O APIC and, as the kernel uses it under
/// QEMU, the HPET, both at QEMU's addresses. A port next to them is
/// granted.
fn reserved_hardware_refused(s: &Startup) -> Result {
    const KIND_MMIO: u32 = 1;
    let io = mikros_test::KIND_IOPORT;
    let reserved = [
        (io, 0x20, 2),
        (io, 0x1F, 2),
        (io, 0xA1, 1),
        (io, 0x40, 4),
        (io, 0x43, 1),
        (io, 0x61, 1),
        (io, 0x3F8, 8),
        (io, 0x3FF, 4),
        (KIND_MMIO, 0xFEC0_0000, PAGE_SIZE),
        (KIND_MMIO, 0xFED0_0000, PAGE_SIZE),
    ];
    let mut resource = 0;
    for (kind, base, size) in reserved {
        // SAFETY: the out pointer is valid for the call.
        let status =
            unsafe { sys::resource_create(s.root_resource, kind, base, size, 0, &mut resource) };
        ensure(
            "refusing hardware the kernel keeps",
            status == ACCESS_DENIED,
        )?;
    }
    // SAFETY: the out pointer is valid for the call.
    check("granting a free port", unsafe {
        sys::resource_create(s.root_resource, io, 0x80, 1, 0, &mut resource)
    })?;
    // SAFETY: the call takes no pointers.
    check("closing", unsafe { sys::handle_close(resource) })
}

fn mem_write_at(mem: Handle, offset: u64, bytes: &[u8]) -> Result {
    // SAFETY: the pointer and length describe `bytes`.
    check("writing", unsafe {
        sys::mem_write(mem, offset, bytes.as_ptr(), bytes.len() as u64)
    })
}

fn mem_read_at<const N: usize>(mem: Handle, offset: u64) -> Result<[u8; N]> {
    let mut bytes = [0u8; N];
    // SAFETY: the pointer and length describe `bytes`.
    check("reading", unsafe {
        sys::mem_read(mem, offset, bytes.as_mut_ptr(), N as u64)
    })?;
    Ok(bytes)
}

/// A clone starts with the source range's bytes and is separate from it
/// afterwards.
fn clones(_: &Startup) -> Result {
    let source = create_memory(3 * PAGE_SIZE)?;
    mem_write_at(source, PAGE_SIZE - 3, b"across pages")?;
    let mut clone = 0;
    // SAFETY: the pointer is valid for the call.
    let status = unsafe { sys::mem_clone(source, 0, 0, 2 * PAGE_SIZE, &mut clone) };
    check("cloning", status)?;
    ensure(
        "copying the range",
        mem_read_at::<12>(clone, PAGE_SIZE - 3)? == *b"across pages",
    )?;
    ensure(
        "reading zero elsewhere",
        mem_read_at::<8>(clone, 200)? == [0; 8],
    )?;
    mem_write_at(source, PAGE_SIZE - 3, b"AC")?;
    ensure(
        "keeping the clone",
        mem_read_at::<4>(clone, PAGE_SIZE - 3)? == *b"acro",
    )?;
    mem_write_at(clone, PAGE_SIZE - 3, b"x")?;
    ensure(
        "keeping the source",
        mem_read_at::<4>(source, PAGE_SIZE - 3)? == *b"ACro",
    )?;
    let record = info::<0x18>(clone, 4)?;
    ensure("sizing the clone", u64_at(&record, 0) == 2 * PAGE_SIZE)?;
    let mut unaligned = 0;
    // SAFETY: the pointer is valid for the call.
    let status = unsafe { sys::mem_clone(source, 0, PAGE_SIZE - 5, PAGE_SIZE, &mut unaligned) };
    ensure("refusing an unaligned offset", status == -10)?;
    // SAFETY: the pointer is valid for the call.
    let status = unsafe { sys::mem_clone(source, 0, 0, PAGE_SIZE + 100, &mut unaligned) };
    ensure("refusing an unaligned length", status == -10)?;
    // A whole page whose bytes never repeat with a period of the kernel's
    // copy chunk, so every chunk of the clone is checked.
    let mut page = [0u8; PAGE_SIZE as usize];
    for (i, b) in page.iter_mut().enumerate() {
        *b = (i / 3) as u8 ^ (i >> 9) as u8;
    }
    mem_write_at(source, PAGE_SIZE, &page)?;
    let mut whole = 0;
    // SAFETY: the pointer is valid for the call.
    let status = unsafe { sys::mem_clone(source, 0, PAGE_SIZE, PAGE_SIZE, &mut whole) };
    check("cloning a page", status)?;
    ensure(
        "copying every chunk",
        mem_read_at::<{ PAGE_SIZE as usize }>(whole, 0)? == page,
    )?;
    // SAFETY: the pointer is valid for the call.
    let bad = unsafe { sys::mem_clone(source, 2, 0, 1, &mut clone) };
    ensure("refusing other options", bad == -10)
}

/// The pager handle of the pager test, and how many page requests its
/// pager thread answered.
static PAGER: AtomicU64 = AtomicU64::new(0);
static REQUESTS: AtomicU64 = AtomicU64::new(0);

/// Answers the page requests on `port` for `mem` until none comes for a
/// second, supplying pages that start with "page N".
extern "sysv64" fn pager_thread(port: u64, mem: u64) -> ! {
    let pager = PAGER.load(Ordering::Acquire) as Handle;
    loop {
        let (status, p) = take_packet(port as Handle, now() + 1_000_000_000);
        if status != 0 || u32_at(&p, 8) != 3 || u32_at(&p, 16) != 1 {
            break;
        }
        REQUESTS.fetch_add(1, Ordering::AcqRel);
        let offset = u64_at(&p, 24);
        let Ok(source) = create_memory(PAGE_SIZE) else {
            break;
        };
        let text = [
            b'p',
            b'a',
            b'g',
            b'e',
            b' ',
            // Tests check the label of the first pages only.
            b'0'.wrapping_add((offset / PAGE_SIZE) as u8),
        ];
        let _ = mem_write_at(source, 0, &text);
        // SAFETY: the call takes no pointers.
        unsafe { sys::pager_supply(pager, mem as Handle, offset, PAGE_SIZE, source, 0) };
    }
    // SAFETY: the call takes no pointers.
    unsafe { sys::thread_exit() };
    unreachable!("thread_exit returned");
}

/// A paged object requests each absent page once, from mappings and
/// system calls, and gets the frames its pager supplies.
fn pagers(s: &Startup) -> Result {
    let (mut pager, mut port, mut mem) = (0, 0, 0);
    // SAFETY: the pointers are valid for the calls.
    unsafe {
        check("creating a pager", sys::pager_create(0, &mut pager))?;
        check("creating a port", sys::port_create(0, &mut port))?;
        let status = sys::pager_create_memory(pager, port, 5, 2 * PAGE_SIZE, 0, &mut mem);
        check("creating a paged object", status)?;
    }
    ensure("reporting the kind", u32_at(&info::<0x18>(mem, 4)?, 8) == 5)?;
    PAGER.store(u64::from(pager), Ordering::Release);
    spawn2(s, pager_thread, u64::from(port), u64::from(mem))?;
    let at = map(s.process, mem, 2 * PAGE_SIZE)?;
    // SAFETY: the mapping covers both pages; reading them waits for the
    // pager.
    let (first, second) = unsafe {
        (
            (at as *const [u8; 6]).read_volatile(),
            ((at + PAGE_SIZE) as *const [u8; 6]).read_volatile(),
        )
    };
    ensure(
        "supplying through faults",
        first == *b"page 0" && second == *b"page 1",
    )?;
    ensure(
        "reading through mem_read",
        mem_read_at::<6>(mem, PAGE_SIZE)? == *b"page 1",
    )?;
    // SAFETY: the mapping is writable.
    unsafe { (at as *mut u8).write_volatile(b'P') };
    ensure("keeping writes", mem_read_at::<6>(mem, 0)? == *b"Page 0")?;
    ensure(
        "requesting each page once",
        REQUESTS.load(Ordering::Acquire) == 2,
    )?;

    let source = create_memory(PAGE_SIZE)?;
    mem_write_at(source, 0, b"left")?;
    let (mut other, plain) = (0, create_memory(PAGE_SIZE)?);
    // SAFETY: the pointer is valid for the call.
    check("creating another pager", unsafe {
        sys::pager_create(0, &mut other)
    })?;
    // SAFETY: the calls take no pointers.
    unsafe {
        ensure(
            "needing its pager",
            sys::pager_supply(other, mem, 0, PAGE_SIZE, source, 0) == -13,
        )?;
        ensure(
            "needing a paged object",
            sys::pager_supply(pager, plain, 0, PAGE_SIZE, source, 0) == -2,
        )?;
        check(
            "supplying present pages",
            sys::pager_supply(pager, mem, 0, PAGE_SIZE, source, 0),
        )?;
    }
    ensure(
        "leaving present pages alone",
        mem_read_at::<6>(mem, 0)? == *b"Page 0",
    )?;
    ensure(
        "decommitting the source",
        mem_read_at::<4>(source, 0)? == [0; 4],
    )?;
    check("sealing a source", seal(plain, 8))?;
    // SAFETY: the call takes no pointers.
    let sealed = unsafe { sys::pager_supply(pager, mem, 0, PAGE_SIZE, plain, 0) };
    ensure("refusing a sealed source", sealed == -13)
}

/// Outputs on absent pages of a paged object wait for the pager instead
/// of failing.
fn paged_outputs(s: &Startup) -> Result {
    let before = REQUESTS.load(Ordering::Acquire);
    let (_pager, mem) = paged_object(s, 5, 0)?;
    let at = map(s.process, mem, 5 * PAGE_SIZE)?;
    let len = (at + PAGE_SIZE) as *mut u64;
    // SAFETY: the record and its size lie in the writable mapping.
    let status = unsafe { sys::object_get_info(mem, 1, at as *mut u8, 64, len) };
    check("reading information into absent pages", status)?;
    // SAFETY: the mapping is readable.
    let size = unsafe { len.read_volatile() };
    ensure("writing the size", size == 24)?;
    let ends = (at + 2 * PAGE_SIZE) as *mut Handle;
    // SAFETY: both handle outputs lie in the writable mapping.
    let status = unsafe { sys::channel_create(0, ends, ends.add(1)) };
    check("creating a channel into an absent page", status)?;
    // SAFETY: the mapping is readable.
    let (a, b) = unsafe { (ends.read_volatile(), ends.add(1).read_volatile()) };
    ensure("writing both handles", a != 0 && b != 0 && a != b)?;
    // SAFETY: the pointer and length describe the message.
    check("writing a message", unsafe {
        sys::channel_write(a, b"paged".as_ptr(), 5, core::ptr::null(), 0)
    })?;
    let (got, sizes) = (
        (at + 3 * PAGE_SIZE) as *mut u8,
        (at + 4 * PAGE_SIZE) as *mut u64,
    );
    // SAFETY: the bytes and both sizes lie in the writable mapping.
    let status =
        unsafe { sys::channel_read(b, got, 16, core::ptr::null_mut(), 0, sizes, sizes.add(1)) };
    check("reading a message into absent pages", status)?;
    // SAFETY: the mapping is readable.
    let (bytes, len, count) = unsafe {
        (
            (got as *const [u8; 5]).read_volatile(),
            sizes.read_volatile(),
            sizes.add(1).read_volatile(),
        )
    };
    ensure(
        "delivering the message",
        &bytes == b"paged" && len == 5 && count == 0,
    )?;
    ensure(
        "requesting each page",
        REQUESTS.load(Ordering::Acquire) == before + 5,
    )
}

const IO: i64 = -28;
const BAD_STATE: i64 = -20;
const NOT_FOUND: i64 = -25;

fn pager_op(pager: Handle, mem: Handle, op: u32, offset: u64, len: u64) -> i64 {
    // SAFETY: the call takes no pointers.
    unsafe { sys::pager_op(pager, mem, op, offset, len) }
}

/// The dirty ranges of `mem` as byte offsets and lengths.
fn dirty(pager: Handle, mem: Handle, len: u64) -> Result<([u64; 8], u64)> {
    let mut out = [0u64; 8];
    let mut count = 0;
    // SAFETY: the pointers describe `out` and `count`.
    let status = unsafe {
        sys::pager_query_dirty(pager, mem, 0, len, out.as_mut_ptr().cast(), 4, &mut count)
    };
    check("querying dirty pages", status)?;
    Ok((out, count))
}

/// A new paged object of `pages` pages with `options`, answered by a
/// pager thread, and its pager.
fn paged_object(s: &Startup, pages: u64, options: u32) -> Result<(Handle, Handle)> {
    let (mut pager, mut port, mut mem) = (0, 0, 0);
    // SAFETY: the pointers are valid for the calls.
    unsafe {
        check("creating a pager", sys::pager_create(0, &mut pager))?;
        check("creating a port", sys::port_create(0, &mut port))?;
        let status = sys::pager_create_memory(pager, port, 6, pages * PAGE_SIZE, options, &mut mem);
        check("creating a paged object", status)?;
    }
    PAGER.store(u64::from(pager), Ordering::Release);
    spawn2(s, pager_thread, u64::from(port), u64::from(mem))?;
    Ok((pager, mem))
}

/// Faults that wait for a pager, in the copy routine of a system call
/// and in user code that set RFLAGS.AC, leave AC clear in the kernel,
/// which the debug kernel checks whenever it switches threads.
fn access_flag_cleared_on_entry(s: &Startup) -> Result {
    let (_pager, mem) = paged_object(s, 2, 0)?;
    let at = map(s.process, mem, 2 * PAGE_SIZE)?;
    let copy = create_memory(PAGE_SIZE)?;
    // SAFETY: the source lies in the mapping; reading it waits for the
    // pager.
    check("writing from an absent page", unsafe {
        sys::mem_write(copy, 0, at as *const u8, 6)
    })?;
    ensure(
        "copying the supplied page",
        mem_read_at::<6>(copy, 0)? == *b"page 0",
    )?;
    let byte: u8;
    // SAFETY: the second page is mapped readable. With AC set, only the
    // aligned stack words and one byte are accessed, so alignment checks
    // cannot fault.
    unsafe {
        core::arch::asm!(
            "pushfq",
            "bts qword ptr [rsp], 18",
            "popfq",
            "mov {byte}, byte ptr [{addr}]",
            "pushfq",
            "btr qword ptr [rsp], 18",
            "popfq",
            addr = in(reg) at + PAGE_SIZE,
            byte = out(reg_byte) byte,
        );
    }
    ensure("reading with AC set", byte == b'p')
}

/// Supplying, writing back and evicting a paged object of more pages
/// than the kernel handles in one piece reaches every page.
fn large_pager_operations(s: &Startup) -> Result {
    let pages = 1300;
    let size = pages * PAGE_SIZE;
    let (mut pager, mut port, mut mem) = (0, 0, 0);
    // SAFETY: the pointers are valid for the calls.
    unsafe {
        check("creating a pager", sys::pager_create(0, &mut pager))?;
        check("creating a port", sys::port_create(0, &mut port))?;
        let status = sys::pager_create_memory(pager, port, 10, size, 2, &mut mem);
        check("creating a paged object", status)?;
    }
    // Every page of the source holds its number.
    let source = create_memory(size)?;
    let at = map(s.process, source, size)?;
    for page in 0..pages {
        // SAFETY: the mapping is writable.
        unsafe { ((at + page * PAGE_SIZE) as *mut u64).write_volatile(page) };
    }
    // SAFETY: the call takes no pointers.
    check("unmapping the source", unsafe {
        sys::vm_unmap(s.process, at, size)
    })?;
    // SAFETY: the call takes no pointers.
    check("supplying", unsafe {
        sys::pager_supply(pager, mem, 0, size, source, 0)
    })?;
    let at = map(s.process, mem, size)?;
    for page in 0..pages {
        let page_at = (at + page * PAGE_SIZE) as *const u64;
        // SAFETY: the mapping is readable.
        ensure(
            "supplying every page",
            unsafe { page_at.read_volatile() } == page,
        )?;
        let left = u64::from_le_bytes(mem_read_at::<8>(source, page * PAGE_SIZE)?);
        ensure("taking every page from the source", left == 0)?;
    }
    ensure("supplying clean pages", dirty(pager, mem, size)?.1 == 0)?;
    for page in 0..pages {
        // SAFETY: the mapping is writable.
        unsafe { ((at + page * PAGE_SIZE + 8) as *mut u8).write_volatile(1) };
    }
    let (ranges, count) = dirty(pager, mem, size)?;
    ensure(
        "tracking every write",
        count == 1 && ranges[..2] == [0, size],
    )?;

    check("beginning writeback", pager_op(pager, mem, 3, 0, size))?;
    let written = [0, 700, pages - 1];
    for page in written {
        // SAFETY: the mapping is writable.
        unsafe { ((at + page * PAGE_SIZE + 8) as *mut u8).write_volatile(2) };
    }
    check("ending writeback", pager_op(pager, mem, 4, 0, size))?;
    let (ranges, count) = dirty(pager, mem, size)?;
    let want = written.map(|p| [p * PAGE_SIZE, PAGE_SIZE]);
    ensure(
        "keeping only pages written meanwhile",
        count == 3 && ranges[..6] == *want.as_flattened(),
    )?;

    // Every clean page is gone after the eviction, so reading it asks
    // the pager for it; the dirty pages stay.
    check("evicting", pager_op(pager, mem, 2, 0, size))?;
    PAGER.store(u64::from(pager), Ordering::Release);
    spawn2(s, pager_thread, u64::from(port), u64::from(mem))?;
    let before = REQUESTS.load(Ordering::Acquire);
    for page in written {
        let page_at = (at + page * PAGE_SIZE) as *const u64;
        // SAFETY: the mapping is readable.
        ensure(
            "keeping dirty pages",
            unsafe { page_at.read_volatile() } == page,
        )?;
    }
    for page in 0..pages {
        mem_read_at::<1>(mem, page * PAGE_SIZE)?;
    }
    ensure(
        "evicting every clean page",
        REQUESTS.load(Ordering::Acquire) == before + pages - 3,
    )?;
    // SAFETY: the calls take no pointers. Closing the port ends the
    // pager thread.
    unsafe {
        check("unmapping", sys::vm_unmap(s.process, at, size))?;
        for handle in [mem, source, port, pager] {
            check("closing", sys::handle_close(handle))?;
        }
    }
    Ok(())
}

/// Dirty tracking, writeback, eviction, failing, a pager's end and
/// revocation of paged objects.
fn pager_operations(s: &Startup) -> Result {
    let size = 3 * PAGE_SIZE;
    let (pager, mem) = paged_object(s, 3, 2)?;
    ensure(
        "reporting dirty tracking",
        u32_at(&info::<0x18>(mem, 4)?, 0x0C) == 2,
    )?;
    for page in 0..3 {
        mem_read_at::<1>(mem, page * PAGE_SIZE)?;
    }
    ensure("supplying clean pages", dirty(pager, mem, size)?.1 == 0)?;
    mem_write_at(mem, PAGE_SIZE, b"w")?;
    let at = map(s.process, mem, size)?;
    // SAFETY: the mapping is writable.
    unsafe { ((at + 2 * PAGE_SIZE) as *mut u8).write_volatile(b'm') };
    let (ranges, count) = dirty(pager, mem, size)?;
    ensure(
        "tracking writes",
        count == 1 && ranges[..2] == [PAGE_SIZE, 2 * PAGE_SIZE],
    )?;

    check("beginning writeback", pager_op(pager, mem, 3, 0, size))?;
    ensure(
        "allowing one writeback",
        pager_op(pager, mem, 3, 0, size) == BAD_STATE,
    )?;
    ensure(
        "naming the range",
        pager_op(pager, mem, 4, 0, PAGE_SIZE) == BAD_STATE,
    )?;
    // SAFETY: the mapping is writable.
    unsafe { ((at + PAGE_SIZE) as *mut u8).write_volatile(b'n') };
    check("ending writeback", pager_op(pager, mem, 4, 0, size))?;
    let (ranges, count) = dirty(pager, mem, size)?;
    ensure(
        "keeping pages written meanwhile",
        count == 1 && ranges[..2] == [PAGE_SIZE, PAGE_SIZE],
    )?;
    check("beginning again", pager_op(pager, mem, 3, 0, size))?;
    check("aborting", pager_op(pager, mem, 5, 0, size))?;
    ensure("staying dirty", dirty(pager, mem, size)?.1 == 1)?;

    let before = REQUESTS.load(Ordering::Acquire);
    check("evicting", pager_op(pager, mem, 2, 0, size))?;
    ensure(
        "keeping dirty pages",
        mem_read_at::<1>(mem, PAGE_SIZE)? == *b"n",
    )?;
    ensure(
        "requesting evicted pages again",
        mem_read_at::<6>(mem, 0)? == *b"page 0",
    )?;
    ensure(
        "counting the request",
        REQUESTS.load(Ordering::Acquire) == before + 1,
    )?;
    ensure(
        "refusing other ops",
        pager_op(pager, mem, 6, 0, size) == -10,
    )?;

    // SAFETY: the call takes no pointers.
    check("revoking", unsafe { sys::pager_revoke(pager, mem) })?;
    // SAFETY: the call takes no pointers.
    check("revoking again", unsafe { sys::pager_revoke(pager, mem) })?;
    let mut byte = [0u8; 1];
    // SAFETY: the pointers describe `byte`.
    unsafe {
        ensure(
            "refusing reads",
            sys::mem_read(mem, 0, byte.as_mut_ptr(), 1) == -13,
        )?;
        ensure(
            "refusing writes",
            sys::mem_write(mem, 0, byte.as_ptr(), 1) == -13,
        )?;
        let mut out = 0;
        let status = sys::vm_map(s.process, PERM_READ, 0, mem, 0, PAGE_SIZE, &mut out);
        ensure("refusing mappings", status == -13)?;
    }

    let (pager, mem) = paged_object(s, 2, 0)?;
    check("failing a page", pager_op(pager, mem, 1, 0, PAGE_SIZE))?;
    // SAFETY: the pointers describe `byte`.
    ensure(
        "failing reads",
        unsafe { sys::mem_read(mem, 0, byte.as_mut_ptr(), 1) } == IO,
    )?;
    ensure(
        "needing dirty tracking",
        unsafe { sys::pager_query_dirty(pager, mem, 0, PAGE_SIZE, byte.as_mut_ptr(), 0, &mut 0) }
            == -2,
    )?;
    // SAFETY: the call takes no pointers.
    check("closing the pager", unsafe { sys::handle_close(pager) })?;
    // SAFETY: the pointers describe `byte`.
    ensure(
        "failing without a pager",
        unsafe { sys::mem_read(mem, PAGE_SIZE, byte.as_mut_ptr(), 1) } == IO,
    )
}

const RESUME: u32 = 0;
const DEBUG_EVENTS: u32 = 1 << 0;
const UNDEFINED_INSTRUCTION: u32 = 3;
const THREAD_STARTING: u32 = 11;
const THREAD_EXITING: u32 = 12;

/// Starts the debuggee with `arg2` and an exception channel created with
/// `options`, and returns the process and the channel.
fn start_debuggee(s: &Startup, arg2: u64, options: u32) -> Result<(Handle, Handle)> {
    let mut channel = 0;
    let (process, _) = start_program_with(s, "debuggee", arg2, |process| {
        // SAFETY: the out pointer is valid for the call.
        check("creating an exception channel", unsafe {
            sys::exception_channel_create(process, options, &mut channel)
        })
    })?;
    Ok((process, channel))
}

/// The type of the next report on exception channel `channel` and the
/// handle of its thread.
fn next_report(channel: Handle) -> Result<(u32, Handle)> {
    let m = read_message(channel, 2000)?;
    ensure("receiving a report", m.len == 0x28 && m.count == 1)?;
    Ok((u32_at(&m.bytes, 0), m.handles[0]))
}

fn resume(thread: Handle) -> Result {
    // SAFETY: the call takes no pointers.
    check("resuming", unsafe {
        sys::thread_resume_from_exception(thread, RESUME)
    })
}

/// A system call from outside a process's gate is refused with a
/// `PROTECTION` exception at the instruction; a gate is set once, before
/// the process starts.
fn syscall_gate(s: &Startup) -> Result {
    const PROTECTION: u32 = 8;
    let mut channel = 0;
    let (process, _) = start_program_with(s, "debuggee", 1, |process| {
        // SAFETY: the out pointer is valid; the other calls take none.
        unsafe {
            check(
                "creating an exception channel",
                sys::exception_channel_create(process, 0, &mut channel),
            )?;
            ensure(
                "refusing an empty gate",
                sys::process_set_syscall_gate(process, 0x1000, 0) == INVALID_ARGS,
            )?;
            ensure(
                "refusing a gate past the top",
                sys::process_set_syscall_gate(process, u64::MAX, 2) == INVALID_ARGS,
            )?;
            // No code of the program lies in the first page.
            check(
                "setting the gate",
                sys::process_set_syscall_gate(process, 0, 0x1000),
            )?;
            ensure(
                "refusing a second gate",
                sys::process_set_syscall_gate(process, 0, 0x2000) == BAD_STATE,
            )
        }
    })?;
    let m = read_message(channel, 2000)?;
    ensure("receiving a report", m.len == 0x28 && m.count == 1)?;
    ensure(
        "reporting a protection fault",
        u32_at(&m.bytes, 0) == PROTECTION,
    )?;
    let address = u64_at(&m.bytes, 0x18);
    ensure(
        "naming the instruction",
        (0x1000..0x7FFF_FFFF_F000).contains(&address),
    )?;
    ensure("giving the #GP vector", u64_at(&m.bytes, 0x20) == 13)?;
    // SAFETY: the calls take no pointers.
    unsafe {
        ensure(
            "refusing a gate once started",
            sys::process_set_syscall_gate(process, 0, 0x1000) == BAD_STATE,
        )?;
        check("killing", sys::task_kill(process))?;
    }
    ensure(
        "terminating the process",
        wait(process, TERMINATED, in_time()).0 == 0,
    )
}

/// Starts the debuggee reading I/O port 0x80, after granting it that port
/// when `granted`, and returns the type of the exception it reports.
fn read_port_0x80(s: &Startup, granted: bool) -> Result<u32> {
    let mut channel = 0;
    let (process, _) = start_program_with(s, "debuggee", 3, |process| {
        // SAFETY: the out pointers are valid; the other call takes none.
        unsafe {
            check(
                "creating an exception channel",
                sys::exception_channel_create(process, 0, &mut channel),
            )?;
            if !granted {
                return Ok(());
            }
            let mut port = 0;
            let kind = mikros_test::KIND_IOPORT;
            let status = sys::resource_create(s.root_resource, kind, 0x80, 1, 0, &mut port);
            check("creating the port resource", status)?;
            check(
                "granting the port",
                sys::process_ioports_enable(process, port, 0x80, 1),
            )
        }
    })?;
    let (kind, _) = next_report(channel)?;
    // SAFETY: the call takes no pointers.
    check("killing", unsafe { sys::task_kill(process) })?;
    ensure(
        "terminating the process",
        wait(process, TERMINATED, in_time()).0 == 0,
    )?;
    Ok(kind)
}

/// A process may use only the I/O ports granted to it: reading another
/// one raises a `PROTECTION` exception, and reading a granted one goes on
/// to the next instruction.
fn ioports_as_granted(s: &Startup) -> Result {
    const PROTECTION: u32 = 8;
    ensure(
        "refusing an ungranted port",
        read_port_0x80(s, false)? == PROTECTION,
    )?;
    ensure(
        "allowing a granted port",
        read_port_0x80(s, true)? == UNDEFINED_INSTRUCTION,
    )
}

/// Killing a blocked thread whose process reports thread events reports
/// its exit, and its end as the last thread terminates the process.
fn kill_blocked_thread(s: &Startup) -> Result {
    let (process, channel) = start_debuggee(s, 1, DEBUG_EVENTS)?;
    let (kind, thread) = next_report(channel)?;
    ensure("reporting the start", kind == THREAD_STARTING)?;
    resume(thread)?;
    wait_until_blocked(thread)?;
    // SAFETY: the call takes no pointers.
    check("killing", unsafe { sys::task_kill(thread) })?;
    let (kind, exiting) = next_report(channel)?;
    ensure("reporting the exit", kind == THREAD_EXITING)?;
    resume(exiting)?;
    ensure(
        "terminating the process",
        wait(process, TERMINATED, in_time()).0 == 0,
    )
}

const PAGE_FAULT: u32 = 2;
const ARITHMETIC: u32 = 4;
const TERMINATE: u32 = 1;
const DELIVER_SIGNAL: u32 = 2;
const ALREADY_EXISTS: i64 = -26;

/// The bytes of the next report on exception channel `channel` and the
/// handle of its thread.
fn report_bytes(channel: Handle) -> Result<([u8; 0x28], Handle)> {
    let m = read_message(channel, 2000)?;
    ensure("receiving a report", m.len == 0x28 && m.count == 1)?;
    let mut bytes = [0; 0x28];
    bytes.copy_from_slice(&m.bytes[..0x28]);
    Ok((bytes, m.handles[0]))
}

fn act(thread: Handle, action: u32) -> i64 {
    // SAFETY: the call takes no pointers.
    unsafe { sys::thread_resume_from_exception(thread, action) }
}

/// The exit code of `process` once it has terminated.
fn exit_code(process: Handle) -> Result<i64> {
    ensure(
        "terminating the process",
        wait(process, TERMINATED, in_time()).0 == 0,
    )?;
    Ok(u64_at(&info::<0x20>(process, 2)?, 0) as i64)
}

/// A read of an unmapped address is reported as a `PAGE_FAULT` at that
/// address, and a division by zero as `ARITHMETIC` at the instruction,
/// with the IDs of the process and the thread and the vector and error
/// code. An undefined action is refused, `TERMINATE` ends the process with
/// exit code -2, a thread no longer in an exception refuses actions, a
/// second exception channel is refused while the first is open, and a
/// killed process ends with exit code -1.
fn exception_reports_and_actions(s: &Startup) -> Result {
    let (process, channel) = start_debuggee(s, 4, 0)?;
    let (r, thread) = report_bytes(channel)?;
    ensure("reporting a page fault", u32_at(&r, 0) == PAGE_FAULT)?;
    ensure(
        "naming the process and the thread",
        u64_at(&r, 0x08) == koid(process)? && u64_at(&r, 0x10) == koid(thread)?,
    )?;
    ensure("giving the faulting address", u64_at(&r, 0x18) == 0x1000)?;
    // A user read of a page that is not present: vector 14, error code 4.
    ensure(
        "giving the vector and error code",
        u64_at(&r, 0x20) == 14 | 4 << 32,
    )?;
    ensure(
        "refusing an undefined action",
        act(thread, 3) == INVALID_ARGS,
    )?;
    check("terminating", act(thread, TERMINATE))?;
    ensure("ending with exit code -2", exit_code(process)? == -2)?;
    ensure(
        "refusing a thread not in an exception",
        act(thread, RESUME) == BAD_STATE,
    )?;

    let (process, channel) = start_debuggee(s, 5, 0)?;
    let (r, _) = report_bytes(channel)?;
    ensure("reporting a division error", u32_at(&r, 0) == ARITHMETIC)?;
    ensure(
        "giving the instruction address",
        (0x1_0000..0x7FFF_FFFF_F000).contains(&u64_at(&r, 0x18)),
    )?;
    ensure("giving the vector", u64_at(&r, 0x20) == 0)?;
    let mut second = 0;
    // SAFETY: the out pointer is valid for the call.
    let status = unsafe { sys::exception_channel_create(process, 0, &mut second) };
    ensure("refusing a second channel", status == ALREADY_EXISTS)?;
    // SAFETY: the call takes no pointers.
    check("killing", unsafe { sys::task_kill(process) })?;
    ensure("ending with exit code -1", exit_code(process)? == -1)
}

/// A thread reports its start with its entry point, where it starts, and
/// its end through `thread_exit` with address 0; neither report takes
/// `DELIVER_SIGNAL`; and the process, whose last thread it was, ends with
/// exit code 0. The threads of a killed process report no exit.
fn thread_events(s: &Startup) -> Result {
    let (process, channel) = start_debuggee(s, 6, DEBUG_EVENTS)?;
    let (r, thread) = report_bytes(channel)?;
    ensure("reporting the start", u32_at(&r, 0) == THREAD_STARTING)?;
    let (status, regs) = general_registers(thread);
    check("reading the registers", status)?;
    ensure(
        "giving the entry point",
        u64_at(&r, 0x18) == u64_at(&regs, 16 * 8),
    )?;
    ensure(
        "refusing a signal for the start",
        act(thread, DELIVER_SIGNAL) == BAD_STATE,
    )?;
    resume(thread)?;
    let (r, exiting) = report_bytes(channel)?;
    ensure(
        "reporting the exit at address 0",
        u32_at(&r, 0) == THREAD_EXITING && u64_at(&r, 0x18) == 0,
    )?;
    ensure(
        "refusing a signal for the exit",
        act(exiting, DELIVER_SIGNAL) == BAD_STATE,
    )?;
    resume(exiting)?;
    ensure("ending with exit code 0", exit_code(process)? == 0)?;

    let (process, channel) = start_debuggee(s, 1, DEBUG_EVENTS)?;
    let (r, thread) = report_bytes(channel)?;
    ensure("reporting the start", u32_at(&r, 0) == THREAD_STARTING)?;
    resume(thread)?;
    wait_until_blocked(thread)?;
    // SAFETY: the call takes no pointers.
    check("killing", unsafe { sys::task_kill(process) })?;
    ensure("ending with exit code -1", exit_code(process)? == -1)?;
    ensure("reporting no exit", read_message(channel, 50).is_err())
}

/// A thread of a new process cannot be started on its own, a start with
/// an entry or a stack outside the user range is refused and keeps the
/// argument handle and the states, and a terminated process takes no new
/// thread and no start.
fn start_refusals(s: &Startup) -> Result {
    let name = b"refused";
    let len = name.len() as u64;
    let (mut process, mut thread, mut event) = (0, 0, 0);
    // SAFETY: the pointers are valid for the calls.
    unsafe {
        check(
            "creating a process",
            sys::process_create(s.budget, name.as_ptr(), len, &mut process),
        )?;
        check(
            "creating a thread",
            sys::thread_create(process, name.as_ptr(), len, &mut thread),
        )?;
        check("creating an event", sys::event_create(0, &mut event))?;
    }
    let (user, kernel) = (0x10_0000, 0xFFFF_8000_0000_0000);
    // SAFETY: the calls take no pointers.
    unsafe {
        ensure(
            "refusing a thread of a new process",
            sys::thread_start(thread, user, user, 0, 0) == BAD_STATE,
        )?;
        ensure(
            "refusing an entry in the kernel half",
            sys::process_start(process, thread, kernel, user, event, 0) == INVALID_ARGS,
        )?;
        ensure(
            "refusing a stack in the kernel half",
            sys::process_start(process, thread, user, kernel, event, 0) == INVALID_ARGS,
        )?;
    }
    ensure("keeping the argument handle", koid(event).is_ok())?;
    ensure(
        "keeping the process new",
        u32_at(&info::<0x20>(process, 2)?, 0x08) == 0,
    )?;
    ensure("keeping the thread new", thread_state(thread)? == 0)?;
    // SAFETY: the call takes no pointers.
    check("killing", unsafe { sys::task_kill(process) })?;
    ensure(
        "terminating the process",
        wait(process, TERMINATED, in_time()).0 == 0,
    )?;
    let mut other = 0;
    // SAFETY: the pointers are valid for the calls.
    unsafe {
        ensure(
            "refusing a thread of a terminated process",
            sys::thread_create(process, name.as_ptr(), len, &mut other) == BAD_STATE,
        )?;
        ensure(
            "refusing to start a terminated process",
            sys::process_start(process, thread, user, user, 0, 0) == BAD_STATE,
        )?;
    }
    for h in [process, thread, event] {
        // SAFETY: the call takes no pointers.
        check("closing", unsafe { sys::handle_close(h) })?;
    }
    Ok(())
}

/// Killing the last thread of a process while it is stopped in an
/// exception terminates the process.
fn kill_stopped_thread(s: &Startup) -> Result {
    let (process, channel) = start_debuggee(s, 0, 0)?;
    let (kind, thread) = next_report(channel)?;
    ensure("reporting the fault", kind == UNDEFINED_INSTRUCTION)?;
    // SAFETY: the call takes no pointers.
    check("killing", unsafe { sys::task_kill(thread) })?;
    ensure(
        "terminating the process",
        wait(process, TERMINATED, in_time()).0 == 0,
    )
}

/// A channel's own endpoints cannot be sent through it, by a write or a
/// call, under any handle value.
fn own_endpoints_not_sent(_: &Startup) -> Result {
    let (mut a, mut b) = (0, 0);
    // SAFETY: the out pointers are valid for the call.
    check("creating a channel", unsafe {
        sys::channel_create(0, &mut a, &mut b)
    })?;
    let mut copy = 0;
    // SAFETY: the out pointer is valid for the call.
    check("duplicating", unsafe {
        sys::handle_duplicate(a, 0x8000_0000, &mut copy)
    })?;
    let byte = [1u8];
    for (what, h) in [("the writing end", copy), ("its peer", b)] {
        // SAFETY: the pointers describe `byte` and `h`.
        let status = unsafe { sys::channel_write(a, byte.as_ptr(), 1, &h, 1) };
        ensure(what, status == INVALID_ARGS)?;
        ensure(what, call(a, &byte, Some(h), 64, soon()).0 == INVALID_ARGS)?;
    }
    ensure("keeping the handles", read_message(b, 1).is_err())?;
    for h in [a, b, copy] {
        // SAFETY: the call takes no pointers.
        check("closing", unsafe { sys::handle_close(h) })?;
    }
    Ok(())
}

const BAD_HANDLE: i64 = -11;
const OUT_OF_RANGE: i64 = -14;
const BUFFER_TOO_SMALL: i64 = -15;
const PEER_CLOSED: i64 = -24;

/// Reads the next message of `channel` into buffers of `cap` bytes and
/// `handles_cap` handles, and returns the status and the two sizes.
fn read_sizes(channel: Handle, cap: usize, handles_cap: usize) -> (i64, u64, u64) {
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

/// A refused `channel_write` queues nothing and keeps the caller's
/// handles; a written handle leaves the caller's table and arrives with
/// the rights it had; a read into buffers that are too small stores the
/// sizes and leaves the message queued; and a closed peer is reported.
fn channel_errors(s: &Startup) -> Result {
    let (mut a, mut b, mut event) = (0, 0, 0);
    // SAFETY: the out pointers are valid for the calls.
    unsafe {
        check("creating a channel", sys::channel_create(0, &mut a, &mut b))?;
        check("creating an event", sys::event_create(0, &mut event))?;
    }
    // 65 handles, one more than a message holds, each transferable.
    let mut many = [0 as Handle; 65];
    for h in &mut many {
        // SAFETY: the out pointer is valid for the call.
        check("duplicating", unsafe {
            sys::handle_duplicate(event, 0x8000_0000, h)
        })?;
    }
    let big = 17 * PAGE_SIZE;
    let at = map(s.process, create_memory(big)?, big)?;
    // SAFETY: the bytes and handles lie in mapped memory.
    let refused = unsafe {
        [
            (
                "refusing 65537 bytes",
                sys::channel_write(a, at as *const u8, 65537, core::ptr::null(), 0),
                OUT_OF_RANGE,
            ),
            (
                "refusing 65 handles",
                sys::channel_write(a, at as *const u8, 1, many.as_ptr(), 65),
                OUT_OF_RANGE,
            ),
            (
                "refusing a handle twice",
                sys::channel_write(a, at as *const u8, 1, [event, event].as_ptr(), 2),
                INVALID_ARGS,
            ),
            (
                "refusing the channel's own handle",
                sys::channel_write(a, at as *const u8, 1, &a, 1),
                INVALID_ARGS,
            ),
        ]
    };
    for (what, status, expected) in refused {
        ensure(what, status == expected)?;
    }
    ensure("queueing nothing", read_sizes(b, 16, 2).0 == SHOULD_WAIT)?;
    for h in many.into_iter().chain([event]) {
        // SAFETY: the call takes no pointers.
        check("keeping the handles", unsafe { sys::handle_close(h) })?;
    }

    let mut sent = 0;
    let rights = (1 << 0) | (1 << 1) | (1 << 9);
    // SAFETY: the out pointer is valid for the call.
    check("creating a handle to send", unsafe {
        sys::event_create(0, &mut event)
    })?;
    // SAFETY: the out pointer is valid for the call.
    check("restricting its rights", unsafe {
        sys::handle_replace(event, rights, &mut sent)
    })?;
    // SAFETY: the pointers describe the message and the handle.
    check("writing", unsafe {
        sys::channel_write(a, b"eight by".as_ptr(), 8, &sent, 1)
    })?;
    ensure(
        "moving the handle out of the table",
        // SAFETY: the call takes no pointers.
        unsafe { sys::handle_close(sent) } == BAD_HANDLE,
    )?;
    for (cap, handles_cap) in [(4, 1), (8, 0)] {
        ensure(
            "storing the sizes of a message too large",
            read_sizes(b, cap, handles_cap) == (BUFFER_TOO_SMALL, 8, 1),
        )?;
    }
    let m = read_message(b, 1)?;
    ensure(
        "keeping the message",
        &m.bytes[..m.len] == b"eight by" && m.count == 1,
    )?;
    ensure(
        "keeping the rights",
        u32_at(&info::<0x18>(m.handles[0], 1)?, 20) == rights,
    )?;
    // SAFETY: the calls take no pointers.
    unsafe {
        check(
            "closing the received handle",
            sys::handle_close(m.handles[0]),
        )?;
        check("closing one end", sys::handle_close(a))?;
    }
    ensure(
        "reporting the closed peer",
        read_sizes(b, 16, 2).0 == PEER_CLOSED,
    )?;
    ensure(
        "refusing to write to a closed peer",
        // SAFETY: the pointer describes one byte.
        unsafe { sys::channel_write(b, b"x".as_ptr(), 1, core::ptr::null(), 0) } == PEER_CLOSED,
    )?;
    // SAFETY: the call takes no pointers.
    check("closing the other end", unsafe { sys::handle_close(b) })
}

/// Two channels whose endpoints are only carried by messages queued
/// toward each other are destroyed once their other handles are closed,
/// and give back their kernel memory.
fn endpoint_cycles_reclaimed(s: &Startup) -> Result {
    let (mut a, mut b, mut c, mut d) = (0, 0, 0, 0);
    // Handle table slots stay charged once made, so the four that the
    // test uses are made first.
    // SAFETY: the out pointers are valid for the calls.
    unsafe {
        check("creating a channel", sys::channel_create(0, &mut a, &mut b))?;
        check("creating a channel", sys::channel_create(0, &mut c, &mut d))?;
    }
    for h in [a, b, c, d] {
        // SAFETY: the call takes no pointers.
        check("closing", unsafe { sys::handle_close(h) })?;
    }
    let before = budget_kernel(s)?;
    // SAFETY: the out pointers are valid for the calls.
    unsafe {
        check("creating a channel", sys::channel_create(0, &mut a, &mut b))?;
        check("creating a channel", sys::channel_create(0, &mut c, &mut d))?;
    }
    let byte = [1u8];
    // SAFETY: the pointers describe `byte` and the handles.
    unsafe {
        check("sending d", sys::channel_write(a, byte.as_ptr(), 1, &d, 1))?;
        check("sending b", sys::channel_write(c, byte.as_ptr(), 1, &b, 1))?;
    }
    ensure("charging the channels", budget_kernel(s)? > before)?;
    for h in [a, c] {
        // SAFETY: the call takes no pointers.
        check("closing", unsafe { sys::handle_close(h) })?;
    }
    ensure("reclaiming the channels", budget_kernel(s)? == before)
}

/// The thread state of a thread stopped in an exception.
const STATE_EXCEPTION: u32 = 3;

/// Fails unless nothing arrives on `channel` for a moment.
fn no_report(channel: Handle) -> Result {
    ensure("sending no report", read_message(channel, 20).is_err())
}

/// Killing a thread that was never started ends it at once, without a
/// report, even when its process asks for thread events.
fn kill_new_thread(s: &Startup) -> Result {
    let (process, channel) = start_debuggee(s, 1, DEBUG_EVENTS)?;
    let (kind, first) = next_report(channel)?;
    ensure("reporting the start", kind == THREAD_STARTING)?;
    resume(first)?;
    let mut thread = 0;
    let name = b"unstarted";
    // SAFETY: the pointers describe `name` and `thread`.
    check("creating a thread", unsafe {
        sys::thread_create(process, name.as_ptr(), name.len() as u64, &mut thread)
    })?;
    // SAFETY: the call takes no pointers.
    check("killing", unsafe { sys::task_kill(thread) })?;
    ensure("ending the thread", wait(thread, TERMINATED, soon()).0 == 0)?;
    no_report(channel)?;
    // SAFETY: the call takes no pointers.
    check("killing the process", unsafe { sys::task_kill(process) })?;
    ensure(
        "terminating the process",
        wait(process, TERMINATED, in_time()).0 == 0,
    )
}

/// Killing a thread whose exception report is still queued removes the
/// report; with thread events the exit is reported next, and a kill
/// while the exit report waits changes nothing.
fn kill_thread_with_queued_report(s: &Startup) -> Result {
    let (process, channel) = start_debuggee(s, 0, DEBUG_EVENTS)?;
    let (kind, thread) = next_report(channel)?;
    ensure("reporting the start", kind == THREAD_STARTING)?;
    resume(thread)?;
    wait_for_state(thread, STATE_EXCEPTION)?;
    // SAFETY: the call takes no pointers.
    check("killing", unsafe { sys::task_kill(thread) })?;
    let (kind, exiting) = next_report(channel)?;
    ensure("reporting the exit", kind == THREAD_EXITING)?;
    // SAFETY: the call takes no pointers.
    check("killing again", unsafe { sys::task_kill(exiting) })?;
    ensure(
        "staying in the exit report",
        thread_state(exiting)? == STATE_EXCEPTION,
    )?;
    resume(exiting)?;
    ensure(
        "terminating the process",
        wait(process, TERMINATED, in_time()).0 == 0,
    )?;
    no_report(channel)?;
    // Without thread events, the thread is found through its process.
    let (process, channel) = start_debuggee(s, 0, 0)?;
    let (koids, count) = thread_list(process)?;
    ensure("listing the thread", count == 1)?;
    let mut thread = 0;
    // SAFETY: the out pointer is valid for the call.
    check("getting the thread", unsafe {
        sys::process_get_thread(process, koids[0], &mut thread)
    })?;
    wait_for_state(thread, STATE_EXCEPTION)?;
    // SAFETY: the call takes no pointers.
    check("killing", unsafe { sys::task_kill(thread) })?;
    ensure(
        "terminating the process",
        wait(process, TERMINATED, in_time()).0 == 0,
    )?;
    no_report(channel)
}

/// Killing a thread with thread events while it is stopped in an
/// exception whose report was read reports its exit next.
fn kill_thread_after_report(s: &Startup) -> Result {
    let (process, channel) = start_debuggee(s, 0, DEBUG_EVENTS)?;
    let (kind, thread) = next_report(channel)?;
    ensure("reporting the start", kind == THREAD_STARTING)?;
    resume(thread)?;
    let (kind, faulting) = next_report(channel)?;
    ensure("reporting the fault", kind == UNDEFINED_INSTRUCTION)?;
    // SAFETY: the call takes no pointers.
    check("killing", unsafe { sys::task_kill(faulting) })?;
    let (kind, exiting) = next_report(channel)?;
    ensure("reporting the exit", kind == THREAD_EXITING)?;
    resume(exiting)?;
    ensure(
        "terminating the process",
        wait(process, TERMINATED, in_time()).0 == 0,
    )
}

/// A data breakpoint hit by loading SS just before `SYSCALL` raises its
/// debug exception at the first instruction of the kernel's entry, and is
/// reported as a `DEBUG` exception of the thread.
fn debug_at_syscall_entry(s: &Startup) -> Result {
    const BREAKPOINT: u32 = 6;
    const DEBUG: u32 = 7;
    let (process, channel) = start_debuggee(s, 2, 0)?;
    let (kind, thread) = next_report(channel)?;
    ensure("stopping at the breakpoint", kind == BREAKPOINT)?;
    let (status, regs) = general_registers(thread);
    check("reading the registers", status)?;
    let selector = u64_at(&regs, 12 * 8);
    // Breakpoint 0 on reads and writes of the two bytes at the selector.
    let dr7: u64 = 1 | 0b11 << 16 | 0b01 << 18;
    let mut debug = [0u8; 48];
    debug[..8].copy_from_slice(&selector.to_le_bytes());
    debug[40..].copy_from_slice(&dr7.to_le_bytes());
    // SAFETY: the pointer and length describe `debug`.
    check("setting the breakpoint", unsafe {
        sys::thread_write_state(thread, 2, debug.as_ptr(), 48)
    })?;
    resume(thread)?;
    let (kind, thread) = next_report(channel)?;
    ensure("reporting the debug exception", kind == DEBUG)?;
    // SAFETY: the call takes no pointers.
    check("killing", unsafe { sys::task_kill(thread) })?;
    ensure(
        "terminating the process",
        wait(process, TERMINATED, in_time()).0 == 0,
    )
}

/// In a fuzz build, coverage goes only into an anonymous object and
/// records the kernel's blocks while it is on.
#[cfg(feature = "fuzz")]
fn coverage(s: &Startup) -> Result {
    let (_pager, paged) = paged_object(s, 1, 0)?;
    // SAFETY: the call takes no pointers.
    let status = unsafe { sys::fuzz_cov_enable(paged) };
    ensure("refusing a paged object", status == -12)?;
    let mem = create_memory(4 * PAGE_SIZE)?;
    // SAFETY: the calls take no pointers or a valid one.
    unsafe {
        check("enabling coverage", sys::fuzz_cov_enable(mem))?;
        now();
        check("disabling coverage", sys::fuzz_cov_disable())?;
    }
    let count = u64::from_le_bytes(mem_read_at::<8>(mem, 0)?);
    ensure("recording blocks", count > 0)
}

/// Outside fuzz builds the numbers of `fuzz_cov_enable` and
/// `fuzz_cov_disable`, 45 and 46, are not assigned, so the bindings leave
/// the calls out and the test makes them by number.
#[cfg(not(feature = "fuzz"))]
fn coverage(_: &Startup) -> Result {
    /// The status of an unassigned number.
    const BAD_SYSCALL: i64 = -5;
    let mem = create_memory(PAGE_SIZE)?;
    for number in [45u64, 46] {
        let status: i64;
        // SAFETY: SYSCALL changes only RAX, RCX and R11, and an
        // unassigned number reads no argument.
        unsafe {
            core::arch::asm!(
                "syscall",
                inlateout("rax") number => status,
                in("rdi") u64::from(mem),
                out("rcx") _,
                out("r11") _,
                options(nostack),
            )
        };
        ensure("refusing a fuzz-only call", status == BAD_SYSCALL)?;
    }
    check("closing the object", unsafe { sys::handle_close(mem) })
}

/// Set by `straddling_reader` once its read returned.
static READ_DONE: AtomicU64 = AtomicU64::new(0);

/// Answers one page request on `port` for `mem` after 50 ms.
extern "sysv64" fn slow_pager_thread(port: u64, mem: u64) -> ! {
    let pager = PAGER.load(Ordering::Acquire) as Handle;
    let (status, p) = take_packet(port as Handle, now() + 1_000_000_000);
    if status == 0 {
        sleep_ms(50);
        if let Ok(source) = create_memory(PAGE_SIZE) {
            // SAFETY: the call takes no pointers.
            unsafe {
                sys::pager_supply(pager, mem as Handle, u64_at(&p, 24), PAGE_SIZE, source, 0)
            };
        }
    }
    // SAFETY: the call takes no pointers.
    unsafe { sys::thread_exit() };
    unreachable!("thread_exit returned");
}

/// Reads 16 bytes of `source` to `at`, then sets `READ_DONE`.
extern "sysv64" fn straddling_reader(source: u64, at: u64) -> ! {
    // SAFETY: the destination lies in two writable mappings.
    unsafe { sys::mem_read(source as Handle, 0, at as *mut u8, 16) };
    READ_DONE.store(1, Ordering::Release);
    // SAFETY: the call takes no pointers.
    unsafe { sys::thread_exit() };
    unreachable!("thread_exit returned");
}

/// Maps `first` and, right after it, a page of a paged object whose
/// pager answers after 50 ms, and starts a thread reading 16 bytes into
/// the last 8 bytes of `first` and the first 8 of the paged page, which
/// then holds `first` while it waits for the pager. Returns where
/// `first` is mapped once the thread waits.
fn start_straddling_read(s: &Startup, first: Handle) -> Result<u64> {
    let (mut pager, mut port, mut paged) = (0, 0, 0);
    // SAFETY: the pointers are valid for the calls.
    unsafe {
        check("creating a pager", sys::pager_create(0, &mut pager))?;
        check("creating a port", sys::port_create(0, &mut port))?;
        let status = sys::pager_create_memory(pager, port, 7, PAGE_SIZE, 0, &mut paged);
        check("creating a paged object", status)?;
    }
    PAGER.store(u64::from(pager), Ordering::Release);
    spawn2(s, slow_pager_thread, u64::from(port), u64::from(paged))?;
    // Find two free pages, then map `first` and the paged object there.
    let at = map(s.process, create_memory(2 * PAGE_SIZE)?, 2 * PAGE_SIZE)?;
    // SAFETY: the call takes no pointers.
    check("freeing the two pages", unsafe {
        sys::vm_unmap(s.process, at, 2 * PAGE_SIZE)
    })?;
    let mut mapped = 0;
    // SAFETY: the out pointer is valid for the call.
    let status = unsafe {
        sys::vm_map(
            s.process,
            PERM_READ | PERM_WRITE | mikros_test::SPECIFIC,
            at,
            first,
            0,
            PAGE_SIZE,
            &mut mapped,
        )
    };
    check("mapping the object", status)?;
    // SAFETY: the out pointer is valid for the call.
    let status = unsafe {
        sys::vm_map(
            s.process,
            PERM_READ | PERM_WRITE | mikros_test::SPECIFIC,
            at + PAGE_SIZE,
            paged,
            0,
            PAGE_SIZE,
            &mut mapped,
        )
    };
    check("mapping the paged object next to it", status)?;
    let source = create_memory(PAGE_SIZE)?;
    mem_write_at(source, 0, b"0123456789abcdef")?;
    READ_DONE.store(0, Ordering::Release);
    let reader = spawn2(s, straddling_reader, u64::from(source), at + PAGE_SIZE - 8)?;
    wait_until_blocked(reader)?;
    Ok(at)
}

/// A write seal waits for a kernel write into the object that is under
/// way, even after the object's mapping is gone.
fn seal_waits_for_outputs(s: &Startup) -> Result {
    let plain = create_memory(PAGE_SIZE)?;
    let at = start_straddling_read(s, plain)?;
    // SAFETY: the call takes no pointers.
    check("unmapping", unsafe {
        sys::vm_unmap(s.process, at, PAGE_SIZE)
    })?;
    check("sealing", seal(plain, 1 << 3))?;
    ensure(
        "waiting for the write",
        READ_DONE.load(Ordering::Acquire) == 1,
    )?;
    ensure(
        "keeping the written bytes",
        mem_read_at::<8>(plain, PAGE_SIZE - 8)? == *b"01234567",
    )
}

/// Eviction waits for a kernel write into the object that is under way,
/// also without dirty tracking.
fn evict_waits_for_outputs(s: &Startup) -> Result {
    let (mut pager, mut port, mut mem) = (0, 0, 0);
    // SAFETY: the pointers are valid for the calls.
    unsafe {
        check("creating a pager", sys::pager_create(0, &mut pager))?;
        check("creating a port", sys::port_create(0, &mut port))?;
        let status = sys::pager_create_memory(pager, port, 8, PAGE_SIZE, 0, &mut mem);
        check("creating a paged object", status)?;
    }
    let source = create_memory(PAGE_SIZE)?;
    // SAFETY: the call takes no pointers.
    check("supplying its page", unsafe {
        sys::pager_supply(pager, mem, 0, PAGE_SIZE, source, 0)
    })?;
    start_straddling_read(s, mem)?;
    check("evicting", pager_op(pager, mem, 2, 0, PAGE_SIZE))?;
    ensure(
        "waiting for the write",
        READ_DONE.load(Ordering::Acquire) == 1,
    )
}

/// Supplying pages from an object waits for a kernel write into it that
/// is under way, and moves the pages with the written bytes.
fn supply_waits_for_outputs(s: &Startup) -> Result {
    let plain = create_memory(PAGE_SIZE)?;
    let at = start_straddling_read(s, plain)?;
    // SAFETY: the call takes no pointers.
    check("unmapping", unsafe {
        sys::vm_unmap(s.process, at, PAGE_SIZE)
    })?;
    let (mut pager, mut port, mut mem) = (0, 0, 0);
    // SAFETY: the pointers are valid for the calls.
    unsafe {
        check("creating a pager", sys::pager_create(0, &mut pager))?;
        check("creating a port", sys::port_create(0, &mut port))?;
        let status = sys::pager_create_memory(pager, port, 9, PAGE_SIZE, 0, &mut mem);
        check("creating a paged object", status)?;
    }
    // SAFETY: the call takes no pointers.
    check("supplying", unsafe {
        sys::pager_supply(pager, mem, 0, PAGE_SIZE, plain, 0)
    })?;
    ensure(
        "waiting for the write",
        READ_DONE.load(Ordering::Acquire) == 1,
    )?;
    ensure(
        "moving the written bytes",
        mem_read_at::<8>(mem, PAGE_SIZE - 8)? == *b"01234567",
    )
}

/// A clone of an object waits for a kernel write into it that is under
/// way, and holds the written bytes.
fn clone_waits_for_outputs(s: &Startup) -> Result {
    let plain = create_memory(PAGE_SIZE)?;
    start_straddling_read(s, plain)?;
    let mut clone = 0;
    // SAFETY: the out pointer is valid for the call.
    check("cloning", unsafe {
        sys::mem_clone(plain, 0, 0, PAGE_SIZE, &mut clone)
    })?;
    ensure(
        "waiting for the write",
        READ_DONE.load(Ordering::Acquire) == 1,
    )?;
    ensure(
        "cloning the written bytes",
        mem_read_at::<8>(clone, PAGE_SIZE - 8)? == *b"01234567",
    )
}

/// The process and the permissions `waiting_mapper` maps with, and the
/// status its `vm_map` returned.
static MAPPER: [AtomicU64; 3] = [AtomicU64::new(0), AtomicU64::new(0), AtomicU64::new(0)];

/// Maps a page of `mem` as `MAPPER` says, with its output at `out`,
/// stores the status in `MAPPER` and exits.
extern "sysv64" fn waiting_mapper(mem: u64, out: u64) -> ! {
    let process = MAPPER[0].load(Ordering::Acquire) as Handle;
    let perms = MAPPER[1].load(Ordering::Acquire) as u32;
    // SAFETY: `out` lies in a writable mapping.
    let status = unsafe {
        sys::vm_map(
            process,
            perms,
            0,
            mem as Handle,
            0,
            PAGE_SIZE,
            out as *mut u64,
        )
    };
    MAPPER[2].store(status as u64, Ordering::Release);
    // SAFETY: the call takes no pointers.
    unsafe { sys::thread_exit() };
    unreachable!("thread_exit returned");
}

/// Starts a thread mapping a page of `mem` with `perms`, whose output
/// lies on a page of a paged object that its pager supplies after 50 ms,
/// and returns the thread once it waits for that page.
fn start_waiting_map(s: &Startup, mem: Handle, perms: u32, key: u64) -> Result<Handle> {
    let (mut pager, mut port, mut paged) = (0, 0, 0);
    // SAFETY: the pointers are valid for the calls.
    unsafe {
        check("creating a pager", sys::pager_create(0, &mut pager))?;
        check("creating a port", sys::port_create(0, &mut port))?;
        let status = sys::pager_create_memory(pager, port, key, PAGE_SIZE, 0, &mut paged);
        check("creating a paged object", status)?;
    }
    PAGER.store(u64::from(pager), Ordering::Release);
    spawn2(s, slow_pager_thread, u64::from(port), u64::from(paged))?;
    let out = map(s.process, paged, PAGE_SIZE)?;
    MAPPER[0].store(u64::from(s.process), Ordering::Release);
    MAPPER[1].store(u64::from(perms), Ordering::Release);
    let mapper = spawn2(s, waiting_mapper, u64::from(mem), out)?;
    wait_until_blocked(mapper)?;
    Ok(mapper)
}

/// A seal or a revocation that comes while `vm_map` waits to prepare its
/// output makes the call fail when it goes on.
fn map_checks_again_after_waiting(s: &Startup) -> Result {
    let plain = create_memory(PAGE_SIZE)?;
    let mapper = start_waiting_map(s, plain, PERM_READ | PERM_WRITE, 10)?;
    check("sealing meanwhile", seal(plain, 1 << 3))?;
    ensure(
        "ending the mapping",
        wait(mapper, TERMINATED, in_time()).0 == 0,
    )?;
    ensure(
        "refusing a writable mapping of a sealed object",
        MAPPER[2].load(Ordering::Acquire) as i64 == ACCESS_DENIED,
    )?;

    let (mut pager, mut port, mut paged) = (0, 0, 0);
    // SAFETY: the pointers are valid for the calls.
    unsafe {
        check("creating a pager", sys::pager_create(0, &mut pager))?;
        check("creating a port", sys::port_create(0, &mut port))?;
        let status = sys::pager_create_memory(pager, port, 11, PAGE_SIZE, 0, &mut paged);
        check("creating a paged object", status)?;
    }
    let mapper = start_waiting_map(s, paged, PERM_READ, 12)?;
    // SAFETY: the call takes no pointers.
    check("revoking meanwhile", unsafe {
        sys::pager_revoke(pager, paged)
    })?;
    ensure(
        "ending the mapping",
        wait(mapper, TERMINATED, in_time()).0 == 0,
    )?;
    ensure(
        "refusing a mapping of a revoked object",
        MAPPER[2].load(Ordering::Acquire) as i64 == ACCESS_DENIED,
    )
}

/// The pages of the source of `supply_beside`: more than the kernel
/// supplies in one piece, so that other threads run in between.
const SUPPLIED_PAGES: u64 = 4 * 512 + 1;

/// Set when the supply of `supply_beside` is about to begin, and the
/// first 8 bytes of the last page of its source as `sealer` or
/// `revoker` read them right after their call returned.
static SUPPLY_BEGUN: AtomicU64 = AtomicU64::new(0);
static SEEN_AFTER: AtomicU64 = AtomicU64::new(0);

/// Records the start of the last page of the supply's `source`.
fn see_last_page(source: Handle) {
    let seen = mem_read_at::<8>(source, (SUPPLIED_PAGES - 1) * PAGE_SIZE);
    SEEN_AFTER.store(seen.map_or(u64::MAX, u64::from_le_bytes), Ordering::Release);
}

/// Seals `source` with `WRITE` once the supply has begun, then records
/// its last page.
extern "sysv64" fn sealer(source: u64, _: u64) -> ! {
    while SUPPLY_BEGUN.load(Ordering::Acquire) == 0 {
        core::hint::spin_loop();
    }
    if seal(source as Handle, 1 << 3) == 0 {
        see_last_page(source as Handle);
    }
    // SAFETY: the call takes no pointers.
    unsafe { sys::thread_exit() };
    unreachable!("thread_exit returned");
}

/// Revokes `paged` of the pager in `PAGER` once the supply has begun,
/// then records the last page of `source`.
extern "sysv64" fn revoker(paged: u64, source: u64) -> ! {
    while SUPPLY_BEGUN.load(Ordering::Acquire) == 0 {
        core::hint::spin_loop();
    }
    let pager = PAGER.load(Ordering::Acquire) as Handle;
    // SAFETY: the call takes no pointers.
    if unsafe { sys::pager_revoke(pager, paged as Handle) } == 0 {
        see_last_page(source as Handle);
    }
    // SAFETY: the call takes no pointers.
    unsafe { sys::thread_exit() };
    unreachable!("thread_exit returned");
}

/// Supplies a new paged object from a source of `SUPPLIED_PAGES` pages
/// while another thread seals the source, when `sealing`, or revokes the
/// object, and checks that the supply did not change the source after
/// that call returned: it ended first, or it was refused.
fn supply_beside(s: &Startup, sealing: bool) -> Result {
    let len = SUPPLIED_PAGES * PAGE_SIZE;
    let source = create_memory(len)?;
    mem_write_at(source, len - PAGE_SIZE, b"lastpage")?;
    let (mut pager, mut port, mut paged) = (0, 0, 0);
    // SAFETY: the pointers are valid for the calls.
    unsafe {
        check("creating a pager", sys::pager_create(0, &mut pager))?;
        check("creating a port", sys::port_create(0, &mut port))?;
        let status = sys::pager_create_memory(pager, port, 13, len, 0, &mut paged);
        check("creating a paged object", status)?;
    }
    PAGER.store(u64::from(pager), Ordering::Release);
    SUPPLY_BEGUN.store(0, Ordering::Release);
    SEEN_AFTER.store(u64::MAX - 1, Ordering::Release);
    let helper = match sealing {
        true => spawn2(s, sealer, u64::from(source), 0)?,
        false => spawn2(s, revoker, u64::from(paged), u64::from(source))?,
    };
    SUPPLY_BEGUN.store(1, Ordering::Release);
    // SAFETY: the call takes no pointers.
    let status = unsafe { sys::pager_supply(pager, paged, 0, len, source, 0) };
    ensure(
        "supplying or refusing the whole call",
        status == 0 || status == ACCESS_DENIED,
    )?;
    ensure(
        "ending the helper",
        wait(helper, TERMINATED, in_time()).0 == 0,
    )?;
    let last = u64::from_le_bytes(mem_read_at::<8>(source, len - PAGE_SIZE)?);
    ensure(
        "leaving the source as it was when the call returned",
        SEEN_AFTER.load(Ordering::Acquire) == last,
    )?;
    for handle in [source, paged, port, pager] {
        // SAFETY: the call takes no pointers.
        check("closing", unsafe { sys::handle_close(handle) })?;
    }
    Ok(())
}

/// A `WRITE` seal of a source and a revocation of the paged object wait
/// for a supply that is under way.
fn seal_and_revoke_wait_for_supply(s: &Startup) -> Result {
    supply_beside(s, true)?;
    supply_beside(s, false)
}

type Test = fn(&Startup) -> Result;

const SUSPENDED: u32 = 1 << 5;
const STATE_BLOCKED: u32 = 2;
const STATE_SUSPENDED: u32 = 5;

/// The state of `thread`.
fn thread_state(thread: Handle) -> Result<u32> {
    Ok(u32_at(&info::<0x18>(thread, 3)?, 0))
}

fn suspend(thread: Handle) -> i64 {
    // SAFETY: the call takes no pointers.
    unsafe { sys::thread_suspend(thread) }
}

fn resume_thread(thread: Handle) -> i64 {
    // SAFETY: the call takes no pointers.
    unsafe { sys::thread_resume(thread) }
}

/// Suspends `thread` and waits until it is suspended.
fn suspend_and_wait(thread: Handle) -> Result {
    check("suspending", suspend(thread))?;
    let (status, _) = wait(thread, SUSPENDED, now() + 1_000_000_000);
    check("becoming suspended", status)?;
    ensure("being SUSPENDED", thread_state(thread)? == STATE_SUSPENDED)
}

/// Reads the general registers of `thread`.
fn general_registers(thread: Handle) -> (i64, [u8; 160]) {
    let mut regs = [0u8; 160];
    // SAFETY: the pointer and length describe `regs`.
    let status = unsafe { sys::thread_read_state(thread, 0, regs.as_mut_ptr(), 160) };
    (status, regs)
}

/// The loops the spinning thread has made.
static SPINS: AtomicU64 = AtomicU64::new(0);

extern "sysv64" fn spin(_: u64, _: u64) -> ! {
    loop {
        SPINS.fetch_add(1, Ordering::AcqRel);
    }
}

/// A thread that often blocked after running briefly still runs soon
/// after waking while another thread spins.
fn sleeper_runs_beside_spinner(s: &Startup) -> Result {
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

/// A suspended thread stops running until every suspension is undone,
/// shows its registers, and can be killed.
fn suspend_threads(s: &Startup) -> Result {
    let mut fresh = 0;
    let name = b"fresh";
    // SAFETY: the pointers describe `name` and `fresh`.
    check("creating a thread", unsafe {
        sys::thread_create(s.process, name.as_ptr(), name.len() as u64, &mut fresh)
    })?;
    ensure("refusing a new thread", suspend(fresh) == BAD_STATE)?;
    ensure(
        "refusing to resume a new thread",
        resume_thread(fresh) == BAD_STATE,
    )?;
    let thread = spawn(s, spin, 0)?;
    sleep_ms(5);
    ensure("spinning", SPINS.load(Ordering::Acquire) > 0)?;
    ensure(
        "hiding the registers of a running thread",
        general_registers(thread).0 == BAD_STATE,
    )?;
    suspend_and_wait(thread)?;
    let (status, regs) = general_registers(thread);
    check("reading the registers", status)?;
    ensure(
        "stopping in user code",
        (0x1_0000..0x7FFF_FFFF_F000).contains(&u64_at(&regs, 0x80)),
    )?;
    let count = SPINS.load(Ordering::Acquire);
    sleep_ms(20);
    ensure("not running", SPINS.load(Ordering::Acquire) == count)?;
    check("suspending again", suspend(thread))?;
    check("resuming once", resume_thread(thread))?;
    sleep_ms(10);
    ensure(
        "counting suspensions",
        SPINS.load(Ordering::Acquire) == count && thread_state(thread)? == STATE_SUSPENDED,
    )?;
    check("resuming", resume_thread(thread))?;
    sleep_ms(10);
    ensure("running again", SPINS.load(Ordering::Acquire) > count)?;
    let (_, observed) = wait(thread, SUSPENDED, 0);
    ensure("clearing SUSPENDED", observed & SUSPENDED == 0)?;
    ensure("refusing a zero count", resume_thread(thread) == BAD_STATE)?;
    suspend_and_wait(thread)?;
    // SAFETY: the call takes no pointers.
    check("killing", unsafe { sys::task_kill(thread) })?;
    ensure("ending", wait(thread, TERMINATED, in_time()).0 == 0)?;
    ensure("refusing a dead thread", suspend(thread) == BAD_STATE)
}

/// What the waiting thread's wait returned.
static WAIT_STATUS: AtomicU64 = AtomicU64::new(u64::MAX);

extern "sysv64" fn wait_for_user0(event: u64, _: u64) -> ! {
    let (status, _) = wait(event as Handle, USER0, i64::MAX);
    WAIT_STATUS.store(status as u64, Ordering::Release);
    // SAFETY: the call takes no pointers.
    unsafe { sys::thread_exit() };
    unreachable!("thread_exit returned");
}

/// A thread suspended in a wait waits again once resumed, without
/// noticing the suspension.
fn suspend_blocked_waits(s: &Startup) -> Result {
    let mut event = 0;
    // SAFETY: the pointer is valid for the call.
    check("creating an event", unsafe {
        sys::event_create(0, &mut event)
    })?;
    let thread = spawn(s, wait_for_user0, u64::from(event))?;
    wait_until_blocked(thread)?;
    suspend_and_wait(thread)?;
    check("resuming", resume_thread(thread))?;
    wait_until_blocked(thread)?;
    ensure(
        "waiting again",
        WAIT_STATUS.load(Ordering::Acquire) == u64::MAX,
    )?;
    suspend_and_wait(thread)?;
    signal(event, 0, USER0)?;
    sleep_ms(10);
    ensure(
        "not returning while suspended",
        WAIT_STATUS.load(Ordering::Acquire) == u64::MAX,
    )?;
    check("resuming", resume_thread(thread))?;
    ensure("ending", wait(thread, TERMINATED, in_time()).0 == 0)?;
    ensure(
        "completing the wait",
        WAIT_STATUS.load(Ordering::Acquire) == 0,
    )
}

/// What the calling thread's call returned: 0 with the expected reply.
static CALL_STATUS: AtomicU64 = AtomicU64::new(u64::MAX);

extern "sysv64" fn call_peer(channel: u64, _: u64) -> ! {
    let (status, _, reply, _) = call(channel as Handle, b"ping", None, 64, i64::MAX);
    let result = match status {
        0 if &reply[4..8] == b"pong" => 0,
        0 => 1,
        e => e,
    };
    CALL_STATUS.store(result as u64, Ordering::Release);
    // SAFETY: the call takes no pointers.
    unsafe { sys::thread_exit() };
    unreachable!("thread_exit returned");
}

/// A thread suspended in `channel_call` keeps its call outstanding and
/// takes the reply once resumed; the request is written once.
fn suspend_in_call(s: &Startup) -> Result {
    let (mut ours, mut theirs) = (0, 0);
    // SAFETY: the pointers are valid for the call.
    check("creating a channel", unsafe {
        sys::channel_create(0, &mut ours, &mut theirs)
    })?;
    let thread = spawn(s, call_peer, u64::from(theirs))?;
    let request = read_message(ours, 1000)?;
    ensure(
        "receiving the request",
        &request.bytes[4..request.len] == b"ping",
    )?;
    wait_until_blocked(thread)?;
    suspend_and_wait(thread)?;
    let (status, regs) = general_registers(thread);
    check("reading the registers", status)?;
    ensure("showing the call's number", u64_at(&regs, 0) == 65)?;
    let mut reply = [0u8; 8];
    reply[..4].copy_from_slice(&request.bytes[..4]);
    reply[4..].copy_from_slice(b"pong");
    // SAFETY: the pointer and length describe `reply`.
    check("replying", unsafe {
        sys::channel_write(ours, reply.as_ptr(), 8, core::ptr::null(), 0)
    })?;
    sleep_ms(10);
    ensure(
        "not returning while suspended",
        CALL_STATUS.load(Ordering::Acquire) == u64::MAX,
    )?;
    check("resuming", resume_thread(thread))?;
    ensure("ending", wait(thread, TERMINATED, in_time()).0 == 0)?;
    ensure("taking the reply", CALL_STATUS.load(Ordering::Acquire) == 0)?;
    let mut probe = [0u8; 64];
    let (mut len, mut count) = (0u64, 0u64);
    // SAFETY: the pointers describe `probe`, `len` and `count`.
    let status = unsafe {
        sys::channel_read(
            ours,
            probe.as_mut_ptr(),
            64,
            core::ptr::null_mut(),
            0,
            &mut len,
            &mut count,
        )
    };
    ensure("writing the request once", status == SHOULD_WAIT)
}

/// The object ID of the object of `handle`.
fn koid(handle: Handle) -> Result<u64> {
    Ok(u64_at(&info::<0x18>(handle, 1)?, 0))
}

/// The `THREADS` record of `process`: up to 128 object IDs and their
/// count.
fn thread_list(process: Handle) -> Result<([u64; 128], usize)> {
    let mut record = [0u8; 1024];
    let mut len = 0;
    // SAFETY: the pointers and length describe `record` and `len`.
    check("listing threads", unsafe {
        sys::object_get_info(process, 7, record.as_mut_ptr(), 1024, &mut len)
    })?;
    let count = len as usize / 8;
    let mut koids = [0u64; 128];
    for (i, k) in koids.iter_mut().enumerate().take(count) {
        *k = u64_at(&record, 8 * i);
    }
    Ok((koids, count))
}

fn get_thread(process: Handle, koid: u64) -> (i64, Handle) {
    let mut thread = 0;
    // SAFETY: the pointer is valid for the call.
    let status = unsafe { sys::process_get_thread(process, koid, &mut thread) };
    (status, thread)
}

/// A process lists its threads that are not dead, and hands out handles
/// to them by object ID.
fn thread_lists(s: &Startup) -> Result {
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

const ACCESS_DENIED: i64 = -13;

fn read_memory(process: Handle, addr: u64, buf: &mut [u8]) -> i64 {
    // SAFETY: the pointer and length describe `buf`.
    unsafe { sys::process_read_memory(process, addr, buf.as_mut_ptr(), buf.len() as u64) }
}

/// A debugger reads the memory of a process across regions and from
/// paged objects; ranges without permissions or mapping are not found,
/// and a revoked object is refused before that.
fn read_process_memory(s: &Startup) -> Result {
    let mem = create_memory(2 * PAGE_SIZE)?;
    mem_write_at(mem, PAGE_SIZE - 3, b"abcdef")?;
    let at = map(s.process, mem, 2 * PAGE_SIZE)?;
    check(
        "splitting the mapping",
        protect(s, PERM_READ, at + PAGE_SIZE, PAGE_SIZE),
    )?;
    let mut got = [0u8; 6];
    check(
        "reading across regions",
        read_memory(s.process, at + PAGE_SIZE - 3, &mut got),
    )?;
    ensure("reading the bytes", got == *b"abcdef")?;
    let mut zeros = [1u8; 8];
    check(
        "reading a page never written",
        read_memory(s.process, at + 2 * PAGE_SIZE - 8, &mut zeros),
    )?;
    ensure("reading zeros", zeros == [0; 8])?;

    let mut other = 0;
    let name = b"target";
    // SAFETY: the pointers describe `name` and `other`.
    check("creating a process", unsafe {
        sys::process_create(s.budget, name.as_ptr(), name.len() as u64, &mut other)
    })?;
    let there = map(other, mem, 2 * PAGE_SIZE)?;
    check(
        "reading another process",
        read_memory(other, there + PAGE_SIZE - 3, &mut got),
    )?;
    ensure("reading its bytes", got == *b"abcdef")?;

    let mut reserved = 0;
    // SAFETY: the out pointer is valid for the call.
    check("reserving a range", unsafe {
        sys::vm_map(s.process, 0, 0, mem, 0, PAGE_SIZE, &mut reserved)
    })?;
    ensure(
        "refusing a range without permissions",
        read_memory(s.process, reserved, &mut got) == NOT_FOUND,
    )?;
    // SAFETY: the call takes no pointers.
    check("unmapping", unsafe {
        sys::vm_unmap(s.process, at + PAGE_SIZE, PAGE_SIZE)
    })?;
    ensure(
        "refusing an unmapped byte",
        read_memory(s.process, at + PAGE_SIZE - 3, &mut got) == NOT_FOUND,
    )?;

    let (pager, paged) = paged_object(s, 2, 0)?;
    let pat = map(s.process, paged, 2 * PAGE_SIZE)?;
    check(
        "reading an absent page",
        read_memory(s.process, pat + PAGE_SIZE, &mut got),
    )?;
    ensure("reading the supplied page", got == *b"page 1")?;
    // SAFETY: the call takes no pointers.
    check("unmapping", unsafe {
        sys::vm_unmap(s.process, pat + PAGE_SIZE, PAGE_SIZE)
    })?;
    // SAFETY: the call takes no pointers.
    check("revoking", unsafe { sys::pager_revoke(pager, paged) })?;
    ensure(
        "refusing a revoked object first",
        read_memory(s.process, pat + PAGE_SIZE - 3, &mut got) == ACCESS_DENIED,
    )?;
    ensure(
        "refusing only the hole",
        read_memory(s.process, pat + PAGE_SIZE, &mut got) == NOT_FOUND,
    )
}

/// The page below init's stack stays reserved: nothing there can be read,
/// a mapping placed there is refused, and access to it cannot be granted.
fn stack_guard_page_kept(s: &Startup) -> Result {
    let rsp: u64;
    // SAFETY: the instruction only reads the stack pointer.
    unsafe { core::arch::asm!("mov {}, rsp", out(reg) rsp) };
    let mut guard = rsp & !(PAGE_SIZE - 1);
    while read_memory(s.process, guard, &mut [0u8; 1]) >= 0 {
        guard -= PAGE_SIZE;
    }
    ensure(
        "finding nothing below the stack",
        read_memory(s.process, guard, &mut [0u8; 1]) == NOT_FOUND,
    )?;
    let mem = create_memory(PAGE_SIZE)?;
    let mut at = 0;
    // SAFETY: the out pointer is valid for the call.
    let status = unsafe {
        sys::vm_map(
            s.process,
            PERM_READ | mikros_test::SPECIFIC,
            guard,
            mem,
            0,
            PAGE_SIZE,
            &mut at,
        )
    };
    if status == 0 {
        // SAFETY: the call takes no pointers.
        unsafe { sys::vm_unmap(s.process, at, PAGE_SIZE) };
    }
    ensure("refusing a mapping there", status == ALREADY_EXISTS)?;
    ensure(
        "refusing access to it",
        protect(s, PERM_READ, guard, PAGE_SIZE) == ACCESS_DENIED,
    )?;
    // SAFETY: the call takes no pointers.
    check("closing", unsafe { sys::handle_close(mem) })
}

fn write_memory(process: Handle, addr: u64, bytes: &[u8]) -> i64 {
    // SAFETY: the pointer and length describe `bytes`.
    unsafe { sys::process_write_memory(process, addr, bytes.as_ptr(), bytes.len() as u64) }
}

/// Maps `len` bytes of `mem` into this process with `options` at `addr`.
fn map_with(s: &Startup, options: u32, addr: u64, mem: Handle, len: u64) -> Result<u64> {
    let mut at = 0;
    // SAFETY: the out pointer is valid for the call.
    check("mapping", unsafe {
        sys::vm_map(s.process, options, addr, mem, 0, len, &mut at)
    })?;
    Ok(at)
}

/// A debugger writes the memory of a process whatever the permissions
/// of its mappings, and new code runs; mappings from handles without
/// `WRITE`, sealed objects, revoked objects and a source buffer that
/// cannot be read to its end refuse the whole write.
fn write_process_memory(s: &Startup) -> Result {
    const EXECUTE: u32 = 4;
    const SPECIFIC: u32 = 1 << 8;
    let mem = create_memory(2 * PAGE_SIZE)?;
    let at = map(s.process, mem, 2 * PAGE_SIZE)?;
    check(
        "making the mapping read-only",
        protect(s, PERM_READ, at + PAGE_SIZE, PAGE_SIZE),
    )?;
    check(
        "writing across regions",
        write_memory(s.process, at + PAGE_SIZE - 3, b"ABCDEF"),
    )?;
    ensure(
        "writing the bytes",
        mem_read_at::<6>(mem, PAGE_SIZE - 3)? == *b"ABCDEF",
    )?;

    let code = create_memory(PAGE_SIZE)?;
    let entry = map_with(s, PERM_READ | EXECUTE, 0, code, PAGE_SIZE)?;
    // SAFETY: the mapping is executable and holds the code written next.
    let run = unsafe { core::mem::transmute::<u64, extern "sysv64" fn() -> u32>(entry) };
    // mov eax, 42; ret
    check(
        "writing code",
        write_memory(s.process, entry, &[0xB8, 42, 0, 0, 0, 0xC3]),
    )?;
    ensure("running the code", run() == 42)?;
    check(
        "changing the code",
        write_memory(s.process, entry + 1, &[7]),
    )?;
    ensure("running the new code", run() == 7)?;

    // The second page of the range comes from a handle without WRITE.
    let mut read_only = 0;
    // SAFETY: the out pointer is valid for the call.
    check("duplicating without WRITE", unsafe {
        sys::handle_duplicate(mem, (1 << 2) | (1 << 5), &mut read_only)
    })?;
    // SAFETY: the call takes no pointers.
    check("unmapping", unsafe {
        sys::vm_unmap(s.process, at + PAGE_SIZE, PAGE_SIZE)
    })?;
    map_with(
        s,
        PERM_READ | SPECIFIC,
        at + PAGE_SIZE,
        read_only,
        PAGE_SIZE,
    )?;
    ensure(
        "refusing a mapping without WRITE",
        write_memory(s.process, at + PAGE_SIZE - 3, b"xyzxyz") == ACCESS_DENIED,
    )?;
    ensure(
        "writing nothing",
        mem_read_at::<6>(mem, PAGE_SIZE - 3)? == *b"ABCDEF",
    )?;

    // The source's first 256 bytes are readable and the rest is not.
    let source = create_memory(2 * PAGE_SIZE)?;
    let src = map(s.process, source, 2 * PAGE_SIZE)?;
    let readable = src + PAGE_SIZE - 256;
    for i in 0..256 {
        // SAFETY: the first page of the mapping is writable.
        unsafe { ((readable + i) as *mut u8).write_volatile(0xAB) };
    }
    check(
        "making the source's end unreadable",
        protect(s, 0, src + PAGE_SIZE, PAGE_SIZE),
    )?;
    ensure(
        "refusing a source that cannot be read",
        // SAFETY: the kernel only reads the 512 bytes from the pointer.
        unsafe { sys::process_write_memory(s.process, at, readable as *const u8, 512) }
            == INVALID_ARGS,
    )?;
    ensure("writing none of it", mem_read_at::<8>(mem, 0)? == [0; 8])?;
    // SAFETY: the call takes no pointers.
    check("unmapping the source", unsafe {
        sys::vm_unmap(s.process, src, 2 * PAGE_SIZE)
    })?;

    let sealed = create_memory(PAGE_SIZE)?;
    let sat = map_with(s, PERM_READ, 0, sealed, PAGE_SIZE)?;
    ensure("sealing", seal(sealed, 1 << 3) == 0)?;
    ensure(
        "refusing a sealed object",
        write_memory(s.process, sat, b"x") == ACCESS_DENIED,
    )?;

    let (pager, paged) = paged_object(s, 2, 2)?;
    let pat = map_with(s, PERM_READ, 0, paged, 2 * PAGE_SIZE)?;
    check(
        "writing a paged object",
        write_memory(s.process, pat + PAGE_SIZE, b"P"),
    )?;
    let (ranges, count) = dirty(pager, paged, 2 * PAGE_SIZE)?;
    ensure(
        "marking the page dirty",
        count == 1 && ranges[..2] == [PAGE_SIZE, PAGE_SIZE],
    )?;
    let mut got = [0u8; 6];
    check(
        "reading back",
        read_memory(s.process, pat + PAGE_SIZE, &mut got),
    )?;
    ensure("keeping the supplied rest", got == *b"Page 1")?;
    // SAFETY: the call takes no pointers.
    check("revoking", unsafe { sys::pager_revoke(pager, paged) })?;
    ensure(
        "refusing a revoked object",
        write_memory(s.process, pat, b"x") == ACCESS_DENIED,
    )
}

/// What `start_probe` finds at its first instruction: RAX to R15 in the
/// order of the `GENERAL` layout, RFLAGS, the FS and GS bases, the x87
/// control word and MXCSR, and the 16 YMM registers.
static mut START_STATE: [u64; 21] = [0; 21];
static mut START_YMM: [[u64; 4]; 16] = [[0; 4]; 16];

/// The values `register_keeper` holds in RAX to R15, without RSP, in
/// that order, and in the YMM registers.
const GENERAL_PATTERN: [u64; 15] = {
    let mut p = [0; 15];
    let mut k = 0;
    while k < 15 {
        p[k] = (k as u64 + 1) * 0x0101_0101_0101_0101;
        k += 1;
    }
    p
};
static YMM_PATTERN: [[u64; 4]; 16] = {
    let mut p = [[0; 4]; 16];
    let mut n = 0;
    while n < 16 {
        let mut lane = 0;
        while lane < 4 {
            p[n][lane] = 0xA5A5_0000_0000_0000 | (n as u64) << 16 | lane as u64;
            lane += 1;
        }
        n += 1;
    }
    p
};

/// Set by `register_keeper` once its registers hold the patterns, and by
/// `register_clobberer` once it ran; and what `register_keeper` found in
/// its registers afterwards.
static KEEPER_READY: AtomicU64 = AtomicU64::new(0);
static CLOBBERER_RAN: AtomicU64 = AtomicU64::new(0);
static mut KEPT: [u64; 15] = [0; 15];
static mut KEPT_YMM: [[u64; 4]; 16] = [[0; 4]; 16];

// `start_probe` records the state a thread starts with before any
// instruction changes it. `register_keeper` fills every general and
// vector register with patterns, spins until `register_clobberer` has
// run with other values in them, and records what it finds. All three
// then end their thread.
core::arch::global_asm!(
    ".global start_probe",
    "start_probe:",
    "mov [rip + {state}], rax",
    "mov [rip + {state} + 8], rbx",
    "mov [rip + {state} + 16], rcx",
    "mov [rip + {state} + 24], rdx",
    "mov [rip + {state} + 32], rsi",
    "mov [rip + {state} + 40], rdi",
    "mov [rip + {state} + 48], rbp",
    "mov [rip + {state} + 56], rsp",
    "mov [rip + {state} + 64], r8",
    "mov [rip + {state} + 72], r9",
    "mov [rip + {state} + 80], r10",
    "mov [rip + {state} + 88], r11",
    "mov [rip + {state} + 96], r12",
    "mov [rip + {state} + 104], r13",
    "mov [rip + {state} + 112], r14",
    "mov [rip + {state} + 120], r15",
    "pushfq",
    "pop qword ptr [rip + {state} + 128]",
    "rdfsbase rax",
    "mov [rip + {state} + 136], rax",
    "rdgsbase rax",
    "mov [rip + {state} + 144], rax",
    "fnstcw word ptr [rip + {state} + 152]",
    "stmxcsr dword ptr [rip + {state} + 160]",
    "vmovdqu [rip + {start_ymm}], ymm0",
    "vmovdqu [rip + {start_ymm} + 32], ymm1",
    "vmovdqu [rip + {start_ymm} + 64], ymm2",
    "vmovdqu [rip + {start_ymm} + 96], ymm3",
    "vmovdqu [rip + {start_ymm} + 128], ymm4",
    "vmovdqu [rip + {start_ymm} + 160], ymm5",
    "vmovdqu [rip + {start_ymm} + 192], ymm6",
    "vmovdqu [rip + {start_ymm} + 224], ymm7",
    "vmovdqu [rip + {start_ymm} + 256], ymm8",
    "vmovdqu [rip + {start_ymm} + 288], ymm9",
    "vmovdqu [rip + {start_ymm} + 320], ymm10",
    "vmovdqu [rip + {start_ymm} + 352], ymm11",
    "vmovdqu [rip + {start_ymm} + 384], ymm12",
    "vmovdqu [rip + {start_ymm} + 416], ymm13",
    "vmovdqu [rip + {start_ymm} + 448], ymm14",
    "vmovdqu [rip + {start_ymm} + 480], ymm15",
    "jmp {done}",
    ".global register_keeper",
    "register_keeper:",
    "mov rax, {g0}",
    "mov rbx, {g1}",
    "mov rcx, {g2}",
    "mov rdx, {g3}",
    "mov rsi, {g4}",
    "mov rdi, {g5}",
    "mov rbp, {g6}",
    "mov r8, {g7}",
    "mov r9, {g8}",
    "mov r10, {g9}",
    "mov r11, {g10}",
    "mov r12, {g11}",
    "mov r13, {g12}",
    "mov r14, {g13}",
    "mov r15, {g14}",
    "vmovdqu ymm0, [rip + {pattern}]",
    "vmovdqu ymm1, [rip + {pattern} + 32]",
    "vmovdqu ymm2, [rip + {pattern} + 64]",
    "vmovdqu ymm3, [rip + {pattern} + 96]",
    "vmovdqu ymm4, [rip + {pattern} + 128]",
    "vmovdqu ymm5, [rip + {pattern} + 160]",
    "vmovdqu ymm6, [rip + {pattern} + 192]",
    "vmovdqu ymm7, [rip + {pattern} + 224]",
    "vmovdqu ymm8, [rip + {pattern} + 256]",
    "vmovdqu ymm9, [rip + {pattern} + 288]",
    "vmovdqu ymm10, [rip + {pattern} + 320]",
    "vmovdqu ymm11, [rip + {pattern} + 352]",
    "vmovdqu ymm12, [rip + {pattern} + 384]",
    "vmovdqu ymm13, [rip + {pattern} + 416]",
    "vmovdqu ymm14, [rip + {pattern} + 448]",
    "vmovdqu ymm15, [rip + {pattern} + 480]",
    "mov qword ptr [rip + {ready}], 1",
    "2:",
    "pause",
    "cmp qword ptr [rip + {ran}], 0",
    "je 2b",
    "mov [rip + {kept}], rax",
    "mov [rip + {kept} + 8], rbx",
    "mov [rip + {kept} + 16], rcx",
    "mov [rip + {kept} + 24], rdx",
    "mov [rip + {kept} + 32], rsi",
    "mov [rip + {kept} + 40], rdi",
    "mov [rip + {kept} + 48], rbp",
    "mov [rip + {kept} + 56], r8",
    "mov [rip + {kept} + 64], r9",
    "mov [rip + {kept} + 72], r10",
    "mov [rip + {kept} + 80], r11",
    "mov [rip + {kept} + 88], r12",
    "mov [rip + {kept} + 96], r13",
    "mov [rip + {kept} + 104], r14",
    "mov [rip + {kept} + 112], r15",
    "vmovdqu [rip + {kept_ymm}], ymm0",
    "vmovdqu [rip + {kept_ymm} + 32], ymm1",
    "vmovdqu [rip + {kept_ymm} + 64], ymm2",
    "vmovdqu [rip + {kept_ymm} + 96], ymm3",
    "vmovdqu [rip + {kept_ymm} + 128], ymm4",
    "vmovdqu [rip + {kept_ymm} + 160], ymm5",
    "vmovdqu [rip + {kept_ymm} + 192], ymm6",
    "vmovdqu [rip + {kept_ymm} + 224], ymm7",
    "vmovdqu [rip + {kept_ymm} + 256], ymm8",
    "vmovdqu [rip + {kept_ymm} + 288], ymm9",
    "vmovdqu [rip + {kept_ymm} + 320], ymm10",
    "vmovdqu [rip + {kept_ymm} + 352], ymm11",
    "vmovdqu [rip + {kept_ymm} + 384], ymm12",
    "vmovdqu [rip + {kept_ymm} + 416], ymm13",
    "vmovdqu [rip + {kept_ymm} + 448], ymm14",
    "vmovdqu [rip + {kept_ymm} + 480], ymm15",
    "jmp {done}",
    ".global register_clobberer",
    "register_clobberer:",
    "mov rax, -1",
    "mov rbx, rax",
    "mov rcx, rax",
    "mov rdx, rax",
    "mov rsi, rax",
    "mov rdi, rax",
    "mov rbp, rax",
    "mov r8, rax",
    "mov r9, rax",
    "mov r10, rax",
    "mov r11, rax",
    "mov r12, rax",
    "mov r13, rax",
    "mov r14, rax",
    "mov r15, rax",
    "vpcmpeqd ymm0, ymm0, ymm0",
    "vmovdqa ymm1, ymm0",
    "vmovdqa ymm2, ymm0",
    "vmovdqa ymm3, ymm0",
    "vmovdqa ymm4, ymm0",
    "vmovdqa ymm5, ymm0",
    "vmovdqa ymm6, ymm0",
    "vmovdqa ymm7, ymm0",
    "vmovdqa ymm8, ymm0",
    "vmovdqa ymm9, ymm0",
    "vmovdqa ymm10, ymm0",
    "vmovdqa ymm11, ymm0",
    "vmovdqa ymm12, ymm0",
    "vmovdqa ymm13, ymm0",
    "vmovdqa ymm14, ymm0",
    "vmovdqa ymm15, ymm0",
    "mov qword ptr [rip + {ran}], 1",
    "jmp {done}",
    state = sym START_STATE,
    start_ymm = sym START_YMM,
    pattern = sym YMM_PATTERN,
    ready = sym KEEPER_READY,
    ran = sym CLOBBERER_RAN,
    kept = sym KEPT,
    kept_ymm = sym KEPT_YMM,
    done = sym end_asm_thread,
    g0 = const GENERAL_PATTERN[0],
    g1 = const GENERAL_PATTERN[1],
    g2 = const GENERAL_PATTERN[2],
    g3 = const GENERAL_PATTERN[3],
    g4 = const GENERAL_PATTERN[4],
    g5 = const GENERAL_PATTERN[5],
    g6 = const GENERAL_PATTERN[6],
    g7 = const GENERAL_PATTERN[7],
    g8 = const GENERAL_PATTERN[8],
    g9 = const GENERAL_PATTERN[9],
    g10 = const GENERAL_PATTERN[10],
    g11 = const GENERAL_PATTERN[11],
    g12 = const GENERAL_PATTERN[12],
    g13 = const GENERAL_PATTERN[13],
    g14 = const GENERAL_PATTERN[14],
);

unsafe extern "sysv64" {
    safe fn start_probe(a: u64, b: u64) -> !;
    safe fn register_keeper(a: u64, b: u64) -> !;
    safe fn register_clobberer(a: u64, b: u64) -> !;
}

/// Ends a thread whose entry is written in assembly.
extern "sysv64" fn end_asm_thread() -> ! {
    // SAFETY: the call takes no pointers.
    unsafe { sys::thread_exit() };
    unreachable!("thread_exit returned");
}

/// A new thread starts with its arguments in RDI and RSI, its stack in
/// RSP, every other general-purpose register, the FS and GS bases and
/// every vector register zero, RFLAGS 0x202, the x87 control word 0x037F
/// and MXCSR 0x1F80.
fn new_thread_starts_clean(s: &Startup) -> Result {
    let (a, b) = (0x1111_2222_3333_4444, 0x5555_6666_7777_8888);
    let thread = spawn2(s, start_probe, a, b)?;
    ensure(
        "ending the probe",
        wait(thread, TERMINATED, in_time()).0 == 0,
    )?;
    // SAFETY: the probe wrote the state and ended.
    let (state, ymm) = unsafe {
        (
            core::ptr::read_volatile(&raw const START_STATE),
            core::ptr::read_volatile(&raw const START_YMM),
        )
    };
    let (rsi, rdi, rsp) = (state[4], state[5], state[7]);
    ensure("passing the arguments", rdi == a && rsi == b)?;
    ensure("aligning the stack as after a call", rsp % 16 == 8)?;
    let others = state[..16]
        .iter()
        .enumerate()
        .all(|(i, &v)| matches!(i, 4 | 5 | 7) || v == 0);
    ensure("clearing the other registers", others)?;
    ensure("starting with RFLAGS 0x202", state[16] == 0x202)?;
    ensure("clearing the bases", state[17] == 0 && state[18] == 0)?;
    ensure(
        "starting the x87 unit and SSE in their initial state",
        state[19] == 0x037F && state[20] == 0x1F80,
    )?;
    ensure("clearing the vector registers", ymm == [[0; 4]; 16])
}

/// A thread switched out by preemption gets every general-purpose and
/// vector register back as it was, although another thread changed them
/// all meanwhile.
fn registers_survive_preemption(s: &Startup) -> Result {
    KEEPER_READY.store(0, Ordering::Release);
    CLOBBERER_RAN.store(0, Ordering::Release);
    let keeper = spawn2(s, register_keeper, 0, 0)?;
    // This thread runs again only once the keeper, holding its patterns,
    // has been switched out.
    let deadline = in_time();
    while KEEPER_READY.load(Ordering::Acquire) == 0 {
        ensure("starting the keeper", now() < deadline)?;
        sleep_ms(1);
    }
    let clobberer = spawn2(s, register_clobberer, 0, 0)?;
    for thread in [keeper, clobberer] {
        ensure(
            "ending the threads",
            wait(thread, TERMINATED, in_time()).0 == 0,
        )?;
    }
    // SAFETY: the keeper wrote what it found and ended.
    let (kept, kept_ymm) = unsafe {
        (
            core::ptr::read_volatile(&raw const KEPT),
            core::ptr::read_volatile(&raw const KEPT_YMM),
        )
    };
    ensure("keeping the general registers", kept == GENERAL_PATTERN)?;
    ensure("keeping the vector registers", kept_ymm == YMM_PATTERN)
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
fn user_bases_set_and_kept(_: &Startup) -> Result {
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

/// The stack pointer and RFLAGS at the first instruction of the signal
/// handler `signal_probe`, the arguments `signal_handler` got, the mask
/// while it ran and how often it ran.
static SIGNAL_ENTRY: [AtomicU64; 2] = [AtomicU64::new(0), AtomicU64::new(0)];
static SIGNAL_ARGS: [AtomicU64; 3] = [AtomicU64::new(0), AtomicU64::new(0), AtomicU64::new(0)];
static SIGNAL_MASK: AtomicU64 = AtomicU64::new(0);
static HANDLED: AtomicU64 = AtomicU64::new(0);
/// The signal frame `signal_handler` found, from 72 bytes below the
/// context to its extended state: the restorer, the information record,
/// the register state, the mask, the extended state size and 16 bytes.
static mut SIGNAL_FRAME: [u64; 33] = [0; 33];

// `signal_probe` records the stack pointer and the flags the handler
// starts with and goes on in `signal_handler`, which returns to
// `signal_restorer`; that finds the context 64 bytes above the stack
// pointer, where the return left it, and returns from the signal.
core::arch::global_asm!(
    ".global signal_probe",
    "signal_probe:",
    "mov [rip + {entry}], rsp",
    "pushfq",
    "pop qword ptr [rip + {entry} + 8]",
    "jmp {handler}",
    ".global signal_restorer",
    "signal_restorer:",
    "lea rdi, [rsp + 64]",
    "call {back}",
    entry = sym SIGNAL_ENTRY,
    handler = sym signal_handler,
    back = sym signal_back,
);

unsafe extern "sysv64" {
    safe fn signal_probe();
    safe fn signal_restorer();
}

extern "sysv64" fn signal_handler(signo: u64, info: u64, context: u64) {
    let mut mask = 0;
    // SAFETY: the pointer is valid; adding no signal only reads the mask.
    unsafe { sys::signal_set_mask(0, 0, &mut mask) };
    SIGNAL_MASK.store(mask, Ordering::Release);
    for (slot, value) in SIGNAL_ARGS.iter().zip([signo, info, context]) {
        slot.store(value, Ordering::Release);
    }
    let from = (context - 72) as *const u64;
    for i in 0..33 {
        // SAFETY: the frame lies on this thread's stack from 72 bytes
        // below the context, and only this handler writes the copy.
        unsafe {
            let word = from.add(i).read_volatile();
            (&raw mut SIGNAL_FRAME)
                .cast::<u64>()
                .add(i)
                .write_volatile(word);
        }
    }
    HANDLED.fetch_add(1, Ordering::AcqRel);
}

extern "sysv64" fn signal_back(context: u64) -> ! {
    // SAFETY: `context` is the context of the frame the handler got.
    unsafe { sys::signal_return(context) };
    unreachable!("signal_return returned");
}

const USR1: u32 = 10;
const INTERRUPTED: i64 = -27;

/// Makes `signal_probe` the handler of `USR1` with `flags`, blocking
/// `USR2` while it runs, and returns the action it replaced.
fn catch_usr1(flags: u32) -> Result<[u8; 32]> {
    let mut action = [0u8; 32];
    action[0x00..0x08].copy_from_slice(&(signal_probe as *const () as u64).to_le_bytes());
    action[0x08..0x10].copy_from_slice(&(1u64 << 11).to_le_bytes());
    action[0x10..0x18].copy_from_slice(&(signal_restorer as *const () as u64).to_le_bytes());
    action[0x18..0x1C].copy_from_slice(&flags.to_le_bytes());
    let mut old = [0u8; 32];
    // SAFETY: the pointers describe `action` and `old`.
    check("setting the action", unsafe {
        sys::signal_set_action(USR1, action.as_ptr(), old.as_mut_ptr())
    })?;
    HANDLED.store(0, Ordering::Release);
    Ok(old)
}

fn restore_usr1(old: &[u8; 32]) -> Result {
    // SAFETY: the pointer describes `old`.
    check("restoring the action", unsafe {
        sys::signal_set_action(USR1, old.as_ptr(), core::ptr::null_mut())
    })
}

/// The calling thread's mask.
fn signal_mask() -> u64 {
    let mut mask = 0;
    // SAFETY: the pointer is valid; adding no signal only reads the mask.
    unsafe { sys::signal_set_mask(0, 0, &mut mask) };
    mask
}

/// The copy of the last signal frame.
fn signal_frame_copy() -> [u64; 33] {
    // SAFETY: the handler that wrote the copy has returned.
    unsafe { core::ptr::read_volatile(&raw const SIGNAL_FRAME) }
}

/// A handler starts with its arguments and the stack and frame in their
/// layout: the context 64-byte aligned, as high as it fits 128 bytes
/// below the interrupted stack pointer, the restorer below the
/// information record below it, the mask before the handler and the
/// extended state size above the register state. DF is clear, and the
/// signal and the action's mask are blocked while the handler runs and
/// not afterwards.
fn signal_frame(s: &Startup) -> Result {
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

/// `signal_return` refuses a context with a reserved byte set, which the
/// debuggee's handler sets before it returns past the instruction that
/// raised the signal, and the process ends as `SEGV` ends it.
fn signal_frame_reserved_bytes(s: &Startup) -> Result {
    const SEGV: u32 = 11;
    let (process, _) = start_program_with(s, "debuggee", 7, |_| Ok(()))?;
    ensure("refusing the context", exit_code(process)? == -3)?;
    ensure(
        "ending the process as SEGV",
        u32_at(&info::<0x20>(process, 2)?, 0x10) == SEGV,
    )
}

/// Set by `signal_sleeper` with what its sleep returned and when.
static SLEEP_RESULT: [AtomicU64; 2] = [AtomicU64::new(0), AtomicU64::new(0)];

/// Sleeps until `deadline`, records the status and the time, and exits.
extern "sysv64" fn signal_sleeper(deadline: u64, _: u64) -> ! {
    // SAFETY: the call takes no pointers.
    let status = unsafe { sys::thread_sleep(deadline as i64) };
    SLEEP_RESULT[0].store(status as u64, Ordering::Release);
    SLEEP_RESULT[1].store(now() as u64, Ordering::Release);
    // SAFETY: the call takes no pointers.
    unsafe { sys::thread_exit() };
    unreachable!("thread_exit returned");
}

/// Signals a thread sleeping for 100 ms with `USR1` handled with `flags`,
/// and returns the status of its sleep, its deadline and when it ended.
fn signal_sleeping_thread(s: &Startup, flags: u32) -> Result<(i64, i64, i64)> {
    let old = catch_usr1(flags)?;
    let deadline = now() + 100_000_000;
    let thread = spawn2(s, signal_sleeper, deadline as u64, 0)?;
    wait_until_blocked(thread)?;
    // SAFETY: the call takes no pointers.
    check("signalling the sleeper", unsafe {
        sys::thread_signal(thread, USR1, 1)
    })?;
    ensure(
        "ending the sleeper",
        wait(thread, TERMINATED, in_time()).0 == 0,
    )?;
    ensure(
        "running the handler once",
        HANDLED.load(Ordering::Acquire) == 1,
    )?;
    restore_usr1(&old)?;
    let [status, ended] = SLEEP_RESULT
        .each_ref()
        .map(|a| a.load(Ordering::Acquire) as i64);
    Ok((status, deadline, ended))
}

/// A handler for a thread sleeping in `thread_sleep` ends the sleep with
/// `ERR_INTERRUPTED`, unless its action has `RESTART`: then the frame
/// resumes the thread at its `SYSCALL` instruction with the arguments in
/// place, and the sleep goes on until its deadline.
fn signal_interrupts_and_restarts(s: &Startup) -> Result {
    const RESTART: u32 = 1 << 0;
    let (status, deadline, ended) = signal_sleeping_thread(s, 0)?;
    ensure(
        "interrupting the sleep",
        status == INTERRUPTED && ended < deadline,
    )?;
    let (status, deadline, ended) = signal_sleeping_thread(s, RESTART)?;
    ensure("restarting the sleep", status == 0 && ended >= deadline)?;
    let frame = signal_frame_copy();
    let (rdi, rip) = (frame[9 + 5], frame[9 + 16]);
    ensure(
        "keeping the deadline as the argument",
        rdi == deadline as u64,
    )?;
    // SAFETY: the instruction lies in this program's code, which is
    // mapped readable.
    let instruction = unsafe { (rip as *const [u8; 2]).read_volatile() };
    ensure(
        "resuming at the SYSCALL instruction",
        instruction == [0x0F, 0x05],
    )
}

/// The values `extended_keeper` loads into XMM0 and XMM1; set by it once
/// they are loaded, and by the test to let it go on; and what it then
/// finds in XMM0 and XMM1.
static EXTENDED_PATTERN: [[u64; 2]; 2] = [
    [0x0011_2233_4455_6677, 0x8899_AABB_CCDD_EEFF],
    [0x0F0E_0D0C_0B0A_0908, 0x0706_0504_0302_0100],
];
static EXTENDED_READY: AtomicU64 = AtomicU64::new(0);
static EXTENDED_GO: AtomicU64 = AtomicU64::new(0);
static mut EXTENDED_FOUND: [u64; 4] = [0; 4];

core::arch::global_asm!(
    ".global extended_keeper",
    "extended_keeper:",
    "movdqu xmm0, [rip + {pattern}]",
    "movdqu xmm1, [rip + {pattern} + 16]",
    "mov qword ptr [rip + {ready}], 1",
    "2:",
    "pause",
    "cmp qword ptr [rip + {go}], 0",
    "je 2b",
    "movdqu [rip + {found}], xmm0",
    "movdqu [rip + {found} + 16], xmm1",
    "jmp {done}",
    pattern = sym EXTENDED_PATTERN,
    ready = sym EXTENDED_READY,
    go = sym EXTENDED_GO,
    found = sym EXTENDED_FOUND,
    done = sym end_asm_thread,
);

unsafe extern "sysv64" {
    safe fn extended_keeper(a: u64, b: u64) -> !;
}

/// A suspended thread's `EXTENDED` state is an 8-byte size and its
/// registers in `XSAVE` format, and a changed state written back is what
/// the thread finds when it resumes. A state whose MXCSR would make
/// `XRSTOR` fault, a buffer too short, a kind that does not exist and a
/// thread that is not suspended are refused.
fn extended_state_round_trip(s: &Startup) -> Result {
    EXTENDED_READY.store(0, Ordering::Release);
    EXTENDED_GO.store(0, Ordering::Release);
    let thread = spawn2(s, extended_keeper, 0, 0)?;
    let deadline = in_time();
    while EXTENDED_READY.load(Ordering::Acquire) == 0 {
        ensure("starting the keeper", now() < deadline)?;
        sleep_ms(1);
    }
    let read = |buf: &mut [u8], kind| {
        // SAFETY: the pointer and length describe `buf`.
        unsafe { sys::thread_read_state(thread, kind, buf.as_mut_ptr(), buf.len() as u64) }
    };
    let mut state = [0u8; 4096];
    ensure(
        "refusing a thread that is not suspended",
        read(&mut state, 1) == BAD_STATE,
    )?;
    suspend_and_wait(thread)?;
    ensure(
        "refusing a buffer too short",
        read(&mut state[..8], 1) == BUFFER_TOO_SMALL,
    )?;
    ensure("refusing another kind", read(&mut state, 3) == INVALID_ARGS)?;
    check("reading the extended state", read(&mut state, 1))?;
    let size = u64_at(&state, 0) as usize;
    ensure("giving the size", (576..=state.len() - 8).contains(&size))?;
    // The XSAVE image follows the size; its legacy region keeps MXCSR at
    // offset 24 and XMM0 and XMM1 at 160 and 176.
    let (mxcsr, xmm0, xmm1) = (8 + 24, 8 + 160, 8 + 176);
    ensure(
        "holding XMM0",
        [u64_at(&state, xmm0), u64_at(&state, xmm0 + 8)] == EXTENDED_PATTERN[0],
    )?;
    state.copy_within(xmm0..xmm0 + 16, xmm1);
    let write = |buf: &[u8]| {
        // SAFETY: the pointer and length lie in `buf`.
        unsafe { sys::thread_write_state(thread, 1, buf.as_ptr(), 8 + size as u64) }
    };
    let mut bad = state;
    bad[mxcsr..mxcsr + 4].copy_from_slice(&(0x1F80u32 | 1 << 31).to_le_bytes());
    ensure("refusing a reserved MXCSR bit", write(&bad) == INVALID_ARGS)?;
    check("writing the extended state", write(&state))?;
    check("resuming", resume_thread(thread))?;
    EXTENDED_GO.store(1, Ordering::Release);
    ensure(
        "ending the keeper",
        wait(thread, TERMINATED, in_time()).0 == 0,
    )?;
    // SAFETY: the keeper wrote what it found and ended.
    let found = unsafe { core::ptr::read_volatile(&raw const EXTENDED_FOUND) };
    ensure("keeping XMM0", found[..2] == EXTENDED_PATTERN[0])?;
    ensure(
        "loading the XMM1 written",
        found[2..] == EXTENDED_PATTERN[0],
    )
}

const NOT_SUPPORTED: i64 = -2;

/// The rights by name, for the tests below.
mod right {
    pub const DUPLICATE: u32 = 1 << 0;
    pub const TRANSFER: u32 = 1 << 1;
    pub const READ: u32 = 1 << 2;
    pub const WRITE: u32 = 1 << 3;
    pub const MAP: u32 = 1 << 5;
    pub const WAIT: u32 = 1 << 6;
    pub const SIGNAL: u32 = 1 << 7;
    pub const MANAGE: u32 = 1 << 8;
    pub const INSPECT: u32 = 1 << 9;
}

/// The handles of the startup message refer to objects of the right types
/// and have the rights they are given: the default rights, and read-only
/// rights for the boot image and the clock page.
fn startup_handle_rights(s: &Startup) -> Result {
    use right::*;
    let base = DUPLICATE | TRANSFER;
    let read_only = base | READ | MAP | INSPECT;
    let mut expected = [
        (s.process, 1, base | MAP | WAIT | SIGNAL | MANAGE | INSPECT),
        (s.thread, 2, base | WAIT | SIGNAL | MANAGE | INSPECT),
        (s.root_resource, 9, base | MANAGE | INSPECT),
        (s.boot_image, 3, read_only),
        (s.log, 10, base | READ | WRITE | MANAGE | INSPECT),
        (s.budget, 11, base | WRITE | WAIT | MANAGE | INSPECT),
        (s.clock_page, 3, read_only),
        (0, 3, base | READ | WRITE | MAP | MANAGE | INSPECT),
    ];
    let count = match s.framebuffer {
        Some(fb) => {
            expected[7].0 = fb.memory;
            8
        }
        None => 7,
    };
    for (handle, kind, rights) in &expected[..count] {
        let record = info::<0x18>(*handle, 1)?;
        ensure(
            "giving a startup handle its type and rights",
            u32_at(&record, 16) == *kind && u32_at(&record, 20) == *rights,
        )?;
    }
    Ok(())
}

/// Suspending, resuming and inspecting threads and processes needs the
/// `MANAGE` right.
fn debugging_needs_manage(s: &Startup) -> Result {
    use right::*;
    let (mut thread, mut process, mut out) = (0, 0, 0);
    // SAFETY: the out pointers are valid for the calls.
    unsafe {
        check(
            "duplicating the thread",
            sys::handle_duplicate(s.thread, DUPLICATE | WAIT | INSPECT, &mut thread),
        )?;
        check(
            "duplicating the process",
            sys::handle_duplicate(s.process, DUPLICATE | WAIT | INSPECT, &mut process),
        )?;
    }
    ensure("refusing to suspend", suspend(thread) == ACCESS_DENIED)?;
    ensure("refusing to resume", resume_thread(thread) == ACCESS_DENIED)?;
    let koid = koid(s.thread)?;
    // SAFETY: the out pointer is valid for the call.
    let status = unsafe { sys::process_get_thread(process, koid, &mut out) };
    ensure("refusing to get a thread", status == ACCESS_DENIED)?;
    let at = &raw const HANDLED as u64;
    ensure(
        "refusing to read memory",
        read_memory(process, at, &mut [0u8; 8]) == ACCESS_DENIED,
    )?;
    ensure(
        "refusing to write memory",
        write_memory(process, at, &[0u8; 8]) == ACCESS_DENIED,
    )?;
    for h in [thread, process] {
        // SAFETY: the call takes no pointers.
        check("closing", unsafe { sys::handle_close(h) })?;
    }
    Ok(())
}

/// Closing the exception channel while a thread waits in an exception
/// ends its process with exit code -2.
fn closed_exception_channel(s: &Startup) -> Result {
    let (process, channel) = start_debuggee(s, 0, 0)?;
    let (r, _) = report_bytes(channel)?;
    ensure(
        "reporting the exception",
        u32_at(&r, 0) == UNDEFINED_INSTRUCTION,
    )?;
    // SAFETY: the call takes no pointers.
    check("closing the channel", unsafe { sys::handle_close(channel) })?;
    ensure("ending with exit code -2", exit_code(process)? == -2)
}

/// The status of the call `interrupted_caller` made.
static INTERRUPTED_STATUS: AtomicU64 = AtomicU64::new(0);

/// Calls through `channel` with no deadline, records the status and exits.
extern "sysv64" fn interrupted_caller(channel: u64, _: u64) -> ! {
    let (status, ..) = call(channel as Handle, b"wait", None, 64, i64::MAX);
    INTERRUPTED_STATUS.store(status as u64, Ordering::Release);
    // SAFETY: the call takes no pointers.
    unsafe { sys::thread_exit() };
    unreachable!("thread_exit returned");
}

/// A handler without `RESTART` ends a `channel_call` waiting for its reply
/// with `ERR_INTERRUPTED`, and a reply that comes later is discarded.
fn call_interrupted_by_signal(s: &Startup) -> Result {
    let old = catch_usr1(0)?;
    let (mut a, mut b) = (0, 0);
    // SAFETY: the out pointers are valid for the call.
    check("creating a channel", unsafe {
        sys::channel_create(0, &mut a, &mut b)
    })?;
    let caller = spawn2(s, interrupted_caller, u64::from(a), 0)?;
    let request = read_message(b, 2000)?;
    wait_until_blocked(caller)?;
    // SAFETY: the call takes no pointers.
    check("signalling the caller", unsafe {
        sys::thread_signal(caller, USR1, 0)
    })?;
    ensure(
        "ending the caller",
        wait(caller, TERMINATED, in_time()).0 == 0,
    )?;
    ensure(
        "interrupting the call",
        INTERRUPTED_STATUS.load(Ordering::Acquire) as i64 == INTERRUPTED,
    )?;
    ensure("running the handler", HANDLED.load(Ordering::Acquire) == 1)?;
    let mut reply = *b"....late";
    reply[..4].copy_from_slice(&request.bytes[..4]);
    // SAFETY: the pointer and length describe `reply`.
    check("replying late", unsafe {
        sys::channel_write(b, reply.as_ptr(), 8, core::ptr::null(), 0)
    })?;
    ensure(
        "discarding the late reply",
        read_sizes(a, 16, 2).0 == SHOULD_WAIT,
    )?;
    restore_usr1(&old)?;
    for h in [a, b] {
        // SAFETY: the call takes no pointers.
        check("closing", unsafe { sys::handle_close(h) })?;
    }
    Ok(())
}

/// A thread suspended while it sleeps in `thread_sleep` goes on sleeping
/// once resumed, until its deadline, and sees no interruption.
fn suspended_sleep_goes_on(s: &Startup) -> Result {
    let deadline = now() + 100_000_000;
    let thread = spawn2(s, signal_sleeper, deadline as u64, 0)?;
    wait_until_blocked(thread)?;
    suspend_and_wait(thread)?;
    check("resuming", resume_thread(thread))?;
    ensure(
        "ending the sleeper",
        wait(thread, TERMINATED, in_time()).0 == 0,
    )?;
    let [status, ended] = SLEEP_RESULT
        .each_ref()
        .map(|a| a.load(Ordering::Acquire) as i64);
    ensure(
        "sleeping until the deadline",
        status == 0 && ended >= deadline,
    )
}

/// A `timeline_wait_async` registration that `port_cancel` removed or
/// whose timeline was closed queues no packet, while one that is met
/// queues a `TIMELINE` packet with its key.
fn timeline_registrations_removed(_: &Startup) -> Result {
    let (mut timeline, mut port) = (0, 0);
    // SAFETY: the out pointers are valid for the calls; the others take
    // no pointers.
    unsafe {
        check(
            "creating a timeline",
            sys::timeline_create(0, 0, &mut timeline),
        )?;
        check("creating a port", sys::port_create(0, &mut port))?;
        check(
            "registering",
            sys::timeline_wait_async(timeline, port, 7, 5),
        )?;
        check("canceling", sys::port_cancel(port, timeline, 7))?;
        check("signalling", sys::timeline_signal(timeline, 10))?;
    }
    ensure(
        "queueing nothing once canceled",
        take_packet(port, soon()).0 == TIMED_OUT,
    )?;
    // SAFETY: the calls take no pointers.
    unsafe {
        check(
            "registering again",
            sys::timeline_wait_async(timeline, port, 8, 20),
        )?;
        check("closing the timeline", sys::handle_close(timeline))?;
    }
    ensure(
        "queueing nothing once closed",
        take_packet(port, soon()).0 == TIMED_OUT,
    )?;
    // SAFETY: the out pointer is valid for the call; the others take no
    // pointers.
    unsafe {
        check(
            "creating another timeline",
            sys::timeline_create(0, 0, &mut timeline),
        )?;
        check(
            "registering on it",
            sys::timeline_wait_async(timeline, port, 9, 1),
        )?;
        check("meeting the value", sys::timeline_signal(timeline, 1))?;
    }
    let (status, packet) = take_packet(port, soon());
    ensure(
        "queueing the packet of the registration met",
        status == 0 && u64_at(&packet, 0) == 9 && u32_at(&packet, 8) == 4,
    )?;
    for h in [timeline, port] {
        // SAFETY: the call takes no pointers.
        check("closing", unsafe { sys::handle_close(h) })?;
    }
    Ok(())
}

/// The debugger calls refuse a range that a physical memory object backs,
/// here the framebuffer's, when there is one.
fn physical_memory_not_debugged(s: &Startup) -> Result {
    let Some(fb) = s.framebuffer else {
        return Ok(());
    };
    let at = map_read_only(s, fb.memory)?;
    ensure(
        "refusing to read it",
        read_memory(s.process, at, &mut [0u8; 8]) == NOT_SUPPORTED,
    )?;
    ensure(
        "refusing to write it",
        write_memory(s.process, at, &[0u8; 8]) == NOT_SUPPORTED,
    )?;
    // SAFETY: the call takes no pointers.
    check("unmapping", unsafe {
        sys::vm_unmap(s.process, at, PAGE_SIZE)
    })
}

/// Reads the byte at `addr`, which the test expects to fault, and exits.
extern "sysv64" fn fault_reader(addr: u64, _: u64) -> ! {
    // SAFETY: `addr` lies in a mapping; a fault is reported, and the test
    // kills the thread.
    unsafe { (addr as *const u8).read_volatile() };
    // SAFETY: the call takes no pointers.
    unsafe { sys::thread_exit() };
    unreachable!("thread_exit returned");
}

/// Starts a thread reading `addr` and checks that it reports a
/// `PAGE_FAULT` at that address on `channel`; then kills the thread.
fn expect_page_fault(s: &Startup, channel: Handle, addr: u64) -> Result {
    let thread = spawn2(s, fault_reader, addr, 0)?;
    let (r, reported) = report_bytes(channel)?;
    ensure(
        "reporting a page fault at the address",
        u32_at(&r, 0) == PAGE_FAULT
            && u64_at(&r, 0x18) == addr
            && u64_at(&r, 0x10) == koid(thread)?,
    )?;
    // SAFETY: the calls take no pointers.
    unsafe {
        check("killing the thread", sys::task_kill(thread))?;
        check("closing the reported thread", sys::handle_close(reported))?;
    }
    ensure(
        "ending the thread",
        wait(thread, TERMINATED, in_time()).0 == 0,
    )
}

/// A user access faults with a `PAGE_FAULT` report on a page its pager
/// failed, on a page that was present when its object was revoked, and on
/// a page that a shrink cut off. The test gives this process an exception
/// channel, so that the faults are reported instead of ending it.
fn faults_on_failed_revoked_and_cut_pages(s: &Startup) -> Result {
    const FAIL: u32 = 1;
    let mut channel = 0;
    // SAFETY: the out pointer is valid for the call.
    check("creating an exception channel", unsafe {
        sys::exception_channel_create(s.process, 0, &mut channel)
    })?;
    let paged_object = || -> Result<(Handle, Handle)> {
        let (mut pager, mut port, mut mem) = (0, 0, 0);
        // SAFETY: the pointers are valid for the calls.
        unsafe {
            check("creating a pager", sys::pager_create(0, &mut pager))?;
            check("creating a port", sys::port_create(0, &mut port))?;
            let status = sys::pager_create_memory(pager, port, 14, PAGE_SIZE, 0, &mut mem);
            check("creating a paged object", status)?;
        }
        Ok((pager, mem))
    };

    let (pager, failed) = paged_object()?;
    check(
        "failing the page",
        pager_op(pager, failed, FAIL, 0, PAGE_SIZE),
    )?;
    expect_page_fault(s, channel, map(s.process, failed, PAGE_SIZE)?)?;

    let (pager, revoked) = paged_object()?;
    // SAFETY: the call takes no pointers.
    check("supplying the page", unsafe {
        sys::pager_supply(pager, revoked, 0, PAGE_SIZE, create_memory(PAGE_SIZE)?, 0)
    })?;
    let at = map(s.process, revoked, PAGE_SIZE)?;
    // SAFETY: the page is mapped and present.
    unsafe { (at as *const u8).read_volatile() };
    // SAFETY: the call takes no pointers.
    check("revoking", unsafe { sys::pager_revoke(pager, revoked) })?;
    expect_page_fault(s, channel, at)?;

    let mut cut = 0;
    // SAFETY: the out pointer is valid for the call.
    check("creating a resizable object", unsafe {
        sys::mem_create(2 * PAGE_SIZE, 1, &mut cut)
    })?;
    let at = map(s.process, cut, 2 * PAGE_SIZE)?;
    // SAFETY: both pages are mapped writable.
    unsafe { ((at + PAGE_SIZE) as *mut u8).write_volatile(1) };
    // SAFETY: the call takes no pointers.
    check("shrinking", unsafe { sys::mem_set_size(cut, PAGE_SIZE) })?;
    expect_page_fault(s, channel, at + PAGE_SIZE)?;
    // SAFETY: the call takes no pointers.
    check("closing the exception channel", unsafe {
        sys::handle_close(channel)
    })
}

/// Whether `needle` occurs in `haystack`.
fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack.windows(needle.len()).any(|w| w == needle)
}

/// `log_write` refuses a severity above 3, a message longer than 256
/// bytes and one that is not UTF-8; `log_read` returns the first record
/// after a sequence, the first one the kernel wrote naming its build
/// kind, and fails with `ERR_SHOULD_WAIT` past the newest. This test runs
/// early, while the first record is still in the log.
fn log_writes_and_reads(s: &Startup) -> Result {
    let long = [b'x'; 257];
    // SAFETY: the pointers and lengths describe the messages.
    unsafe {
        ensure(
            "refusing severity 4",
            sys::log_write(s.log, 4, b"x".as_ptr(), 1) == INVALID_ARGS,
        )?;
        ensure(
            "refusing 257 bytes",
            sys::log_write(s.log, 1, long.as_ptr(), 257) == INVALID_ARGS,
        )?;
        ensure(
            "refusing bytes that are not UTF-8",
            sys::log_write(s.log, 1, [0xFFu8, 0xFE].as_ptr(), 2) == INVALID_ARGS,
        )?;
    }
    let read = |after: u64| {
        let mut record = [0u8; 288];
        // SAFETY: the pointer describes `record`.
        let status = unsafe { sys::log_read(s.log, after, record.as_mut_ptr()) };
        (status, record)
    };
    let (status, first) = read(0);
    check("reading the first record", status)?;
    let len = usize::from(u16::from_le_bytes([first[0x1A], first[0x1B]]));
    let message = &first[0x20..0x20 + len.min(256)];
    ensure(
        "starting with the kernel's own record",
        u64_at(&first, 0) == 1 && first[0x19] == 0 && contains(message, b"Mikros kernel"),
    )?;
    ensure(
        "naming the build kind",
        [&b"production"[..], b"debug", b"fuzz"]
            .iter()
            .any(|kind| contains(message, kind)),
    )?;
    let (status, second) = read(1);
    check("reading the next record", status)?;
    ensure("returning the next sequence", u64_at(&second, 0) == 2)?;
    ensure(
        "finding none past the newest",
        read(u64::MAX - 1).0 == SHOULD_WAIT,
    )
}

/// A child budget starts with the limits given, no usage and both
/// thresholds disabled. Its thresholds can be set, and `OVER_THRESHOLD`
/// follows its usage as a process charged to it comes and goes.
fn budget_creation_and_thresholds(s: &Startup) -> Result {
    const OVER_THRESHOLD: u32 = 1 << 6;
    let mut child = 0;
    // SAFETY: the out pointer is valid for the call.
    check("creating a budget", unsafe {
        sys::budget_create(s.budget, 1 << 20, 64, &mut child)
    })?;
    let r = info::<0x38>(child, 6)?;
    ensure(
        "taking the limits",
        u64_at(&r, 0x00) == 1 << 20 && u64_at(&r, 0x10) == 64,
    )?;
    ensure(
        "starting with no usage",
        u64_at(&r, 0x08) == 0 && u64_at(&r, 0x18) == 0 && u32_at(&r, 0x20) == 0,
    )?;
    ensure(
        "disabling the thresholds",
        u64_at(&r, 0x28) == u64::MAX && u64_at(&r, 0x30) == u64::MAX,
    )?;
    // SAFETY: the call takes no pointers.
    check("setting the thresholds", unsafe {
        sys::budget_set_thresholds(child, 1, 32)
    })?;
    let r = info::<0x38>(child, 6)?;
    ensure(
        "keeping the thresholds",
        u64_at(&r, 0x28) == 1 && u64_at(&r, 0x30) == 32,
    )?;
    ensure(
        "staying below the thresholds",
        wait(child, OVER_THRESHOLD, now()).0 == TIMED_OUT,
    )?;
    let (name, mut process) = (b"charged", 0);
    // SAFETY: the pointers describe `name` and `process`.
    check("creating a process in the budget", unsafe {
        sys::process_create(child, name.as_ptr(), name.len() as u64, &mut process)
    })?;
    let r = info::<0x38>(child, 6)?;
    ensure(
        "charging the process",
        u64_at(&r, 0x08) > 0 && u32_at(&r, 0x20) == 1,
    )?;
    ensure(
        "crossing the kernel threshold",
        wait(child, OVER_THRESHOLD, now()).0 == 0,
    )?;
    // SAFETY: the call takes no pointers.
    check("closing the process", unsafe { sys::handle_close(process) })?;
    ensure(
        "releasing the charge",
        u64_at(&info::<0x38>(child, 6)?, 0x08) == 0,
    )?;
    ensure(
        "falling below the threshold",
        wait(child, OVER_THRESHOLD, now()).0 == TIMED_OUT,
    )?;
    // SAFETY: the call takes no pointers.
    check("closing the budget", unsafe { sys::handle_close(child) })
}

/// The monotonic clock never goes back, and the clock page that init gets
/// has its rights and header: version 1, an even sequence, and the
/// counter as source only with the time stamp counter, whose values then
/// lie between those of the clock before and after.
fn monotonic_clock_and_page(s: &Startup) -> Result {
    let mut last = now();
    for _ in 0..1000 {
        let t = now();
        ensure("never going back", t >= last)?;
        last = t;
    }
    let rights = (1 << 0) | (1 << 1) | (1 << 2) | (1 << 5) | (1 << 9);
    ensure(
        "giving the clock page its rights",
        u32_at(&info::<0x18>(s.clock_page, 1)?, 20) == rights,
    )?;
    let at = map_read_only(s, s.clock_page)?;
    // SAFETY: the page is mapped readable, and only the kernel writes it.
    let page = unsafe { (at as *const [u8; 0x28]).read_volatile() };
    ensure(
        "starting the header",
        u32_at(&page, 0x00) % 2 == 0 && u32_at(&page, 0x04) == 1,
    )?;
    match u32_at(&page, 0x08) {
        0 => {}
        1 => {
            let before = now();
            let tsc: u64;
            // SAFETY: RDTSC only reads the counter.
            unsafe {
                core::arch::asm!(
                    "rdtsc",
                    "shl rdx, 32",
                    "or rax, rdx",
                    out("rax") tsc,
                    out("rdx") _,
                    options(nomem, nostack),
                )
            };
            let after = now();
            let (mult, shift) = (u32_at(&page, 0x10), u32_at(&page, 0x14));
            let (tsc_base, ns_base) = (u64_at(&page, 0x18), u64_at(&page, 0x20) as i64);
            let delta = (u128::from(tsc - tsc_base) * u128::from(mult)) >> shift;
            let computed = ns_base + delta as i64;
            ensure(
                "computing the clock from the page",
                (before..=after).contains(&computed),
            )?;
        }
        _ => ensure("naming a defined source", false)?,
    }
    // SAFETY: the call takes no pointers.
    check("unmapping the clock page", unsafe {
        sys::vm_unmap(s.process, at, PAGE_SIZE)
    })
}

/// Maps the page of `mem` read-only into this process.
fn map_read_only(s: &Startup, mem: Handle) -> Result<u64> {
    let mut at = 0;
    // SAFETY: the out pointer is valid for the call.
    let status = unsafe { sys::vm_map(s.process, PERM_READ, 0, mem, 0, PAGE_SIZE, &mut at) };
    check("mapping read-only", status)?;
    Ok(at)
}

/// A mapping keeps its object after the last handle to it is closed, and
/// nothing is or can be mapped in the low 64 KiB.
fn mappings_outlive_handles(s: &Startup) -> Result {
    let mem = create_memory(PAGE_SIZE)?;
    let at = map(s.process, mem, PAGE_SIZE)?;
    // SAFETY: the call takes no pointers.
    check("closing the handle", unsafe { sys::handle_close(mem) })?;
    // SAFETY: the mapping is readable and writable.
    let back = unsafe {
        (at as *mut u64).write_volatile(0x5A5A_A5A5);
        (at as *const u64).read_volatile()
    };
    ensure("using the mapping", back == 0x5A5A_A5A5)?;
    ensure(
        "reading the object through the mapping",
        read_memory(s.process, at, &mut [0u8; 8]) == 0,
    )?;
    // SAFETY: the call takes no pointers.
    check("unmapping", unsafe {
        sys::vm_unmap(s.process, at, PAGE_SIZE)
    })?;
    let mem = create_memory(PAGE_SIZE)?;
    let mut addr = 0;
    // SAFETY: the out pointer is valid for the call.
    let status = unsafe {
        sys::vm_map(
            s.process,
            PERM_READ | mikros_test::SPECIFIC,
            0x1000,
            mem,
            0,
            PAGE_SIZE,
            &mut addr,
        )
    };
    ensure("refusing the low 64 KiB", status == OUT_OF_RANGE)?;
    ensure(
        "mapping nothing there",
        read_memory(s.process, 0x1000, &mut [0u8; 1]) == NOT_FOUND,
    )?;
    // SAFETY: the call takes no pointers.
    check("closing", unsafe { sys::handle_close(mem) })
}

const TESTS: [(&str, Test); 80] = [
    ("boot_memory_uncharged", boot_memory_uncharged),
    ("log_writes_and_reads", log_writes_and_reads),
    ("info_topics", info_topics),
    ("events_and_waits", events_and_waits),
    ("timers", timers),
    ("priorities", priorities),
    ("ports", ports),
    ("log_readers", log_readers),
    ("memory_operations", memory_operations),
    ("large_memory_operations", large_memory_operations),
    (
        "region_charges_reach_the_limit",
        region_charges_reach_the_limit,
    ),
    ("seals", seals),
    ("channel_calls", channel_calls),
    ("futexes", futexes),
    ("timelines", timelines),
    ("streams", streams),
    ("failed_outputs", failed_outputs),
    ("outputs_outside_user_range", outputs_outside_user_range),
    ("inputs_outside_user_memory", inputs_outside_user_memory),
    ("undefined_options_refused", undefined_options_refused),
    ("reserved_hardware_refused", reserved_hardware_refused),
    ("clones", clones),
    ("pagers", pagers),
    ("pager_operations", pager_operations),
    ("large_pager_operations", large_pager_operations),
    ("paged_outputs", paged_outputs),
    ("access_flag_cleared_on_entry", access_flag_cleared_on_entry),
    ("kill_blocked_thread", kill_blocked_thread),
    (
        "exception_reports_and_actions",
        exception_reports_and_actions,
    ),
    ("thread_events", thread_events),
    ("start_refusals", start_refusals),
    ("kill_stopped_thread", kill_stopped_thread),
    ("debug_at_syscall_entry", debug_at_syscall_entry),
    ("kill_new_thread", kill_new_thread),
    (
        "kill_thread_with_queued_report",
        kill_thread_with_queued_report,
    ),
    ("kill_thread_after_report", kill_thread_after_report),
    ("own_endpoints_not_sent", own_endpoints_not_sent),
    ("channel_errors", channel_errors),
    ("endpoint_cycles_reclaimed", endpoint_cycles_reclaimed),
    ("coverage", coverage),
    ("seal_waits_for_outputs", seal_waits_for_outputs),
    ("evict_waits_for_outputs", evict_waits_for_outputs),
    ("supply_waits_for_outputs", supply_waits_for_outputs),
    ("clone_waits_for_outputs", clone_waits_for_outputs),
    (
        "map_checks_again_after_waiting",
        map_checks_again_after_waiting,
    ),
    (
        "seal_and_revoke_wait_for_supply",
        seal_and_revoke_wait_for_supply,
    ),
    ("memory_objects", memory_objects),
    ("shared_mappings", shared_mappings),
    ("random_bytes", random_bytes),
    ("write_execute_policy", write_execute_policy),
    ("syscall_gate", syscall_gate),
    ("ioports_as_granted", ioports_as_granted),
    ("sleeper_runs_beside_spinner", sleeper_runs_beside_spinner),
    ("suspend_threads", suspend_threads),
    ("suspend_blocked_waits", suspend_blocked_waits),
    ("suspend_in_call", suspend_in_call),
    ("thread_lists", thread_lists),
    ("read_process_memory", read_process_memory),
    ("stack_guard_page_kept", stack_guard_page_kept),
    ("write_process_memory", write_process_memory),
    ("threads", threads),
    ("new_thread_starts_clean", new_thread_starts_clean),
    ("registers_survive_preemption", registers_survive_preemption),
    ("user_bases_set_and_kept", user_bases_set_and_kept),
    (
        "budget_creation_and_thresholds",
        budget_creation_and_thresholds,
    ),
    ("monotonic_clock_and_page", monotonic_clock_and_page),
    ("mappings_outlive_handles", mappings_outlive_handles),
    ("signal_frame", signal_frame),
    ("signal_frame_reserved_bytes", signal_frame_reserved_bytes),
    (
        "signal_interrupts_and_restarts",
        signal_interrupts_and_restarts,
    ),
    ("extended_state_round_trip", extended_state_round_trip),
    ("startup_handle_rights", startup_handle_rights),
    ("debugging_needs_manage", debugging_needs_manage),
    ("closed_exception_channel", closed_exception_channel),
    ("call_interrupted_by_signal", call_interrupted_by_signal),
    ("suspended_sleep_goes_on", suspended_sleep_goes_on),
    (
        "timeline_registrations_removed",
        timeline_registrations_removed,
    ),
    ("physical_memory_not_debugged", physical_memory_not_debugged),
    (
        "faults_on_failed_revoked_and_cut_pages",
        faults_on_failed_revoked_and_cut_pages,
    ),
    ("processes_and_channels", processes_and_channels),
];

fn main(s: &Startup) -> i64 {
    if let Err(f) = open_exit_port(s.root_resource, s.process) {
        log!(Error, "TEST setup FAILED: {} ({})", f.what, f.status);
        return 1;
    }
    let mut failed = 0;
    for (name, test) in TESTS {
        match test(s) {
            Ok(()) => log!(Info, "TEST {name} PASSED"),
            Err(f) => {
                failed += 1;
                log!(Error, "TEST {name} FAILED: {} ({})", f.what, f.status);
            }
        }
    }
    log!(
        Info,
        "TESTS DONE: {} passed, {failed} failed",
        TESTS.len() - failed
    );
    exit_qemu(if failed == 0 { PASSED } else { FAILED })
}
