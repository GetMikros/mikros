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
    // SAFETY: the call takes no pointers; the stack is mapped and unused.
    let status = unsafe { sys::thread_start(thread, entry, at + stack_size - 8, 77, 0) };
    check("starting the thread", status)?;
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
    // SAFETY: the call takes no pointers.
    let status =
        unsafe { sys::process_start(process, thread, loaded.entry, loaded.stack, theirs, 0) };
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

fn protect(s: &Startup, perms: u32, addr: u64, len: u64) -> i64 {
    // SAFETY: the call takes no pointers.
    unsafe { sys::vm_protect(s.process, perms, addr, len) }
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
    ensure("keeping the packet", status == 0 && u64_at(&p, 0) == 5)
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
    let status = unsafe { sys::mem_clone(source, 0, PAGE_SIZE - 5, PAGE_SIZE + 100, &mut clone) };
    check("cloning", status)?;
    ensure(
        "copying the range",
        mem_read_at::<14>(clone, 0)? == *b"\0\0across pages",
    )?;
    ensure(
        "reading zero elsewhere",
        mem_read_at::<8>(clone, 200)? == [0; 8],
    )?;
    mem_write_at(source, PAGE_SIZE - 3, b"AC")?;
    ensure("keeping the clone", mem_read_at::<4>(clone, 2)? == *b"acro")?;
    mem_write_at(clone, 2, b"x")?;
    ensure(
        "keeping the source",
        mem_read_at::<4>(source, PAGE_SIZE - 3)? == *b"ACro",
    )?;
    let record = info::<0x18>(clone, 4)?;
    ensure("sizing the clone", u64_at(&record, 0) == PAGE_SIZE + 100)?;
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
            b'0' + (offset / PAGE_SIZE) as u8,
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
    let (_pager, mem) = paged_object(s, 3, 0)?;
    let at = map(s.process, mem, 3 * PAGE_SIZE)?;
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
    ensure(
        "requesting each page",
        REQUESTS.load(Ordering::Acquire) == before + 3,
    )
}

const IO: i64 = -28;
const BAD_STATE: i64 = -20;

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

type Test = fn(&Startup) -> Result;

const TESTS: [(&str, Test); 20] = [
    ("info_topics", info_topics),
    ("events_and_waits", events_and_waits),
    ("timers", timers),
    ("priorities", priorities),
    ("ports", ports),
    ("log_readers", log_readers),
    ("memory_operations", memory_operations),
    ("seals", seals),
    ("channel_calls", channel_calls),
    ("futexes", futexes),
    ("timelines", timelines),
    ("streams", streams),
    ("failed_outputs", failed_outputs),
    ("clones", clones),
    ("pagers", pagers),
    ("pager_operations", pager_operations),
    ("paged_outputs", paged_outputs),
    ("memory_objects", memory_objects),
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
