//! The kernel's threads, as a system test: starting threads of the
//! test's own process, their FS and GS bases, the state a new thread
//! starts with, registers kept across preemption, suspending threads
//! and their extended state, waits, calls and sleeps that a suspension
//! interrupts, the right that debugging needs, the thread list of a
//! process, and futexes.

#![no_std]
#![no_main]

extern crate alloc;

use core::sync::atomic::{AtomicU64, Ordering};
use mikros_rt::Managed;
use mikros_rt::sys::{self, Handle};
use mikros_test::*;

mikros_rt::managed_main!(main);

fn main(m: &Managed) -> i64 {
    mikros_test::run(
        m,
        &[
            ("threads", threads),
            ("user_bases_set_and_kept", user_bases_set_and_kept),
            ("thread_lists", thread_lists),
            ("futexes", futexes),
            ("new_thread_starts_clean", new_thread_starts_clean),
            ("registers_survive_preemption", registers_survive_preemption),
            ("extended_state_round_trip", extended_state_round_trip),
            ("suspend_threads", suspend_threads),
            ("suspend_blocked_waits", suspend_blocked_waits),
            ("suspend_in_call", suspend_in_call),
            ("suspended_sleep_goes_on", suspended_sleep_goes_on),
            ("debugging_needs_manage", debugging_needs_manage),
        ],
    )
}

const STATE_SUSPENDED: u32 = 5;

static THREAD_RAN: AtomicU64 = AtomicU64::new(0);

extern "sysv64" fn thread_entry(value: u64, _arg2: u64) -> ! {
    THREAD_RAN.store(value, Ordering::Release);
    // SAFETY: the call takes no pointers.
    unsafe { sys::thread_exit() };
    unreachable!("thread_exit returned");
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

/// Ends a thread whose entry is written in assembly.
extern "sysv64" fn end_asm_thread() -> ! {
    // SAFETY: the call takes no pointers.
    unsafe { sys::thread_exit() };
    unreachable!("thread_exit returned");
}

/// A second thread of this process runs and exits.
fn threads(s: &Own) -> Result {
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

/// The index of the waiter that `futex_waiter` recorded as woken
/// `at`-th, once one has, or a failure after `in_time`.
fn futex_woken(at: usize) -> Result<u64> {
    let deadline = in_time();
    loop {
        let index = FUTEX_ORDER[at].load(Ordering::Acquire);
        if index != 0 {
            return Ok(index);
        }
        ensure("recording the woken waiter", now() < deadline)?;
        sleep_ms(1);
    }
}

/// Futexes compare before blocking, time out, wake in arrival order and
/// are keyed by memory object and offset.
fn futexes(s: &Own) -> Result {
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
    // Each waiter is blocked before the next starts, so that they wait
    // in the order of their indices.
    for index in 1..=3 {
        let waiter = spawn2(s, futex_waiter, word, index)?;
        wait_until_blocked(waiter)?;
    }
    ensure("waking one", futex_wake(word, 1)? == 1)?;
    ensure("waking the oldest", futex_woken(0)? == 1)?;
    // The second mapping reaches the same futex. Waiters are woken one
    // at a time, as threads woken together may run in either order.
    ensure("waking the next", futex_wake(second + 64, 1)? == 1)?;
    ensure("waking in order", futex_woken(1)? == 2)?;
    ensure("waking the rest", futex_wake(word, u32::MAX)? == 1)?;
    ensure("waking the last", futex_woken(2)? == 3)?;
    ensure("waking nobody", futex_wake(word, 1)? == 0)
}

/// A process lists its threads that are not dead, and hands out handles
/// to them by object ID.
fn thread_lists(s: &Own) -> Result {
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
fn user_bases_set_and_kept(_: &Own) -> Result {
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

/// A new thread starts with its arguments in RDI and RSI, its stack in
/// RSP, every other general-purpose register, the FS and GS bases and
/// every vector register zero, RFLAGS 0x202, the x87 control word 0x037F
/// and MXCSR 0x1F80.
fn new_thread_starts_clean(s: &Own) -> Result {
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
fn registers_survive_preemption(s: &Own) -> Result {
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

/// What `start_probe` finds at its first instruction: RAX to R15 in the
/// order of the `GENERAL` layout, RFLAGS, the FS and GS bases, the x87
/// control word and MXCSR, and the 16 YMM registers.
static mut START_STATE: [u64; 21] = [0; 21];
static mut START_YMM: [[u64; 4]; 16] = [[0; 4]; 16];

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

/// A suspended thread stops running until every suspension is undone,
/// shows its registers, and can be killed.
fn suspend_threads(s: &Own) -> Result {
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

/// The values `extended_keeper` loads into XMM0 and XMM1; set by it once
/// they are loaded, and by the test to let it go on; and what it then
/// finds in XMM0 and XMM1.
static EXTENDED_PATTERN: [[u64; 2]; 2] = [
    [0x0011_2233_4455_6677, 0x8899_AABB_CCDD_EEFF],
    [0x0F0E_0D0C_0B0A_0908, 0x0706_0504_0302_0100],
];

static EXTENDED_READY: AtomicU64 = AtomicU64::new(0);

static EXTENDED_GO: AtomicU64 = AtomicU64::new(0);

/// A suspended thread's `EXTENDED` state is an 8-byte size and its
/// registers in `XSAVE` format, and a changed state written back is what
/// the thread finds when it resumes. A state whose MXCSR would make
/// `XRSTOR` fault, a buffer too short, a kind that does not exist and a
/// thread that is not suspended are refused.
fn extended_state_round_trip(s: &Own) -> Result {
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
fn suspend_blocked_waits(s: &Own) -> Result {
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
fn suspend_in_call(s: &Own) -> Result {
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

/// Suspending, resuming and inspecting threads and processes needs the
/// `MANAGE` right.
fn debugging_needs_manage(s: &Own) -> Result {
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

/// A thread suspended while it sleeps in `thread_sleep` goes on sleeping
/// once resumed, until its deadline, and sees no interruption.
fn suspended_sleep_goes_on(s: &Own) -> Result {
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
