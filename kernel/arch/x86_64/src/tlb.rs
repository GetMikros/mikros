//! Invalidating translations on other processors.

use crate::apic;
use crate::paging::PAGE_SIZE;
use crate::percpu;
use core::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use kernel_core::smp::MAX_CPUS;
use kernel_core::sync::SpinLock;

/// The vector of the interrupt that asks a processor to invalidate
/// translations.
pub const SHOOTDOWN_VECTOR: u8 = 242;

/// The space ID of the kernel's own tables.
pub const KERNEL: u64 = 0;

/// The most pages invalidated one by one; more flush everything.
const PAGES_ONE_BY_ONE: u64 = 32;

/// A set of processors, by number.
pub struct CpuMask([AtomicU64; MAX_CPUS / 64]);

impl CpuMask {
    pub const fn new() -> CpuMask {
        CpuMask([const { AtomicU64::new(0) }; MAX_CPUS / 64])
    }

    pub fn insert(&self, cpu: usize) {
        self.0[cpu / 64].fetch_or(1 << (cpu % 64), Ordering::SeqCst);
    }

    pub fn remove(&self, cpu: usize) {
        self.0[cpu / 64].fetch_and(!(1 << (cpu % 64)), Ordering::SeqCst);
    }

    /// The processors in the set.
    pub fn iter(&self) -> impl Iterator<Item = usize> + '_ {
        self.0.iter().enumerate().flat_map(|(w, word)| {
            let bits = word.load(Ordering::SeqCst);
            (0..64)
                .filter(move |b| bits & (1 << b) != 0)
                .map(move |b| w * 64 + b)
        })
    }
}

/// The processors that take shootdowns: every running processor once
/// its local APIC can receive them.
pub static RUNNING: CpuMask = CpuMask::new();

/// The shootdown being sent: the space whose translations go, the range
/// for the kernel's tables, whether the space is being freed, and the
/// number of processors that have not handled it yet.
struct Request {
    space: AtomicU64,
    start: AtomicU64,
    len: AtomicU64,
    free: AtomicBool,
    left: AtomicUsize,
}

static REQUEST: Request = Request {
    space: AtomicU64::new(0),
    start: AtomicU64::new(0),
    len: AtomicU64::new(0),
    free: AtomicBool::new(false),
    left: AtomicUsize::new(0),
};

/// Held while a shootdown is sent, so that one is in flight at a time.
static SENDER: SpinLock<()> = SpinLock::new(kernel_core::lock_order::TLB_SENDER, ());

/// Makes every processor in `targets` other than this one drop the
/// translations of `space`: for the kernel's tables those of the `len`
/// bytes at `start`, for a user space all of it, which it switches away
/// from when `free` is set. Returns once they all have. The changes to
/// the tables must be made first.
pub fn shootdown(space: u64, start: u64, len: u64, free: bool, targets: &CpuMask) {
    let me = percpu::index() as usize;
    if targets.iter().all(|cpu| cpu == me) {
        return;
    }
    let _sender = SENDER.lock();
    REQUEST.space.store(space, Ordering::Relaxed);
    REQUEST.start.store(start, Ordering::Relaxed);
    REQUEST.len.store(len, Ordering::Relaxed);
    REQUEST.free.store(free, Ordering::Relaxed);
    for cpu in targets.iter().filter(|&cpu| cpu != me) {
        let Some(data) = percpu::of(cpu) else {
            continue;
        };
        REQUEST.left.fetch_add(1, Ordering::AcqRel);
        data.shootdown_pending.store(true, Ordering::Release);
        apic::send_ipi(
            data.apic_id.load(Ordering::Relaxed),
            apic::ICR_FIXED | u32::from(SHOOTDOWN_VECTOR),
        );
    }
    while REQUEST.left.load(Ordering::Acquire) != 0 {
        service();
        core::hint::spin_loop();
    }
}

/// Handles a shootdown sent to this processor, if one is pending. Called
/// from its interrupt and while the processor spins, so that a processor
/// waiting for a lock that the sender holds still handles it.
pub fn service() {
    if !percpu::READY.load(Ordering::Acquire) {
        return;
    }
    let cpu = percpu::this();
    if !cpu.shootdown_pending.swap(false, Ordering::Acquire) {
        return;
    }
    let space = REQUEST.space.load(Ordering::Relaxed);
    if space == KERNEL {
        let start = REQUEST.start.load(Ordering::Relaxed);
        let len = REQUEST.len.load(Ordering::Relaxed);
        if len / PAGE_SIZE <= PAGES_ONE_BY_ONE {
            for page in (start..start + len).step_by(PAGE_SIZE as usize) {
                crate::cpu::invlpg(page);
            }
        } else {
            crate::cpu::flush_all();
        }
    } else {
        crate::page_tables::serve_shootdown(space, REQUEST.free.load(Ordering::Relaxed));
    }
    REQUEST.left.fetch_sub(1, Ordering::Release);
}

/// Makes this processor take shootdowns from now on, and drops every
/// translation it may have cached before.
pub fn join() {
    RUNNING.insert(percpu::index() as usize);
    crate::cpu::flush_all();
}
