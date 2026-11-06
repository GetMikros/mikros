//! Choosing where a position-independent kernel image and the direct map
//! go: at random, from random words the firmware or the processor gave,
//! unless the command line has `kaslr=off`.

use boot_protocol::x86_64::{DIRECT_MAP_BASE, KERNEL_RANGE_START};

/// The alignment of a randomized `kernel_virt_base`.
pub const KERNEL_ALIGN: u64 = 2 << 20;
/// The alignment of a randomized `direct_map_base`.
pub const DIRECT_MAP_ALIGN: u64 = 1 << 30;
/// A randomized direct map ends at or below this address.
pub const DIRECT_MAP_END: u64 = 0xFFFF_C000_0000_0000;
/// The random words that placement takes at most, as many as it may
/// reject.
pub const WORDS: usize = 8;

/// The command line word that keeps the non-randomized placement.
pub const KASLR_OFF: &[u8] = b"kaslr=off";

/// Whether `cmdline` has the word `kaslr=off`.
pub fn kaslr_off(cmdline: &[u8]) -> bool {
    cmdline.split(|&b| b == b' ').any(|w| w == KASLR_OFF)
}

/// An index below `count`, chosen uniformly from the first of `words`
/// that does not favour any index, or `None` when every word does.
fn uniform(count: u64, words: &mut impl Iterator<Item = u64>) -> Option<u64> {
    // Every index has as many words below `limit` that give it.
    let limit = u64::MAX / count * count;
    words.find(|&w| w < limit).map(|w| w % count)
}

/// A `KERNEL_ALIGN`-aligned address at which an image of `size` bytes
/// lies wholly in the kernel image range, which runs to the top of the
/// address space, chosen uniformly from `words`.
pub fn kernel_base(size: u64, words: &mut impl Iterator<Item = u64>) -> Option<u64> {
    let room = 0u64.wrapping_sub(KERNEL_RANGE_START);
    let slots = room.checked_sub(size)? / KERNEL_ALIGN + 1;
    uniform(slots, words).map(|i| KERNEL_RANGE_START + i * KERNEL_ALIGN)
}

/// A `DIRECT_MAP_ALIGN`-aligned address of at least `DIRECT_MAP_BASE` at
/// which the direct map of physical addresses below `end` ends at or
/// below `DIRECT_MAP_END`, chosen uniformly from `words`.
pub fn direct_map_base(end: u64, words: &mut impl Iterator<Item = u64>) -> Option<u64> {
    let room = DIRECT_MAP_END - DIRECT_MAP_BASE;
    let slots = room.checked_sub(end)? / DIRECT_MAP_ALIGN + 1;
    uniform(slots, words).map(|i| DIRECT_MAP_BASE + i * DIRECT_MAP_ALIGN)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_the_off_switch_as_a_whole_word() {
        assert!(kaslr_off(b"root=bootfs kaslr=off"));
        assert!(kaslr_off(b"kaslr=off"));
        assert!(!kaslr_off(b"kaslr=offline"));
        assert!(!kaslr_off(b"root=bootfs"));
    }

    #[test]
    fn places_the_kernel_in_its_range() {
        let size = 0x40_0000;
        // 2 GiB, less 4 MiB, in 2 MiB steps, and the base itself.
        let slots = (0x8000_0000 - size) / KERNEL_ALIGN + 1;
        let lowest = kernel_base(size, &mut [0].into_iter());
        assert_eq!(lowest, Some(KERNEL_RANGE_START));
        let highest = kernel_base(size, &mut [slots - 1].into_iter()).unwrap();
        assert_eq!(highest, 0u64.wrapping_sub(size));
        let wrapped = kernel_base(size, &mut [slots].into_iter());
        assert_eq!(wrapped, Some(KERNEL_RANGE_START));
        assert_eq!(kernel_base(0x8000_0001, &mut [0].into_iter()), None);
    }

    #[test]
    fn places_the_direct_map_below_its_end() {
        let end = 3 << 29; // 1.5 GiB
        let slots = (DIRECT_MAP_END - DIRECT_MAP_BASE - end) / DIRECT_MAP_ALIGN + 1;
        let highest = direct_map_base(end, &mut [slots - 1].into_iter()).unwrap();
        assert!(highest + end <= DIRECT_MAP_END);
        assert!(highest + DIRECT_MAP_ALIGN + end > DIRECT_MAP_END);
        assert_eq!(highest % DIRECT_MAP_ALIGN, 0);
        let lowest = direct_map_base(end, &mut [0].into_iter());
        assert_eq!(lowest, Some(DIRECT_MAP_BASE));
    }

    #[test]
    fn skips_words_that_would_favour_an_index() {
        // With 3 slots, the last word of the 2^64 is the one rejected.
        let mut words = [u64::MAX, 4].into_iter();
        assert_eq!(uniform(3, &mut words), Some(1));
        assert_eq!(uniform(3, &mut [u64::MAX].into_iter()), None);
    }
}
