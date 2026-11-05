//! `__mikros_start`, which a program's `_start` calls with its bootstrap
//! channel and its `main`. It relocates the program, receives and checks
//! the whole startup transfer, installs what the C library keeps of it,
//! and only then runs musl's start, which runs the initializers and
//! `main`.

use crate::{clock, fd, syscall};
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
const AT_RANDOM: usize = 25;
/// The size of an ELF64 program header.
const PHDR_SIZE: usize = 56;
const PAGE_SIZE: usize = 4096;

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

// The program's `_start` calls this with the bootstrap channel in RDI
// and `main` in RSI. Until the program is relocated, only assembly
// runs; the arguments wait in callee-saved registers meanwhile.
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

extern "sysv64" fn start(bootstrap: u64, main: Main) -> ! {
    // A refused transfer has closed every handle it received.
    let Ok(mut managed) = mikros_rt::managed::receive(bootstrap as sys::Handle) else {
        exit(REFUSED)
    };
    let header = &managed.header;
    let tid = object_id(header.thread.raw()).unwrap_or(1);
    syscall::set_process(header.process.raw(), tid);
    clock::install(
        header.process.raw(),
        header.thread.raw(),
        header.directory.handle().raw(),
        header.clock_page.raw(),
        header.time_page.as_ref().map(|page| page.raw()),
    );
    fd::install(&mut managed.handles);
    let Some(strings) = arguments(&header.arguments) else {
        exit(REFUSED)
    };
    let mut random = Box::new([0u8; 16]);
    // SAFETY: the pointer describes `random`.
    unsafe { sys::system_get_random(random.as_mut_ptr(), 16) };
    let auxv = [
        (AT_PHDR, header.program_phdr as usize),
        (AT_PHENT, PHDR_SIZE),
        (AT_PHNUM, header.program_phnum as usize),
        (AT_PAGESZ, PAGE_SIZE),
        (AT_BASE, header.interpreter_base as usize),
        (AT_RANDOM, Box::leak(random).as_ptr() as usize),
        (AT_NULL, 0),
    ];
    // musl reads the environment and the auxiliary vector after the
    // arguments, as the System V ABI lays them out on the stack.
    let mut vector: Vec<usize> = Vec::new();
    vector.extend(strings.args.iter().map(|s| s.as_ptr() as usize));
    vector.push(0);
    vector.extend(strings.env.iter().map(|s| s.as_ptr() as usize));
    vector.push(0);
    for (key, value) in auxv {
        vector.extend([key, value]);
    }
    let argc = strings.args.len() as i32;
    let argv = Vec::leak(vector).as_mut_ptr() as *mut *mut u8;
    // The C library keeps the rest of the transfer, such as the
    // launcher and directory connections, for as long as it runs.
    core::mem::forget(managed);
    // SAFETY: `argv` holds `argc` arguments, the environment and the
    // auxiliary vector, each list ended as musl expects.
    let code = unsafe { __libc_start_main(main, argc, argv, 0, 0, 0) };
    exit(i64::from(code))
}

fn exit(code: i64) -> ! {
    // SAFETY: ending the process takes no pointers.
    unsafe { sys::process_exit(code) };
    unreachable!("process_exit returned");
}

/// The object ID of `handle`.
fn object_id(handle: sys::Handle) -> Option<u32> {
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
