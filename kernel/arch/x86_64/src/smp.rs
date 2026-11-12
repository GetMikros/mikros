//! Finding and starting the other processors.

use crate::apic;
use crate::control::Optional;
use crate::features::Leaves;
use crate::gdt;
use crate::log::log;
use crate::memory;
use crate::paging::{Flags, Mapper, PAGE_SIZE, Policy, Tables};
use crate::percpu::{self, PerCpu};
use crate::space;
use alloc::vec::Vec;
use boot_protocol::BootInfo;
use core::sync::atomic::{AtomicBool, AtomicPtr, AtomicU8, AtomicU64, AtomicUsize, Ordering};
use kernel_core::acpi::madt;
use kernel_core::cpu::Cpu;
use kernel_core::smp::{self, MAX_CPUS};
use kernel_core::sync::SpinLock;

/// The APIC ID of processor `n` at index `n`, set by `find`.
pub static APIC_IDS: SpinLock<Vec<u32>> =
    SpinLock::new(kernel_core::lock_order::SMP_APIC_IDS, Vec::new());

/// Numbers the enabled processors that the MADT `madt` lists, with
/// `boot`, the boot processor's APIC ID, as processor 0, and logs those
/// beyond the kernel's limit.
pub fn find(madt: &[u8], boot: u32) {
    let found = smp::number(boot, madt::enabled_processors(madt), MAX_CPUS)
        .unwrap_or_else(|| panic!("no memory for the processor list"));
    for id in &found.beyond {
        log!(
            Warning,
            "processor with APIC ID {id} not used: more than {MAX_CPUS} processors"
        );
    }
    log!(Info, "{} processors found", found.used.len());
    *APIC_IDS.lock() = found.used;
}

/// Offsets in the trampoline page of the data the code there reads: its
/// GDT and GDT pointer, the far pointer to its 64-bit code, the startup
/// page table, and the stack, entry point and argument of the processor.
const T_GDT: u64 = 0xF80;
const T_GDTR: u64 = 0xF98;
const T_FAR: u64 = 0xFA0;
const T_CR3: u64 = 0xFA8;
const T_STACK: u64 = 0xFB0;
const T_ENTRY: u64 = 0xFB8;
const T_ARG: u64 = 0xFC0;

/// The trampoline's GDT: null, 64-bit code at 0x08 and data at 0x10.
const T_DESCRIPTORS: [u64; 3] = [0, 0x00AF_9A00_0000_FFFF, 0x00CF_9200_0000_FFFF];

// The code copied into the trampoline page. A startup interrupt starts
// the processor in real mode at the page's first byte, with CS holding
// the page's physical address divided by 16. The code loads the GDT,
// enables PAE, the startup page table, long mode and no-execute pages,
// and then paging and protection together, and jumps to its 64-bit part,
// which takes the stack and the argument, points the GS base at the
// argument, the processor's data, and jumps to the entry point. The
// fuzz build's coverage hook reads that data through GS from the entry
// point's first instruction on.
core::arch::global_asm!(
    ".pushsection .rodata.ap_trampoline, \"a\"",
    ".global ap_trampoline_start",
    ".global ap_trampoline_long",
    ".global ap_trampoline_end",
    ".code16",
    "ap_trampoline_start:",
    "cli",
    "cld",
    "movw %cs, %ax",
    "movw %ax, %ds",
    "lgdtl {gdtr}",
    "movl %cr4, %eax",
    "orl $0x20, %eax",
    "movl %eax, %cr4",
    "movl {cr3}, %eax",
    "movl %eax, %cr3",
    "movl $0xC0000080, %ecx",
    "rdmsr",
    "orl $0x900, %eax",
    "wrmsr",
    "movl %cr0, %eax",
    "orl $0x80000001, %eax",
    "movl %eax, %cr0",
    "ljmpl *{far}",
    ".code64",
    "ap_trampoline_long:",
    "movq (ap_trampoline_start + {stack})(%rip), %rsp",
    "movq (ap_trampoline_start + {arg})(%rip), %rdi",
    "movl ${gs_base}, %ecx",
    "movl %edi, %eax",
    "movq %rdi, %rdx",
    "shrq $32, %rdx",
    "wrmsr",
    "movq (ap_trampoline_start + {entry})(%rip), %rax",
    "jmpq *%rax",
    "ap_trampoline_end:",
    ".popsection",
    gdtr = const T_GDTR,
    cr3 = const T_CR3,
    far = const T_FAR,
    stack = const T_STACK,
    arg = const T_ARG,
    entry = const T_ENTRY,
    gs_base = const crate::percpu::IA32_GS_BASE,
    options(att_syntax),
);

