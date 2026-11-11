//! The start of a C program: receiving and checking the whole startup
//! transfer, installing what the C library keeps of it, and running
//! musl's start, which runs the initializers and `main`, only after.
//!
//! A program's `_start` calls `__mikros_start` with its bootstrap
//! channel and its `main`. In the static library, it relocates the
//! program and receives the transfer itself. In the shared library,
//! which is also the dynamic linker, the dynamic linker has received it
//! with `__mikros_dynamic_start` before it loaded the program, so
//! `__mikros_start` only runs musl's start with the arguments it kept.

use crate::{clock, fd, fs, syscall};
use alloc::boxed::Box;
use alloc::vec;
use alloc::vec::Vec;
use mikros_rt::interfaces::__midl::OwnedHandle;
use mikros_rt::sys;

/// The exit code of a process whose startup transfer was refused.
const REFUSED: i64 = 127;

const INFO_BASIC: u32 = 1;
const INFO_MEMORY: u32 = 4;
/// The most bytes of the `arguments` memory object that are read.
const ARGUMENTS_MAX: u64 = 1 << 20;

const AT_NULL: usize = 0;
const AT_PHDR: usize = 3;
const AT_PHENT: usize = 4;
const AT_PHNUM: usize = 5;
const AT_PAGESZ: usize = 6;
const AT_BASE: usize = 7;
const AT_ENTRY: usize = 9;
const AT_UID: usize = 11;
const AT_EUID: usize = 12;
const AT_GID: usize = 13;
const AT_EGID: usize = 14;
const AT_RANDOM: usize = 25;
/// The size of an ELF64 program header.
const PHDR_SIZE: usize = 56;
const PAGE_SIZE: usize = 4096;
/// The offset of `e_entry` in an ELF64 header.
const E_ENTRY: usize = 24;
/// The prefix of the app IDs of system components.
const SYSTEM: &[u8] = b"system.";

type Main = extern "C" fn(i32, *mut *mut u8, *mut *mut u8) -> i32;

unsafe extern "C" {
    fn __libc_start_main(
        main: Main,
        argc: i32,
        argv: *mut *mut u8,
        init: usize,
        fini: usize,
        ldso: usize,
    ) -> i32;
}

/// The arguments, environment and auxiliary vector of the program, laid
/// out as the System V ABI lays them out on the stack, from the count of
/// arguments on.
static VECTOR: core::sync::atomic::AtomicPtr<usize> =
    core::sync::atomic::AtomicPtr::new(core::ptr::null_mut());
/// Whether the process is a system component.
static SYSTEM_PROCESS: core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(false);

// The program's `_start` calls this with the bootstrap channel in RDI
// and `main` in RSI. Until the program is relocated, only assembly
// runs; the arguments wait in callee-saved registers meanwhile.
#[cfg(not(feature = "shared"))]
core::arch::global_asm!(
    ".globl __mikros_start",
    ".type __mikros_start, @function",
    "__mikros_start:",
    "and rsp, -16",
    "mov r12, rdi",
    "mov r13, rsi",
    "call __mikros_relocate",
    "mov rdi, r12",
    "mov rsi, r13",
    "call {start}",
    "ud2",
    start = sym start,
);

#[cfg(not(feature = "shared"))]
extern "sysv64" fn start(bootstrap: u64, main: Main) -> ! {
    receive(bootstrap as sys::Handle);
    run(main)
}

/// The program's start in the shared library: the dynamic linker has
/// received the transfer.
#[cfg(feature = "shared")]
#[unsafe(no_mangle)]
extern "C" fn __mikros_start(_bootstrap: u64, main: Main) -> ! {
    run(main)
}

/// Receives the transfer for the dynamic linker, and returns the
/// vector it loads the program with.
#[cfg(feature = "shared")]
#[unsafe(no_mangle)]
extern "C" fn __mikros_dynamic_start(bootstrap: u32) -> *mut usize {
    receive(bootstrap)
}

/// Whether the process is a system component, which the dynamic linker
/// keeps from searching `LD_LIBRARY_PATH`.
#[cfg(feature = "shared")]
#[unsafe(no_mangle)]
extern "C" fn __mikros_system_process() -> i32 {
    i32::from(SYSTEM_PROCESS.load(core::sync::atomic::Ordering::Relaxed))
}

/// Runs musl's start with the program's arguments.
fn run(main: Main) -> ! {
    let vector = VECTOR.load(core::sync::atomic::Ordering::Relaxed);
    // SAFETY: `receive` stored the vector, which begins with the count
    // of arguments, before this runs.
    let argc = unsafe { *vector } as i32;
    // SAFETY: the arguments follow the count, and each list of the
    // vector ends as musl expects.
    let code = unsafe { __libc_start_main(main, argc, vector.add(1).cast(), 0, 0, 0) };
    exit(i64::from(code))
}

