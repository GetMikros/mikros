extern crate std;

use super::*;
use alloc::collections::BTreeMap;
use alloc::vec;

/// How a BAR register answers writes: the bits it keeps, and the bits
/// it always reads as.
#[derive(Clone, Copy, Default)]
struct Register {
    writable: u32,
    fixed: u32,
}

/// A function in memory: its configuration space, and how each BAR
/// register answers writes.
struct Function {
    space: [u8; 4096],
    bars: [Register; 6],
}

/// Configuration space in memory, which notes a BAR written while the
/// function decodes.
#[derive(Default)]
struct Fake {
    functions: BTreeMap<Address, Function>,
    written_while_decoding: bool,
}

const fn at(bus: u8, device: u8, function: u8) -> Address {
    Address {
        bus,
        device,
        function,
    }
}

impl Fake {
    fn add(&mut self, at: Address, header: &Header, header_type: u8) -> &mut Function {
        let mut function = Function {
            space: [0; 4096],
            bars: [Register::default(); 6],
        };
        let mut put = |offset: u16, bytes: &[u8]| {
            let offset = usize::from(offset);
            function.space[offset..offset + bytes.len()].copy_from_slice(bytes);
        };
        put(VENDOR_ID, &header.vendor_id.to_le_bytes());
        put(DEVICE_ID, &header.device_id.to_le_bytes());
        put(
            SUBSYSTEM_VENDOR_ID,
            &header.subsystem_vendor_id.to_le_bytes(),
        );
        put(SUBSYSTEM_ID, &header.subsystem_id.to_le_bytes());
        put(REVISION, &[header.revision]);
        put(PROG_IF, &[header.prog_if]);
        put(SUBCLASS, &[header.subclass]);
        put(CLASS, &[header.class]);
        put(HEADER_TYPE, &[header_type]);
        put(INTERRUPT_PIN, &[header.interrupt_pin]);
        self.functions.entry(at).or_insert(function)
    }
}

impl Function {
    /// Makes BAR register `index` decode `size` bytes from `base` as
    /// `flags` says, taking the next register too for a 64-bit BAR.
    fn bar(&mut self, index: usize, flags: u32, base: u64, size: u64, decoded_bits: u32) {
        let low_mask = !((size - 1) as u32) & decoded_bits;
        let low_flags = if flags & BAR_IO != 0 {
            BAR_IO_FLAGS
        } else {
            BAR_MEMORY_FLAGS
        };
        self.bars[index] = Register {
            writable: low_mask & !low_flags,
            fixed: flags,
        };
        self.set(index, (base as u32 & !low_flags) | flags);
        if flags & BAR_TYPE == BAR_64 {
            self.bars[index + 1] = Register {
                writable: !(((size - 1) >> 32) as u32),
                fixed: 0,
            };
            self.set(index + 1, (base >> 32) as u32);
        }
    }

    fn set(&mut self, index: usize, value: u32) {
        let offset = usize::from(BAR0) + 4 * index;
        self.space[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
    }
}

impl Config for Fake {
    fn read(&self, at: Address, offset: u16, width: u8) -> u32 {
        let Some(function) = self.functions.get(&at) else {
            return u32::MAX >> (32 - 8 * u32::from(width));
        };
        let offset = usize::from(offset);
        let mut bytes = [0u8; 4];
        bytes[..usize::from(width)]
            .copy_from_slice(&function.space[offset..offset + usize::from(width)]);
        u32::from_le_bytes(bytes)
    }

