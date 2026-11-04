//! The core's data for each processor, reached through the architecture
//! interface's per-processor pointer.

use crate::arch;
use crate::sched::CpuSched;
use crate::sync::SpinLock;
use core::sync::atomic::{AtomicBool, AtomicUsize};
#[cfg(feature = "fuzz")]
use core::sync::atomic::{AtomicPtr, AtomicU64};

/// The core's state of one processor.
pub struct Cpu {
    /// The number of spin locks held on the processor. A processor runs
    /// one thread at a time and switches threads only where no lock is
    /// held, so this counts the locks of the running thread.
    pub(crate) locks_held: AtomicUsize,
    /// The objects destroyed on the processor since its last reschedule
    /// check.
    pub(crate) destroyed: AtomicUsize,
    /// Set when the running thread's slice has ended or a thread was
    /// queued that should run first.
    pub reschedule: AtomicBool,
    /// The scheduler state of the processor.
    pub(crate) sched: SpinLock<CpuSched>,
    /// The number of threads whose queue entry this processor holds room
    /// for: those it owns that have not gone.
    pub(crate) assigned: AtomicUsize,
    /// In the fuzz build, the object ID of the running thread, or 0 while
    /// the processor idles, and its allocation failures, or null.
    #[cfg(feature = "fuzz")]
    pub fuzz_running: AtomicU64,
    #[cfg(feature = "fuzz")]
    pub fuzz_alloc_failures: AtomicPtr<()>,
}

impl Cpu {
    pub const fn new() -> Cpu {
        Cpu {
            locks_held: AtomicUsize::new(0),
            destroyed: AtomicUsize::new(0),
            reschedule: AtomicBool::new(false),
            sched: SpinLock::new(CpuSched::new()),
            assigned: AtomicUsize::new(0),
            #[cfg(feature = "fuzz")]
            fuzz_running: AtomicU64::new(0),
            #[cfg(feature = "fuzz")]
            fuzz_alloc_failures: AtomicPtr::new(core::ptr::null_mut()),
        }
    }
}

impl Default for Cpu {
    fn default() -> Cpu {
        Cpu::new()
    }
}

/// The boot processor's data, which is also used until a processor's
/// data is set.
static BOOT: Cpu = Cpu::new();

/// The data of each processor the kernel uses, by number.
static ALL: [core::sync::atomic::AtomicPtr<Cpu>; crate::smp::MAX_CPUS] =
    [const { core::sync::atomic::AtomicPtr::new(core::ptr::null_mut()) }; crate::smp::MAX_CPUS];

/// The number of processors registered.
static COUNT: AtomicUsize = AtomicUsize::new(1);

/// The data of processor `index`, once it runs.
pub fn of(index: usize) -> Option<&'static Cpu> {
    if index == 0 {
        return Some(&BOOT);
    }
    let data = ALL.get(index)?.load(core::sync::atomic::Ordering::Acquire);
    // SAFETY: only `register` stores pointers, to data that lives as long
    // as the kernel runs.
    (!data.is_null()).then(|| unsafe { &*data })
}

/// The number of processors the kernel uses.
pub fn count() -> usize {
    COUNT.load(core::sync::atomic::Ordering::Acquire)
}

/// Records `cpu` as the data of processor `index`, the next one to run,
/// once the architecture has started it.
pub fn register(index: usize, cpu: &'static Cpu) {
    ALL[index].store(
        (cpu as *const Cpu).cast_mut(),
        core::sync::atomic::Ordering::Release,
    );
    COUNT.fetch_max(index + 1, core::sync::atomic::Ordering::AcqRel);
}

/// This processor's data. Before the architecture layer has set it, the
/// boot processor's.
pub fn this() -> &'static Cpu {
    let data = arch::cpu_data();
    if data.is_null() {
        return &BOOT;
    }
    // SAFETY: only `start_boot` and `start` set the pointer, to a `Cpu`
    // that lives as long as the processor runs.
    unsafe { &*data.cast::<Cpu>() }
}

/// Makes the boot processor's data this processor's. Called once on the
/// boot processor, as soon as the architecture layer can hold the pointer.
pub fn start_boot() {
    // SAFETY: `BOOT` is static and is only read as a `Cpu`.
    unsafe { arch::set_cpu_data((&raw const BOOT).cast()) };
}

/// Makes `cpu` this processor's data.
///
/// # Safety
///
/// No spin lock may be held on this processor, and `cpu` must not be
/// another processor's data.
pub unsafe fn start(cpu: &'static Cpu) {
    // SAFETY: `cpu` is static and is only read as a `Cpu`.
    unsafe { arch::set_cpu_data((cpu as *const Cpu).cast()) };
}