/// Receives and checks the whole startup transfer on `bootstrap`,
/// installs what the C library keeps of it, and returns the vector of
/// the program. A refused transfer or malformed arguments end the
/// process.
fn receive(bootstrap: sys::Handle) -> *mut usize {
    // A refused transfer has closed every handle it received.
    let Ok(mut managed) = mikros_rt::managed::receive(bootstrap) else {
        exit(REFUSED)
    };
    let header = &managed.header;
    let tid = object_id(header.thread.raw()).unwrap_or(1);
    syscall::set_process(header.process.raw(), tid);
    crate::thread::install(header.thread.raw());
    crate::process::install(header.pid, header.parent, header.launcher.handle().raw());
    clock::install(
        header.process.raw(),
        header.thread.raw(),
        header.directory.handle().raw(),
        header.clock_page.raw(),
        header.time_page.as_ref().map(|page| page.raw()),
    );
    fd::install(&mut managed.handles);
    fs::install(core::mem::take(&mut managed.namespace));
    crate::library::install(header.bundle.as_deref());
    crate::terminal::control_standard_input();
    SYSTEM_PROCESS.store(
        header.app_id.as_bytes().starts_with(SYSTEM),
        core::sync::atomic::Ordering::Relaxed,
    );
    let Some(strings) = arguments(&header.arguments) else {
        exit(REFUSED)
    };
    let mut random = Box::new([0u8; 16]);
    // SAFETY: the pointer describes `random`.
    unsafe { sys::system_get_random(random.as_mut_ptr(), 16) };
    let base = header.program_base as usize;
    // SAFETY: the program's ELF header is mapped at its base, since the
    // program headers that the loader found lie in its first segment.
    let entry = base + unsafe { core::ptr::read_volatile((base + E_ENTRY) as *const u64) } as usize;
    let auxv = [
        (AT_PHDR, header.program_phdr as usize),
        (AT_PHENT, PHDR_SIZE),
        (AT_PHNUM, header.program_phnum as usize),
        (AT_PAGESZ, PAGE_SIZE),
        (AT_BASE, header.interpreter_base as usize),
        (AT_ENTRY, entry),
        // No program is set-user-ID: these only tell musl so.
        (AT_UID, 0),
        (AT_EUID, 0),
        (AT_GID, 0),
        (AT_EGID, 0),
        (AT_RANDOM, Box::leak(random).as_ptr() as usize),
        (AT_NULL, 0),
    ];
    let mut vector: Vec<usize> = vec![strings.args.len()];
    vector.extend(strings.args.iter().map(|s| s.as_ptr() as usize));
    vector.push(0);
    vector.extend(strings.env.iter().map(|s| s.as_ptr() as usize));
    vector.push(0);
    for (key, value) in auxv {
        vector.extend([key, value]);
    }
    let vector = Vec::leak(vector).as_mut_ptr();
    VECTOR.store(vector, core::sync::atomic::Ordering::Relaxed);
    // The startup handles left go to the program, which takes them; the
    // C library keeps the rest of the transfer, such as the launcher and
    // directory connections, for as long as it runs.
    let directory = managed.header.directory.handle().raw();
    crate::startup::install(core::mem::take(&mut managed.handles), directory);
    core::mem::forget(managed);
    vector
}

fn exit(code: i64) -> ! {
    // SAFETY: ending the process takes no pointers.
    unsafe { sys::process_exit(code) };
    unreachable!("process_exit returned");
}

/// The object ID of `handle`.
pub(crate) fn object_id(handle: sys::Handle) -> Option<u32> {
    let record = info::<0x18>(handle, INFO_BASIC)?;
    Some(u64::from_le_bytes(record[..8].try_into().ok()?) as u32)
}

fn info<const N: usize>(handle: sys::Handle, topic: u32) -> Option<[u8; N]> {
    let mut record = [0u8; N];
    let mut len = 0;
    // SAFETY: the pointers describe `record` and `len`.
    let status =
        unsafe { sys::object_get_info(handle, topic, record.as_mut_ptr(), N as u64, &mut len) };
    (status == mikros_rt::OK && len >= N as u64).then_some(record)
}

/// The arguments and the environment, each string ending in NUL.
struct Strings {
    args: Vec<&'static [u8]>,
    env: Vec<&'static [u8]>,
}

/// Decodes the `arguments` memory object: the argument and environment
/// counts as `u32` values, then each argument and each environment
/// string ending in NUL. Returns `None` when the object holds fewer
/// strings than it counts.
fn arguments(mem: &OwnedHandle) -> Option<Strings> {
    let record = info::<0x18>(mem.raw(), INFO_MEMORY)?;
    let size = u64::from_le_bytes(record[..8].try_into().ok()?).min(ARGUMENTS_MAX);
    let mut bytes = vec![0u8; size as usize];
    // SAFETY: the pointer describes `bytes`.
    let status = unsafe { sys::mem_read(mem.raw(), 0, bytes.as_mut_ptr(), size) };
    if status != mikros_rt::OK || bytes.len() < 8 {
        return None;
    }
    let argc = u32::from_le_bytes(bytes[0..4].try_into().ok()?) as usize;
    let envc = u32::from_le_bytes(bytes[4..8].try_into().ok()?) as usize;
    let bytes: &'static [u8] = Vec::leak(bytes);
    let mut rest = &bytes[8..];
    let mut take = |count: usize| -> Option<Vec<&'static [u8]>> {
        let mut strings = Vec::new();
        for _ in 0..count {
            let end = rest.iter().position(|&b| b == 0)?;
            strings.push(&rest[..=end]);
            rest = &rest[end + 1..];
        }
        Some(strings)
    };
    let args = take(argc)?;
    let env = take(envc)?;
    Some(Strings { args, env })
}