unsafe extern "C" {
    static ap_trampoline_start: u8;
    static ap_trampoline_long: u8;
    static ap_trampoline_end: u8;
}

/// The size of an application processor's stack.
const STACK_SIZE: u64 = 64 << 10;

/// How far a starting processor has come.
const WAITING: u8 = 0;
const STARTED: u8 = 1;
const READY: u8 = 2;
const FAILED: u8 = 3;
/// The processor compares its TSC with processor 0's.
const SYNCING: u8 = 4;

/// The rounds in which a starting processor and processor 0 read their
/// TSC in turn, each value checked not to lie before the other's last.
const SYNC_ROUNDS: u64 = 1000;
/// The turn of the comparison: odd for the starting processor, even for
/// processor 0, and the value the last one read.
static SYNC_TURN: AtomicU64 = AtomicU64::new(0);
/// The turn processor 0 sets when it gives up on a comparison, so that
/// the starting processor stops waiting for its next turn.
const GAVE_UP: u64 = u64::MAX;
static SYNC_LAST: AtomicU64 = AtomicU64::new(0);
/// Set when a processor's TSC lay before processor 0's by more than the
/// clock's resolution, or the comparison did not end.
static TSC_DISAGREES: AtomicBool = AtomicBool::new(false);
/// The most cycles a processor's TSC lay from processor 0's after the
/// corrections, among those that disagree.
static TSC_WARP: AtomicU64 = AtomicU64::new(0);
/// In one comparison, the most cycles the starting processor's TSC lay
/// behind processor 0's, as it saw, and ahead of it, as processor 0 saw.
static TSC_BEHIND: AtomicU64 = AtomicU64::new(0);
static TSC_AHEAD: AtomicU64 = AtomicU64::new(0);
/// Processor 0's verdict after a comparison, and the cycles to add to the
/// starting processor's TSC with `CORRECT`.
static VERDICT: AtomicU8 = AtomicU8::new(PENDING);
const PENDING: u8 = 0;
const CORRECT: u8 = 1;
const AGREE: u8 = 2;
const DISAGREE: u8 = 3;
static TSC_CORRECTION: AtomicU64 = AtomicU64::new(0);
/// The most corrections of one processor's TSC.
const TSC_CORRECTIONS: u32 = 3;
const IA32_TSC_ADJUST: u32 = 0x3B;
/// Set once every processor started and the clock source is final; the
/// started processors use the clock only then.
static RELEASED: AtomicBool = AtomicBool::new(false);

/// The stage of the processor being started.
static STAGE: AtomicU8 = AtomicU8::new(WAITING);
/// The MADT, while processors start.
static MADT: AtomicPtr<u8> = AtomicPtr::new(core::ptr::null_mut());
static MADT_LEN: AtomicUsize = AtomicUsize::new(0);
/// The boot processor's optional features and whether it uses x2APIC
/// mode, which every other processor must match.
static BOOT_FEATURES: SpinLock<Option<(Optional, bool)>> =
    SpinLock::new(kernel_core::lock_order::SMP_BOOT_FEATURES, None);

fn cpuid(leaf: u32, subleaf: u32) -> [u32; 4] {
    let r = core::arch::x86_64::__cpuid_count(leaf, subleaf);
    [r.eax, r.ebx, r.ecx, r.edx]
}

/// Frames below 4 GiB for the startup page table, whose top level the
/// processor loads while CR3 is still 32 bits wide.
struct LowTables;

impl Tables for LowTables {
    fn alloc(&mut self) -> Option<u64> {
        memory::alloc_contiguous(1, 1, 1 << 32)
    }

