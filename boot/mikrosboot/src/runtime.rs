//! Preparing the final firmware memory map for `SetVirtualAddressMap`,
//! which moves the firmware's runtime services into the runtime window.

use crate::memmap::{EFI_MEMORY_RUNTIME, firmware_kind};
use boot_protocol::PAGE_SIZE;
use boot_protocol::x86_64::{RUNTIME_WINDOW_BASE, RUNTIME_WINDOW_SIZE};

/// Byte offsets of the fields of a UEFI memory descriptor.
const TYPE: usize = 0;
const PHYS_START: usize = 8;
const VIRT_START: usize = 16;
const PAGES: usize = 24;
const ATTRIBUTE: usize = 32;
/// The size of a descriptor without padding, which holds those fields.
pub const MIN_DESCRIPTOR_SIZE: usize = 40;

fn u32_at(d: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(d[at..at + 4].try_into().unwrap_or_default())
}

fn u64_at(d: &[u8], at: usize) -> u64 {
    u64::from_le_bytes(d[at..at + 8].try_into().unwrap_or_default())
}

/// Whether the range of descriptor `d` lies below the end of the runtime
/// window, or needs no address in it.
fn fits(d: &[u8]) -> bool {
    let kind = firmware_kind(u32_at(d, TYPE), u64_at(d, ATTRIBUTE));
    let end = u64_at(d, PAGES)
        .checked_mul(PAGE_SIZE)
        .and_then(|size| u64_at(d, PHYS_START).checked_add(size));
    !kind.is_firmware_runtime() || end.is_some_and(|end| end <= RUNTIME_WINDOW_SIZE)
}

/// Rewrites the firmware memory map in `map`, whose descriptors are
/// `descriptor_size` bytes each, into the one `SetVirtualAddressMap` is
/// given: the descriptors with `EFI_MEMORY_RUNTIME`, in their order, at
/// the start of `map`, each with its virtual start at
/// `RUNTIME_WINDOW_BASE` plus its physical start. Gives their number, or
/// `None`, leaving `map` unchanged, when a range of a firmware runtime
/// kind ends past `RUNTIME_WINDOW_SIZE` or the descriptor size is below
/// `MIN_DESCRIPTOR_SIZE`. A partial descriptor at the end is ignored.
pub fn virtual_map(map: &mut [u8], descriptor_size: usize) -> Option<usize> {
    if descriptor_size < MIN_DESCRIPTOR_SIZE {
        return None;
    }
    if !map.chunks_exact(descriptor_size).all(fits) {
        return None;
    }
    let count = map.len() / descriptor_size;
    let mut kept = 0;
    for i in 0..count {
        let at = i * descriptor_size;
        if u64_at(&map[at..], ATTRIBUTE) & EFI_MEMORY_RUNTIME == 0 {
            continue;
        }
        let to = kept * descriptor_size;
        map.copy_within(at..at + descriptor_size, to);
        let virt = RUNTIME_WINDOW_BASE + u64_at(&map[to..], PHYS_START);
        map[to + VIRT_START..to + VIRT_START + 8].copy_from_slice(&virt.to_le_bytes());
        kept += 1;
    }
    Some(kept)
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec::Vec;

    /// A descriptor of 48 bytes, with a virtual start that is not yet set
    /// and padding that must be kept.
    fn descriptor(uefi_type: u32, phys: u64, pages: u64, attribute: u64) -> [u8; 48] {
        let mut d = [0xAA; 48];
        d[TYPE..TYPE + 4].copy_from_slice(&uefi_type.to_le_bytes());
        d[PHYS_START..PHYS_START + 8].copy_from_slice(&phys.to_le_bytes());
        d[VIRT_START..VIRT_START + 8].copy_from_slice(&0u64.to_le_bytes());
        d[PAGES..PAGES + 8].copy_from_slice(&pages.to_le_bytes());
        d[ATTRIBUTE..ATTRIBUTE + 8].copy_from_slice(&attribute.to_le_bytes());
        d
    }

    fn map(descriptors: &[[u8; 48]]) -> Vec<u8> {
        descriptors.concat()
    }

    const RT: u64 = EFI_MEMORY_RUNTIME | 0xF;

    #[test]
    fn keeps_runtime_descriptors_with_window_addresses() {
        let code = descriptor(5, 0x7E00_0000, 16, RT);
        let data = descriptor(6, 0x7F00_0000, 8, RT);
        let flash = descriptor(11, 0xFFC0_0000, 1024, EFI_MEMORY_RUNTIME | 1);
        let nvs = descriptor(10, 0x7F80_0000, 2, RT);
        let mut m = map(&[
            descriptor(7, 0, 0x100, 0xF),
            code,
            descriptor(4, 0x10_0000, 0x10, 0xF),
            data,
            flash,
            descriptor(11, 0xFEC0_0000, 1, 1),
            nvs,
        ]);
        assert_eq!(virtual_map(&mut m, 48), Some(4));
        for (i, (d, phys)) in [
            (code, 0x7E00_0000),
            (data, 0x7F00_0000),
            (flash, 0xFFC0_0000),
            (nvs, 0x7F80_0000),
        ]
        .into_iter()
        .enumerate()
        {
            let got = &m[i * 48..(i + 1) * 48];
            assert_eq!(u64_at(got, VIRT_START), RUNTIME_WINDOW_BASE + phys, "{i}");
            assert_eq!(got[..VIRT_START], d[..VIRT_START], "{i}");
            assert_eq!(got[VIRT_START + 8..], d[VIRT_START + 8..], "{i}");
        }
    }

    #[test]
    fn keeps_no_descriptor_without_runtime_ranges() {
        let mut m = map(&[descriptor(7, 0, 0x100, 0xF)]);
        assert_eq!(virtual_map(&mut m, 48), Some(0));
    }

    #[test]
    fn refuses_runtime_ranges_past_the_window() {
        let end = RUNTIME_WINDOW_SIZE;
        let last = descriptor(6, end - 2 * PAGE_SIZE, 2, RT);
        let mut m = map(&[last]);
        assert_eq!(virtual_map(&mut m, 48), Some(1));
        for past in [
            descriptor(6, end - PAGE_SIZE, 2, RT),
            descriptor(5, end, 1, RT),
            descriptor(11, end, 1, RT),
            descriptor(0, end, 1, RT),
            // A runtime type without the attribute still gets a window
            // address in the kernel's tables.
            descriptor(6, end, 1, 0),
            descriptor(6, u64::MAX - PAGE_SIZE, 4, RT),
        ] {
            let mut m = map(&[last, past]);
            let before = m.clone();
            assert_eq!(virtual_map(&mut m, 48), None);
            assert_eq!(m, before, "left unchanged");
        }
        // Other ranges may lie past it.
        let mut m = map(&[last, descriptor(7, end, 1, 0xF)]);
        assert_eq!(virtual_map(&mut m, 48), Some(1));
    }

    #[test]
    fn takes_the_descriptor_size_from_the_firmware() {
        let mut m: Vec<u8> = [descriptor(7, 0, 1, 0), descriptor(6, 0x1000, 1, RT)]
            .iter()
            .flat_map(|d| d[..40].iter().copied())
            .collect();
        assert_eq!(virtual_map(&mut m, 40), Some(1));
        assert_eq!(u64_at(&m, PHYS_START), 0x1000);
        assert_eq!(u64_at(&m, VIRT_START), RUNTIME_WINDOW_BASE + 0x1000);
        assert_eq!(virtual_map(&mut m, 32), None);
    }
}
