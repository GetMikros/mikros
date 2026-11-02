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
fn processes_and_channels(s: &Startup) -> Result {
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
    let program = ustar::find(image, b"tests/channel-echo");
    ensure("finding tests/channel-echo", program.is_some())?;

    let (mut process, mut thread) = (0, 0);
    let name = b"channel-echo";
    // SAFETY: the pointers describe `name` and `process`.
    let status =
        unsafe { sys::process_create(s.budget, name.as_ptr(), name.len() as u64, &mut process) };
    check("creating a process", status)?;
    // SAFETY: the pointers describe `name` and `thread`.
    let status =
        unsafe { sys::thread_create(process, name.as_ptr(), name.len() as u64, &mut thread) };
    check("creating its thread", status)?;
    let loaded = elf::load(process, program.unwrap_or_default())?;
    let (mut ours, mut theirs) = (0, 0);
    // SAFETY: the out pointers are valid for the call.
    check("creating a channel", unsafe {
        sys::channel_create(0, &mut ours, &mut theirs)
    })?;
    // SAFETY: the call takes no pointers.
    let status =
        unsafe { sys::process_start(process, thread, loaded.entry, loaded.stack, theirs, 0) };
    check("starting the process", status)?;

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

type Test = fn(&Startup) -> Result;

const TESTS: [(&str, Test); 4] = [
    ("info_topics", info_topics),
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