    fn table(&mut self, phys: u64) -> &mut [u64; 512] {
        // SAFETY: the frame came from the allocator for a page table, so
        // nothing else uses it, and the direct map covers it.
        unsafe { &mut *(space::direct(phys) as *mut [u64; 512]) }
    }
}

fn free_frame(phys: u64) {
    if let Some(frames) = memory::FRAMES.lock().as_mut() {
        frames.free(phys, 1);
    }
}

/// Waits until the monotonic clock reaches `until` or `done` holds, and
/// returns whether `done` held.
fn wait_until(until: i64, done: impl Fn() -> bool) -> bool {
    while crate::time::now() < until {
        if done() {
            return true;
        }
        core::hint::spin_loop();
    }
    done()
}

/// Starts the processors that `find` numbered, through the trampoline
/// page in `info`, and leaves those that fail unused. The clock and the
/// boot processor's local APIC must be set up.
pub fn start(info: &BootInfo) {
    let mem = crate::memory::DirectMem(info);
    let tables = kernel_core::acpi::Tables::new(&mem, info.firmware_table_phys)
        .unwrap_or_else(|e| panic!("{e}"));
    let madt = tables
        .find(*b"APIC")
        .unwrap_or_else(|e| panic!("{e}"))
        .unwrap_or_else(|| panic!("the ACPI MADT is missing"));
    let ids = core::mem::take(&mut *APIC_IDS.lock());
    let mut used = Vec::new();
    used.try_reserve(ids.len())
        .unwrap_or_else(|_| panic!("no memory for the processor list"));
    used.push(ids[0]);
    if ids.len() > 1 && info.arch_data0 == 0 {
        log!(
            Warning,
            "no trampoline page: no other processor can be started"
        );
    }
    if ids.len() > 1 && info.arch_data0 != 0 {
        *BOOT_FEATURES.lock() = Some((Optional::read(cpuid), apic::x2apic()));
        MADT_LEN.store(madt.len(), Ordering::Relaxed);
        MADT.store(madt.as_ptr().cast_mut(), Ordering::Release);
        let page = info.arch_data0;
        let mut tables = LowTables;
        let mut mapper = Mapper::user(&mut tables, space::root())
            .unwrap_or_else(|e| panic!("building the startup page table: {e}"));
        let flags = Flags {
            writable: false,
            executable: true,
            global: false,
            user: false,
            policy: Policy::Cached,
        };
        mapper
            .map(page, page, PAGE_SIZE, flags)
            .unwrap_or_else(|e| panic!("building the startup page table: {e}"));
        let root = mapper.root();
        write_trampoline(page, root);
        for &id in &ids[1..] {
            if start_one(page, id, used.len()) {
                used.push(id);
            }
        }
        // No processor is starting any more, so the startup page table
        // and its mapping of the trampoline page go.
        mapper.free_lower_half(free_frame);
        free_frame(root);
        MADT.store(core::ptr::null_mut(), Ordering::Release);
    }
    if used.len() > 1 {
        if TSC_DISAGREES.load(Ordering::Relaxed) {
            let warp = TSC_WARP.load(Ordering::Relaxed);
            log!(
                Info,
                "TSC comparison: up to {warp} cycles apart after corrections"
            );
        }
        match TSC_DISAGREES.load(Ordering::Relaxed) {
            false => log!(Info, "the TSCs of all {} processors agree", used.len()),
            true if crate::time::from_tsc() && crate::time::leave_tsc() => {
                log!(
                    Warning,
                    "the processors' TSCs disagree: monotonic clock from the HPET"
                )
            }
            true if crate::time::from_tsc() => {
                log!(
                    Error,
                    "the processors' TSCs disagree, and there is no HPET to use instead"
                )
            }
            true => log!(Info, "the processors' TSCs disagree"),
        }
    }
    RELEASED.store(true, Ordering::Release);
    log!(Info, "{} processors running", used.len());
    *APIC_IDS.lock() = used;
}

