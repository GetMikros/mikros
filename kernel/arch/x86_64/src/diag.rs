//! The diagnostic build's records of the x86_64 kernel's state, after
//! those of `kernel_core::diag`:
//!
//! - `K build_id`
//! - `H block_size in_use`, for each heap size class, and `H pages count`
//!   for whole-page blocks
//! - `F total free pinned`, in frames, pinned for DMA
//! - `D used_bytes size_bytes`, of the kernel's dynamic region
//! - `S cpu pending`, whether a shootdown waits on the processor
//! - `I device domain pages`, for each IOMMU domain

use core::fmt::{self, Write};
use core::sync::atomic::{AtomicU64, Ordering};

/// The frames pinned for DMA.
pub static PINNED: AtomicU64 = AtomicU64::new(0);

/// Writes the records. Without `wait`, as in a panic report, those that
/// need a lock that is held are left out.
pub fn records(out: &mut dyn Write, wait: bool) -> fmt::Result {
    out.write_str("K\t")?;
    for b in crate::panic::build_id() {
        write!(out, "{b:02x}")?;
    }
    out.write_char('\n')?;
    if let Some((classes, pages)) = crate::heap::usage(wait) {
        for (size, used) in classes {
            writeln!(out, "H\t{size}\t{used}")?;
        }
        writeln!(out, "H\tpages\t{pages}")?;
    }
    let frames = match wait {
        true => Some(crate::memory::FRAMES.lock()),
        false => crate::memory::FRAMES.try_lock(),
    };
    if let Some(Some(frames)) = frames.as_deref() {
        let pinned = PINNED.load(Ordering::Relaxed);
        writeln!(
            out,
            "F\t{}\t{}\t{pinned}",
            frames.total_count(),
            frames.free_count()
        )?;
    }
    drop(frames);
    if let Some((used, size)) = crate::space::dynamic_use(wait) {
        writeln!(out, "D\t{used}\t{size}")?;
    }
    for cpu in crate::tlb::RUNNING.iter() {
        if let Some(data) = crate::percpu::of(cpu) {
            let pending = data.shootdown_pending.load(Ordering::Relaxed);
            writeln!(out, "S\t{cpu}\t{}", u8::from(pending))?;
        }
    }
    crate::iommu::domains(wait, &mut |device, domain, pages| {
        writeln!(out, "I\t{device:#06x}\t{domain}\t{pages}")
    })
}
