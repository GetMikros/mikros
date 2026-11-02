//! Placing the kernel's segments in its physically contiguous range.

use crate::elf::Kernel;
use boot_protocol::PAGE_SIZE;

/// The kernel image range is placed at a physical address aligned to this.
pub const PHYS_ALIGN: u64 = 2 << 20;

/// The virtual range of the loaded kernel image.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Layout {
    /// The lowest segment address, rounded down to a page.
    pub virt_base: u64,
    /// The size of the range from `virt_base` to the end of the highest
    /// segment, rounded up to a page.
    pub size: u64,
}

pub fn layout(kernel: &Kernel) -> Layout {
    let start = kernel
        .segments
        .iter()
        .map(|s| s.pages().0)
        .min()
        .unwrap_or(0);
    let end = kernel
        .segments
        .iter()
        .map(|s| s.pages().1)
        .max()
        .unwrap_or(0);
    Layout {
        virt_base: start as u64,
        size: (end - start) as u64,
    }
}

/// Fills `range`, the kernel image range of `layout`, with the segments'
/// file contents from `file` and zeros everywhere else.
pub fn copy(file: &[u8], kernel: &Kernel, layout: Layout, range: &mut [u8]) {
    assert_eq!(range.len() as u64, layout.size);
    range.fill(0);
    for segment in &kernel.segments {
        let at = (segment.vaddr - layout.virt_base) as usize;
        let from = segment.offset as usize;
        let len = segment.file_size as usize;
        range[at..at + len].copy_from_slice(&file[from..from + len]);
    }
}

/// The number of pages in `size` bytes.
pub fn pages(size: u64) -> u64 {
    size.div_ceil(PAGE_SIZE)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::elf::{PF_W, PF_X, Segment};
    use alloc::vec;

    const START: u64 = boot_protocol::x86_64::KERNEL_RANGE_START;

    fn kernel() -> Kernel {
        Kernel {
            entry: START + 0x1000,
            segments: vec![
                Segment {
                    vaddr: START + 0x1000,
                    mem_size: 0x10,
                    offset: 0,
                    file_size: 4,
                    flags: PF_X,
                },
                Segment {
                    vaddr: START + 0x3008,
                    mem_size: 0x2000,
                    offset: 4,
                    file_size: 2,
                    flags: PF_W,
                },
            ],
            version: (1, 0),
        }
    }

    #[test]
    fn spans_from_the_lowest_to_the_highest_page() {
        let l = layout(&kernel());
        assert_eq!(
            l,
            Layout {
                virt_base: START + 0x1000,
                size: 0x5000
            }
        );
        assert_eq!(pages(l.size), 5);
    }

    #[test]
    fn copies_file_bytes_and_zeroes_the_rest() {
        let file = [1, 2, 3, 4, 5, 6, 7];
        let k = kernel();
        let l = layout(&k);
        let mut range = vec![0xaa; l.size as usize];
        copy(&file, &k, l, &mut range);
        assert_eq!(&range[..4], [1, 2, 3, 4]);
        assert_eq!(&range[0x2008..0x200a], [5, 6]);
        let nonzero = range.iter().filter(|&&b| b != 0).count();
        assert_eq!(nonzero, 6);
    }
}
