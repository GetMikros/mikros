//! The kernel's lock classes, in the one order in which they are taken:
//! a thread that holds a lock of one class takes only locks of classes
//! listed after it. The fuzz build checks the order as locks are taken.

/// A lock class; classes compare in the order they are listed.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Class(pub u16);

impl Class {
    /// The class's name, for reports.
    pub fn name(self) -> &'static str {
        NAMES.get(usize::from(self.0)).copied().unwrap_or("?")
    }
}

macro_rules! classes {
    ($($name:ident),* $(,)?) => {
        /// The names of the classes, in order.
        pub const NAMES: &[&str] = &[$(stringify!($name)),*];
        classes!(@ 0u16; $($name),*);
    };
    (@ $n:expr; $name:ident $(, $rest:ident)*) => {
        pub const $name: Class = Class($n);
        classes!(@ $n + 1; $($rest),*);
    };
    (@ $n:expr;) => {};
}

classes! {
    PROCESS_REGISTRY,
    PROCESS_INIT,
    RESOURCE_LIVE,
    PROCESS_THREADS,
    PROCESS_HANDLES,
    PROCESS_EXCEPTION,
    PROCESS_STOPPED,
    PROCESS_SIGNALS,
    THREAD_SIGNALS,
    TIMER,
    PAGER_OBJECTS,
    MEMORY_PAGES,
    MEMORY_PAGING,
    MEMORY_CHARGES,
    MEMORY_WRITEBACK,
    MEMORY_CHANGING,
    ASPACE_REGIONS,
    MEMORY_MAPPINGS,
    ZERO_FRAME,
    CHANNEL_SHARED,
    CHANNEL_TRANSIT,
    CHANNEL_DYING,
    CHANNEL_NEXT,
    CHANNEL_HOLDER,
    CHANNEL_OUTER,
    CHANNEL_OBSERVER,
    STREAM_SHARED,
    PORT_REGISTRATIONS,
    PORT_STATE,
    PAGER_WAITERS,
    WAIT_WAITERS,
    WAIT_CALLERS,
    WAIT_FUTEXES,
    TIMELINE_WAITERS,
    LOG_OBJECT,
    LOG_READERS,
    FUZZ_STOP,
    BTI_QUARANTINE,
    BTI_PIN,
    IOMMU,
    IOMMU_LOGGED,
    INTERRUPT_SLOTS,
    IO_APICS,
    RESERVED_RANGES,
    THREAD_COVERAGE,
    SCHED,
    THREAD_USER,
    THREAD_DEBUG,
    THREAD_ENTITY,
    PROCESS_IOPORTS,
    OPEN_PORTS,
    ASPACE_INNER,
    BUDGET_ACCOUNTS,
    RNG,
    NODES_RESERVE,
    HEAP,
    TLB_SENDER,
    SPACE_DYNAMIC,
    FRAMES,
    PCIDS,
    SMP_APIC_IDS,
    SMP_BOOT_FEATURES,
    SPACE_BOOT_INFO,
    SPACE_CMDLINE,
    MOCK_OUTPUT,
    TEST,
}

/// What taking a lock checks against the locks held.
#[cfg(lock_check)]
pub(crate) enum Check {
    /// None of a class listed at or after the lock's is held.
    Order,
    /// None of a class listed after the lock's is held.
    Nested,
    /// Nothing, for a lock taken without waiting.
    None,
}

/// Records that this processor takes a lock of `class`, after making the
/// `check`.
#[cfg(lock_check)]
pub(crate) fn acquire(class: Class, check: Check) {
    use core::sync::atomic::Ordering;
    let cpu = crate::cpu::this();
    let count = cpu.held_count.load(Ordering::Relaxed);
    for slot in cpu.held.iter().take(count) {
        let held = Class(slot.load(Ordering::Relaxed));
        let against = match check {
            Check::Order => held >= class,
            Check::Nested => held > class,
            Check::None => false,
        };
        if against {
            violation(held, class);
        }
    }
    if let Some(slot) = cpu.held.get(count) {
        slot.store(class.0, Ordering::Relaxed);
    }
    cpu.held_count.store(count + 1, Ordering::Relaxed);
}

/// Records that this processor releases a lock of `class`.
#[cfg(lock_check)]
pub(crate) fn release(class: Class) {
    use core::sync::atomic::Ordering;
    let cpu = crate::cpu::this();
    let count = cpu.held_count.load(Ordering::Relaxed);
    let tracked = count.min(cpu.held.len());
    if let Some(i) = (0..tracked)
        .rev()
        .find(|&i| cpu.held[i].load(Ordering::Relaxed) == class.0)
    {
        for j in i..tracked - 1 {
            let next = cpu.held[j + 1].load(Ordering::Relaxed);
            cpu.held[j].store(next, Ordering::Relaxed);
        }
    }
    cpu.held_count
        .store(count.saturating_sub(1), Ordering::Relaxed);
}

/// Stops the kernel on taking `taking` while `held` is held, against the
/// order.
#[cfg(all(lock_check, not(lock_discover)))]
fn violation(held: Class, taking: Class) {
    panic!(
        "lock order: taking {} while holding {}",
        taking.name(),
        held.name()
    );
}

/// Lists taking `taking` while `held` is held, once for each pair.
#[cfg(lock_discover)]
fn violation(held: Class, taking: Class) {
    use core::sync::atomic::{AtomicU64, Ordering};
    static SEEN: [AtomicU64; 128] = [const { AtomicU64::new(0) }; 128];
    let pair = usize::from(held.0) * NAMES.len() + usize::from(taking.0);
    let bit = 1 << (pair % 64);
    if SEEN[pair / 64 % 128].fetch_or(bit, Ordering::Relaxed) & bit != 0 {
        return;
    }
    crate::arch::debug_write(b"LOCKORDER ");
    crate::arch::debug_write(held.name().as_bytes());
    crate::arch::debug_write(b" ");
    crate::arch::debug_write(taking.name().as_bytes());
    crate::arch::debug_write(b"\r\n");
}
