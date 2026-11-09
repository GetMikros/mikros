//! Numbering the processors the kernel uses.

use alloc::vec::Vec;

/// The most processors the kernel uses.
pub const MAX_CPUS: usize = 256;

/// The processors the kernel uses, by their architecture's IDs, and those
/// it leaves unused because there are too many.
#[derive(Debug, PartialEq, Eq)]
pub struct Processors {
    /// The ID of processor `n` at index `n`. Processor 0 is the boot
    /// processor.
    pub used: Vec<u32>,
    pub beyond: Vec<u32>,
}

/// Numbers the processors `found`, with `boot`, the boot processor, as
/// processor 0 and the others in the order found, up to `max` in all.
/// Returns `None` when memory runs out.
pub fn number(boot: u32, found: impl Iterator<Item = u32>, max: usize) -> Option<Processors> {
    let mut used = Vec::new();
    let mut beyond = Vec::new();
    used.try_reserve(1).ok()?;
    used.push(boot);
    for id in found.filter(|&id| id != boot) {
        let list = if used.len() < max {
            &mut used
        } else {
            &mut beyond
        };
        list.try_reserve(1).ok()?;
        list.push(id);
    }
    Some(Processors { used, beyond })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numbers_the_boot_processor_first() {
        let p = number(5, [1, 5, 2, 3].into_iter(), 3).expect("memory");
        assert_eq!(p.used, [5, 1, 2]);
        assert_eq!(p.beyond, [3]);
        let p = number(7, [1].into_iter(), MAX_CPUS).expect("memory");
        assert_eq!(p.used, [7, 1]);
        assert!(p.beyond.is_empty());
    }
}