    fn write(&mut self, at: Address, offset: u16, width: u8, value: u32) {
        let decoding = self.read(at, COMMAND, 2) as u16 & (COMMAND_IO | COMMAND_MEMORY) != 0;
        let Some(function) = self.functions.get_mut(&at) else {
            return;
        };
        let bar = (BAR0..BAR0 + 24).contains(&offset);
        if bar {
            self.written_while_decoding |= decoding;
            let index = usize::from((offset - BAR0) / 4);
            let register = function.bars[index];
            function.set(index, (value & register.writable) | register.fixed);
            return;
        }
        let offset = usize::from(offset);
        let bytes = value.to_le_bytes();
        function.space[offset..offset + usize::from(width)]
            .copy_from_slice(&bytes[..usize::from(width)]);
    }
}

const VIRTIO_BLOCK: Header = Header {
    vendor_id: 0x1af4,
    device_id: 0x1042,
    subsystem_vendor_id: 0x1af4,
    subsystem_id: 0x1100,
    revision: 0x01,
    class: 0x01,
    subclass: 0x00,
    prog_if: 0x00,
    interrupt_pin: 1,
};

#[test]
fn formats_hardware_ids_from_the_most_specific() {
    assert_eq!(
        VIRTIO_BLOCK.hardware_ids(),
        [
            "pci:v1af4d1042sv1af4sd1100r01",
            "pci:v1af4d1042sv1af4sd1100",
            "pci:v1af4d1042",
            "pci:c010000",
            "pci:c0100",
        ]
    );
}

#[test]
fn reads_headers() {
    let mut fake = Fake::default();
    fake.add(at(0, 4, 0), &VIRTIO_BLOCK, 0);
    assert_eq!(Header::read(&fake, at(0, 4, 0)), VIRTIO_BLOCK);
}

#[test]
fn finds_the_functions_with_type_0_headers() {
    let mut fake = Fake::default();
    // A single function, a device with functions 0 and 2, a bridge with
    // a type 1 header, and a function behind a device that is not
    // multi-function, which is not looked for.
    fake.add(at(0, 0, 0), &VIRTIO_BLOCK, 0);
    fake.add(at(0, 3, 0), &VIRTIO_BLOCK, MULTI_FUNCTION);
    fake.add(at(0, 3, 2), &VIRTIO_BLOCK, 0);
    fake.add(at(0, 7, 0), &VIRTIO_BLOCK, 0);
    fake.add(at(0, 7, 1), &VIRTIO_BLOCK, 0);
    fake.add(at(1, 0, 0), &VIRTIO_BLOCK, 1);
    fake.add(at(2, 31, 0), &VIRTIO_BLOCK, 0);
    assert_eq!(
        scan(&fake, 0, 1),
        [at(0, 0, 0), at(0, 3, 0), at(0, 3, 2), at(0, 7, 0)]
    );
    assert_eq!(scan(&fake, 2, 2), [at(2, 31, 0)]);
}

#[test]
fn sizes_bars_with_decoding_off_and_restores_them() {
    let mut fake = Fake::default();
    let f = at(0, 4, 0);
    let function = fake.add(f, &VIRTIO_BLOCK, 0);
    function.space[usize::from(COMMAND)] = 0x07;
    function.bar(0, 0, 0xFEB0_0000, 0x1000, u32::MAX);
    // I/O BARs may decode only 16 bits.
    function.bar(1, BAR_IO, 0xC040, 0x20, 0xFFFF);
    function.bar(
        2,
        BAR_64 | BAR_PREFETCHABLE,
        0x8_0000_0000,
        0x4000,
        u32::MAX,
    );
    function.bar(4, BAR_64, 0x40_0000_0000, 8 << 30, u32::MAX);
    let before: Vec<u32> = (0..6).map(|i| fake.read(f, BAR0 + 4 * i, 4)).collect();
    let bars = size_bars(&mut fake, f);
    let bar = |index, kind, prefetchable, base, size| Bar {
        index,
        kind,
        prefetchable,
        base,
        size,
    };
    assert_eq!(
        bars,
        vec![
            bar(0, BarKind::Mmio, false, 0xFEB0_0000, 0x1000),
            bar(1, BarKind::Io, false, 0xC040, 0x20),
            bar(2, BarKind::Mmio, true, 0x8_0000_0000, 0x4000),
            bar(4, BarKind::Mmio, false, 0x40_0000_0000, 8 << 30),
        ]
    );
    let after: Vec<u32> = (0..6).map(|i| fake.read(f, BAR0 + 4 * i, 4)).collect();
    assert_eq!(before, after);
    assert_eq!(fake.read(f, COMMAND, 2), 0x07);
    assert!(!fake.written_while_decoding);
}

#[test]
fn leaves_out_unimplemented_bars() {
    let mut fake = Fake::default();
    let f = at(0, 1, 0);
    fake.add(f, &VIRTIO_BLOCK, 0)
        .bar(3, 0, 0xFE00_0000, 0x100, u32::MAX);
    let bars = size_bars(&mut fake, f);
    assert_eq!(bars.len(), 1);
    assert_eq!((bars[0].index, bars[0].size), (3, 0x100));
}

#[test]
fn places_buses_devices_and_functions_in_the_ecam_range() {
    assert_eq!(ecam_offset(0, at(0, 0, 0), 0x10), 0x10);
    assert_eq!(
        ecam_offset(0, at(1, 2, 3), 0),
        (1 << 20) | (2 << 15) | (3 << 12)
    );
    assert_eq!(
        ecam_offset(4, at(5, 31, 7), 0xFFC),
        (1 << 20) | (31 << 15) | (7 << 12) | 0xFFC
    );
}

#[test]
fn walks_capabilities_and_finds_the_msi_registers() {
    let mut fake = Fake::default();
    let f = at(0, 2, 0);
    let function = fake.add(f, &VIRTIO_BLOCK, 0);
    let mut put = |offset: usize, bytes: &[u8]| {
        function.space[offset..offset + bytes.len()].copy_from_slice(bytes);
    };
    put(usize::from(STATUS), &(1u16 << 4).to_le_bytes());
    put(usize::from(CAPABILITIES), &[0x40]);
    // Power management, then MSI with 64-bit addresses and masking,
    // then MSI-X, whose next pointer leads back to MSI.
    put(0x40, &[0x01, 0x50]);
    put(0x50, &[CAP_MSI, 0x70]);
    put(0x52, &((1u16 << 7) | (1 << 8)).to_le_bytes());
    put(0x70, &[CAP_MSIX, 0x50]);
    assert_eq!(
        capabilities(&fake, f),
        [(0x01, 0x40), (CAP_MSI, 0x50), (CAP_MSIX, 0x70)]
    );
    assert_eq!(msi_registers(&fake, f), [0x50..0x68, 0x70..0x7C]);
    // Without the status bit there is no list.
    let plain = at(0, 3, 0);
    fake.add(plain, &VIRTIO_BLOCK, 0);
    assert!(capabilities(&fake, plain).is_empty());
}