/// Copies the trampoline code into the page at `page`, with its data and
/// `root` as the startup page table.
fn write_trampoline(page: u64, root: u64) {
    let start = (&raw const ap_trampoline_start) as u64;
    let long = (&raw const ap_trampoline_long) as u64;
    let end = (&raw const ap_trampoline_end) as u64;
    let base = space::direct(page) as *mut u8;
    // SAFETY: the code lies in the kernel image between the two symbols,
    // the trampoline page is in the direct map and used for nothing else,
    // and the code ends before the data offsets.
    unsafe {
        core::ptr::copy_nonoverlapping(start as *const u8, base, (end - start) as usize);
        let at = |offset: u64| base.add(offset as usize);
        for (i, d) in T_DESCRIPTORS.iter().enumerate() {
            at(T_GDT + 8 * i as u64).cast::<u64>().write_unaligned(*d);
        }
        at(T_GDTR).cast::<u16>().write_unaligned(23);
        at(T_GDTR + 2)
            .cast::<u32>()
            .write_unaligned((page + T_GDT) as u32);
        at(T_FAR)
            .cast::<u32>()
            .write_unaligned((page + long - start) as u32);
        at(T_FAR + 4).cast::<u16>().write_unaligned(0x08);
        at(T_CR3).cast::<u32>().write_unaligned(root as u32);
        at(T_ENTRY)
            .cast::<u64>()
            .write_unaligned(ap_entry as *const () as u64);
    }
    const _: () = assert!(T_GDT >= 0x100);
}

/// The TSC, read after the loads before it and before the stores after
/// it.
fn ordered_tsc() -> u64 {
    // SAFETY: LFENCE and RDTSC have no preconditions.
    unsafe {
        core::arch::x86_64::_mm_lfence();
        let tsc = core::arch::x86_64::_rdtsc();
        core::arch::x86_64::_mm_lfence();
        tsc
    }
}

/// The clock's resolution, a nanosecond, in TSC ticks.
fn tolerance() -> u64 {
    crate::time::tsc_hz().div_ceil(1_000_000_000).max(1)
}

/// Processor 0's part of the comparison with a starting processor: it
/// reads its TSC and gives the turn over, then checks its next value
/// against the one the other processor read, recording in `TSC_AHEAD` how
/// far the other one's lay ahead. Returns false when the comparison did
/// not end within 100 ms.
fn compare_tsc_boot() -> bool {
    let (tolerance, deadline) = (tolerance(), crate::time::now() + 100_000_000);
    for round in 0..SYNC_ROUNDS {
        SYNC_LAST.store(ordered_tsc(), Ordering::Release);
        SYNC_TURN.store(2 * round + 1, Ordering::Release);
        if !wait_until(deadline, || {
            SYNC_TURN.load(Ordering::Acquire) == 2 * round + 2
        }) {
            SYNC_TURN.store(GAVE_UP, Ordering::Release);
            return false;
        }
        let (mine, theirs) = (ordered_tsc(), SYNC_LAST.load(Ordering::Acquire));
        if mine + tolerance < theirs {
            TSC_AHEAD.fetch_max(theirs - mine, Ordering::Relaxed);
        }
    }
    true
}

