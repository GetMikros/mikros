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
    let deadline = in_time();
    while thread_state(thread)? != STATE_BLOCKED {
        ensure("blocking", now() < deadline)?;
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

fn set_kernel_limit(s: &Startup, limit: u64) -> Result {
    // SAFETY: the call takes no pointers.
    check("setting the limits", unsafe {
        sys::budget_set_limits(s.budget, limit, u64::MAX)
    })
}

/// Mapping and splitting regions is charged to the caller's budget, and
/// a call refused at the limit changes nothing.
fn region_charges_reach_the_limit(s: &Startup) -> Result {
    const NO_RESOURCES: i64 = -3;
    let len = 3 * PAGE_SIZE;
    let mem = create_memory(len)?;
    let mut mapped = [0u64; 1000];
    // The first mapping's middle page is touched before the limit is
    // set, as a fault that needs kernel memory above it would fail.
    mapped[0] = map(s.process, mem, len)?;
    // SAFETY: the mapping is writable.
    unsafe { ((mapped[0] + PAGE_SIZE) as *mut u64).write_volatile(1) };
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
/// pages; protection changes stay within the mapping handle's rights.
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
    // Every address, offset and size of these calls is a whole number of
    // pages.
    // SAFETY: the calls take no pointers or a valid one.
    unsafe {
        ensure(
            "refusing an unaligned op",
            sys::mem_op(mem, 1, 0, PAGE_SIZE + 1) == -10,
        )?;
        ensure(
            "refusing an unaligned size",
            sys::mem_set_size(mem, PAGE_SIZE + 1) == -10,
        )?;
        let mut unused = 0;
        ensure(
            "refusing an unaligned mapping",
            sys::vm_map(s.process, PERM_READ, 0, mem, 0, 100, &mut unused) == -10,
        )?;
        ensure(
            "refusing an unaligned unmapping",
            sys::vm_unmap(s.process, at, 100) == -10,
        )?;
    }
    ensure(
        "refusing an unaligned protection",
        protect(s, PERM_READ, at, 100) == -10,
    )?;

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
    // The second mapping reaches the same futex.
    ensure("waking the rest", futex_wake(second + 64, u32::MAX)? == 2)?;
    sleep_ms(5);
    ensure(
        "waking in order",
        FUTEX_ORDER[1].load(Ordering::Acquire) == 2 && FUTEX_ORDER[2].load(Ordering::Acquire) == 3,
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
/// `WRITE`, sealed objects and revoked objects refuse the whole write.
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

const TESTS: [(&str, Test); 42] = [
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
    ("clones", clones),
    ("pagers", pagers),
    ("pager_operations", pager_operations),
    ("large_pager_operations", large_pager_operations),
    ("paged_outputs", paged_outputs),
    ("kill_blocked_thread", kill_blocked_thread),
    ("kill_stopped_thread", kill_stopped_thread),
    ("debug_at_syscall_entry", debug_at_syscall_entry),
    ("coverage", coverage),
    ("seal_waits_for_outputs", seal_waits_for_outputs),
    ("evict_waits_for_outputs", evict_waits_for_outputs),
    ("supply_waits_for_outputs", supply_waits_for_outputs),
    ("clone_waits_for_outputs", clone_waits_for_outputs),
    ("memory_objects", memory_objects),
    ("shared_mappings", shared_mappings),
    ("random_bytes", random_bytes),
    ("write_execute_policy", write_execute_policy),
    ("syscall_gate", syscall_gate),
    ("sleeper_runs_beside_spinner", sleeper_runs_beside_spinner),
    ("suspend_threads", suspend_threads),
    ("suspend_blocked_waits", suspend_blocked_waits),
    ("suspend_in_call", suspend_in_call),
    ("thread_lists", thread_lists),
    ("read_process_memory", read_process_memory),
    ("write_process_memory", write_process_memory),
    ("threads", threads),
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