/// Compares the TSC of the starting processor `index` with processor 0's
/// and, where they differ and the processor has `TSC_ADJUST`, has it
/// correct its TSC by the difference measured and compares again, at most
/// `TSC_CORRECTIONS` times. The TSCs disagree when they still differ.
fn sync_tsc(index: usize) {
    let mut corrected: i64 = 0;
    let mut attempt = 0;
    // The largest difference of the first comparison, before corrections.
    let mut first = None;
    let (behind, ahead, agree) = loop {
        TSC_BEHIND.store(0, Ordering::Relaxed);
        TSC_AHEAD.store(0, Ordering::Relaxed);
        let completed = compare_tsc_boot();
        let behind = TSC_BEHIND.load(Ordering::Relaxed);
        let ahead = TSC_AHEAD.load(Ordering::Relaxed);
        first.get_or_insert(behind.max(ahead));
        if completed && behind == 0 && ahead == 0 {
            break (behind, ahead, true);
        }
        if !completed || attempt == TSC_CORRECTIONS || !tsc_adjust() {
            break (behind, ahead, false);
        }
        // A processor behind moves forward by what it saw; one ahead,
        // which processor 0 saw, moves back by that.
        let by = if behind >= ahead {
            behind as i64
        } else {
            -(ahead as i64)
        };
        TSC_CORRECTION.store(by as u64, Ordering::Relaxed);
        SYNC_TURN.store(0, Ordering::Release);
        VERDICT.store(CORRECT, Ordering::Release);
        let deadline = crate::time::now() + 100_000_000;
        if !wait_until(deadline, || VERDICT.load(Ordering::Acquire) == PENDING) {
            break (behind, ahead, false);
        }
        corrected += by;
        attempt += 1;
    };
    let worst = behind.max(ahead);
    let first = first.unwrap_or(worst);
    let ns =
        |cycles: u64| u128::from(cycles) * 1_000_000_000 / u128::from(crate::time::tsc_hz().max(1));
    log!(
        Info,
        "processor {index}: TSC up to {first} cycles ({} ns) from processor 0's, TSC_ADJUST {}, corrected by {corrected} cycles: {} ({worst} cycles, {} ns, apart)",
        ns(first),
        if tsc_adjust() { "present" } else { "absent" },
        if agree { "they agree" } else { "they disagree" },
        ns(worst)
    );
    if !agree {
        TSC_DISAGREES.store(true, Ordering::Relaxed);
        TSC_WARP.fetch_max(worst, Ordering::Relaxed);
    }
    VERDICT.store(if agree { AGREE } else { DISAGREE }, Ordering::Release);
}

/// Whether this processor has `IA32_TSC_ADJUST`.
fn tsc_adjust() -> bool {
    core::arch::x86_64::__cpuid_count(7, 0).ebx & (1 << 1) != 0
}

/// The starting processor's part of `sync_tsc`: compares, and corrects
/// its TSC as processor 0 says until it gives its verdict.
fn sync_tsc_secondary() {
    loop {
        compare_tsc_secondary();
        let verdict = loop {
            match VERDICT.load(Ordering::Acquire) {
                PENDING => core::hint::spin_loop(),
                verdict => break verdict,
            }
        };
        if verdict != CORRECT {
            return;
        }
        let by = TSC_CORRECTION.load(Ordering::Relaxed);
        // SAFETY: processor 0 asks for this only of a processor that has
        // TSC_ADJUST; changing it moves this processor's TSC, which
        // nothing uses before the comparison ends.
        unsafe {
            let adjust = crate::cpu::read_msr(IA32_TSC_ADJUST);
            crate::cpu::write_msr(IA32_TSC_ADJUST, adjust.wrapping_add(by));
        }
        VERDICT.store(PENDING, Ordering::Release);
    }
}

/// The starting processor's part of the comparison: once processor 0
/// gives it the turn, it checks its TSC against processor 0's last value
/// and gives the turn back with its own.
fn compare_tsc_secondary() {
    let tolerance = tolerance();
    for round in 0..SYNC_ROUNDS {
        loop {
            match SYNC_TURN.load(Ordering::Acquire) {
                GAVE_UP => return,
                turn if turn == 2 * round + 1 => break,
                _ => core::hint::spin_loop(),
            }
        }
        let theirs = SYNC_LAST.load(Ordering::Acquire);
        let mine = ordered_tsc();
        if mine + tolerance < theirs {
            TSC_BEHIND.fetch_max(theirs - mine, Ordering::Relaxed);
        }
        SYNC_LAST.store(mine, Ordering::Release);
        // Processor 0 may have given up meanwhile; then the turn stays.
        if SYNC_TURN
            .compare_exchange(
                2 * round + 1,
                2 * round + 2,
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .is_err()
        {
            return;
        }
    }
}

/// Starts the processor whose APIC ID is `id` as processor `index`.
/// Returns whether it runs.
fn start_one(page: u64, id: u32, index: usize) -> bool {
    // A comparison that processor 0 gave up on leaves no turn behind, and
    // the last processor's verdict is not this one's.
    SYNC_TURN.store(0, Ordering::Release);
    VERDICT.store(PENDING, Ordering::Release);
    let core = alloc::boxed::Box::leak(
        kernel_core::fallible::try_box(Cpu::new())
            .unwrap_or_else(|_| panic!("no memory for processor {index}")),
    );
    let cpu = percpu::new_secondary(index as u32, id, (&raw const *core).cast())
        .unwrap_or_else(|| panic!("no memory for processor {index}"));
    let top = space::new_stack(STACK_SIZE);
    #[cfg(feature = "diag")]
    crate::panic::others::set_stack(index, top - STACK_SIZE, top);
    SYNC_TURN.store(0, Ordering::Relaxed);
    STAGE.store(WAITING, Ordering::Release);
    let base = space::direct(page) as *mut u8;
    // SAFETY: the data lies in the trampoline page, which no processor
    // runs while `STAGE` is `WAITING`.
    unsafe {
        base.add(T_STACK as usize)
            .cast::<u64>()
            .write_unaligned(top);
        base.add(T_ARG as usize)
            .cast::<u64>()
            .write_unaligned((cpu as *const PerCpu) as u64);
    }
    let started = || STAGE.load(Ordering::Acquire) != WAITING;
    apic::send_ipi(id, apic::ICR_INIT);
    wait_until(crate::time::now() + 10_000_000, || false);
    let first = crate::time::now();
    let vector = (page / PAGE_SIZE) as u32;
    apic::send_ipi(id, apic::ICR_STARTUP | vector);
    if !wait_until(first + 200_000, started) {
        apic::send_ipi(id, apic::ICR_STARTUP | vector);
    }
    if !wait_until(first + 100_000_000, started) {
        log!(Error, "processor with APIC ID {id} did not start");
        return false;
    }
    // Once started, it no longer uses the trampoline page.
    loop {
        match STAGE.load(Ordering::Acquire) {
            SYNCING => {
                sync_tsc(index);
                // The processor goes on to READY.
                while STAGE.load(Ordering::Acquire) == SYNCING {
                    core::hint::spin_loop();
                }
            }
            READY => {
                kernel_core::cpu::register(index, core);
                return true;
            }
            FAILED => {
                log!(
                    Error,
                    "processor with APIC ID {id} stopped: its features differ from processor 0's"
                );
                return false;
            }
            _ => core::hint::spin_loop(),
        }
    }
}

/// Where an application processor continues from the trampoline, on its
/// stack, with `cpu` as its data.
extern "sysv64" fn ap_entry(cpu: &'static PerCpu) -> ! {
    // SAFETY: the kernel's tables map the kernel image and this
    // processor's stack as the startup page table does.
    unsafe { crate::cpu::write_cr3(space::root()) };
    // SAFETY: `cpu` came from `new_secondary` for this processor only.
    unsafe { percpu::init_secondary(cpu) };
    STAGE.store(STARTED, Ordering::Release);
    let matches = Leaves::read(cpuid).missing().next().is_none()
        && *BOOT_FEATURES.lock() == Some((Optional::read(cpuid), apic::has_x2apic()));
    if !matches {
        STAGE.store(FAILED, Ordering::Release);
        crate::cpu::halt();
    }
    space::load_pat();
    crate::cpu::serialize_lfence();
    crate::cpu::init();
    let ist = || space::new_stack(gdt::IST_STACK_SIZE);
    gdt::init(ist(), ist(), ist(), ist());
    crate::idt::load();
    crate::syscall::init();
    // SAFETY: `start` keeps the MADT in place until this processor is
    // ready.
    let madt = unsafe {
        core::slice::from_raw_parts(
            MADT.load(Ordering::Acquire),
            MADT_LEN.load(Ordering::Relaxed),
        )
    };
    apic::init_secondary(madt);
    crate::tlb::join();
    STAGE.store(SYNCING, Ordering::Release);
    sync_tsc_secondary();
    STAGE.store(READY, Ordering::Release);
    // Processor 0 may still move the clock to another source; until it is
    // final, this processor only answers TLB shootdowns.
    while !RELEASED.load(Ordering::Acquire) {
        crate::tlb::service();
        core::hint::spin_loop();
    }
    crate::timer::init();
    kernel_core::boot::start_secondary()
}
