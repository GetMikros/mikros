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

#[test]
fn chooses_how_to_reset_a_function() {
    let mut fake = Fake::default();
    let put = |function: &mut Function, offset: usize, bytes: &[u8]| {
        function.space[offset..offset + bytes.len()].copy_from_slice(bytes);
    };
    let list = |function: &mut Function, caps: &[(u8, usize)]| {
        put(function, usize::from(STATUS), &(1u16 << 4).to_le_bytes());
        put(function, usize::from(CAPABILITIES), &[caps[0].1 as u8]);
        for (i, &(id, offset)) in caps.iter().enumerate() {
            let next = caps.get(i + 1).map_or(0, |c| c.1 as u8);
            put(function, offset, &[id, next]);
        }
    };
    // A PCI Express function level reset comes first.
    let flr = at(0, 1, 0);
    let f = fake.add(flr, &VIRTIO_BLOCK, 0);
    list(f, &[(CAP_PM, 0x40), (CAP_PCIE, 0x60)]);
    put(f, 0x64, &(1u32 << 28).to_le_bytes());
    assert_eq!(reset_method(&fake, flr, 0), Some(Reset::PcieFlr(0x60)));
    // Then the Advanced Features one.
    let af = at(0, 2, 0);
    let f = fake.add(af, &VIRTIO_BLOCK, 0);
    list(f, &[(CAP_PCIE, 0x40), (CAP_AF, 0x50)]);
    put(f, 0x53, &[1 << 1]);
    assert_eq!(reset_method(&fake, af, 0), Some(Reset::AfFlr(0x50)));
    // Then D3hot to D0, unless No_Soft_Reset is set.
    let pm = at(0, 3, 0);
    let f = fake.add(pm, &VIRTIO_BLOCK, 0);
    list(f, &[(CAP_PM, 0x40)]);
    assert_eq!(reset_method(&fake, pm, 0), Some(Reset::PowerCycle(0x40)));
    put(
        fake.functions.get_mut(&pm).unwrap(),
        0x44,
        &(1u32 << 3).to_le_bytes(),
    );
    assert_eq!(reset_method(&fake, pm, 0), None);
    // Then the secondary bus reset of the bridge above, for the only
    // device on that bus, but not for one of two.
    let bridge = at(0, 5, 0);
    let f = fake.add(bridge, &VIRTIO_BLOCK, 1);
    put(f, usize::from(SECONDARY_BUS), &[1]);
    let alone = at(1, 0, 0);
    fake.add(alone, &VIRTIO_BLOCK, 0);
    assert_eq!(
        reset_method(&fake, alone, 0),
        Some(Reset::SecondaryBus(bridge))
    );
    fake.add(at(1, 1, 0), &VIRTIO_BLOCK, 0);
    assert_eq!(reset_method(&fake, alone, 0), None);
    // A function on the root bus has no bridge above it.
    assert_eq!(reset_method(&fake, at(0, 3, 0), 0), None);
}

/// Configuration space that counts the reads asked of it.
struct Counted<'a>(&'a Fake, core::cell::Cell<usize>);

impl Config for Counted<'_> {
    fn read(&self, at: Address, offset: u16, width: u8) -> u32 {
        self.1.set(self.1.get() + 1);
        self.0.read(at, offset, width)
    }

    fn write(&mut self, _: Address, _: u16, _: u8, _: u32) {}
}

#[test]
fn looks_for_the_bridge_above_a_function_only_where_it_can_be() {
    let mut fake = Fake::default();
    // A function on the first bus without a way to reset it: no bridge
    // can be above it, so only its own capabilities are read.
    let root = at(0, 2, 0);
    fake.add(root, &VIRTIO_BLOCK, 0);
    let counted = Counted(&fake, core::cell::Cell::new(0));
    assert_eq!(reset_method(&counted, root, 0), None);
    assert!(counted.1.get() < 8, "{} reads", counted.1.get());
    // A bridge on bus 1 to bus 2, as function 1 of a multi-function
    // device, above the only device of bus 2.
    fake.add(at(1, 4, 0), &VIRTIO_BLOCK, MULTI_FUNCTION);
    let f = fake.add(at(1, 4, 1), &VIRTIO_BLOCK, 1);
    f.space[usize::from(SECONDARY_BUS)] = 2;
    let below = at(2, 0, 0);
    fake.add(below, &VIRTIO_BLOCK, 0);
    let counted = Counted(&fake, core::cell::Cell::new(0));
    assert_eq!(
        reset_method(&counted, below, 0),
        Some(Reset::SecondaryBus(at(1, 4, 1)))
    );
    // The device numbers of buses 0 and 1, and the functions of the two
    // devices found, rather than every function of every bus.
    assert!(counted.1.get() < 128, "{} reads", counted.1.get());
}

fn memory(index: u8, base: u64, size: u64) -> Bar {
    Bar {
        index,
        kind: BarKind::Mmio,
        prefetchable: false,
        base,
        size,
    }
}

fn pages_claim(group: usize, start: u64, end: u64) -> Claim {
    Claim::Pages { group, start, end }
}

#[test]
fn claims_the_pages_that_memory_bars_touch() {
    let io = Bar {
        index: 4,
        kind: BarKind::Io,
        prefetchable: false,
        base: 0xc040,
        size: 0x20,
    };
    // A 2 KiB BAR alone in its page, as an AHCI controller's, gets the
    // page; an I/O BAR gets exactly its ports; a large BAR its pages.
    let ahci = [memory(5, 0xfcd0_0000, 0x800), io];
    let nvme = [memory(0, 0xfcf0_0000, 0x4000)];
    assert_eq!(
        claims(&[&ahci, &nvme]),
        [
            vec![
                pages_claim(0, 0xfcd0_0000, 0xfcd0_1000),
                Claim::Io {
                    base: 0xc040,
                    size: 0x20
                }
            ],
            vec![pages_claim(0, 0xfcf0_0000, 0xfcf0_4000)],
        ]
    );
    // A BAR that is not page-aligned touches the pages around it.
    assert_eq!(pages(&memory(0, 0x1800, 0x1000)), 0x1000..0x3000);
}

#[test]
fn shares_one_resource_between_bars_of_a_function_on_a_page() {
    // Two BARs of one function in one page, a third reaching into the
    // next page from that one, and a fourth elsewhere.
    let bars = [
        memory(0, 0x10_0000, 0x400),
        memory(1, 0x10_0800, 0x100),
        memory(2, 0x10_0c00, 0x800),
        memory(3, 0x20_0000, 0x1000),
    ];
    assert_eq!(
        claims(&[&bars]),
        [vec![
            pages_claim(0, 0x10_0000, 0x10_2000),
            pages_claim(0, 0x10_0000, 0x10_2000),
            pages_claim(0, 0x10_0000, 0x10_2000),
            pages_claim(1, 0x20_0000, 0x20_1000),
        ]]
    );
}

#[test]
fn claims_nothing_for_a_page_that_another_function_touches() {
    // Two functions' small BARs in one page get nothing; their other BARs
    // keep their own pages.
    let a = [memory(0, 0x30_0000, 0x100), memory(1, 0x40_0000, 0x1000)];
    let b = [memory(0, 0x30_0400, 0x100)];
    let c = [memory(2, 0x50_0000, 0x100)];
    assert_eq!(
        claims(&[&a, &b, &c]),
        [
            vec![Claim::Shared, pages_claim(0, 0x40_0000, 0x40_1000)],
            vec![Claim::Shared],
            vec![pages_claim(0, 0x50_0000, 0x50_1000)],
        ]
    );
    // A BAR of a function that shares one of its pages gets nothing even
    // where another BAR of that function goes on to a page of its own.
    let wide = [memory(0, 0x60_0000, 0x100), memory(1, 0x60_0000, 0x2000)];
    let small = [memory(0, 0x60_1800, 0x100)];
    assert_eq!(
        claims(&[&wide, &small]),
        [
            vec![pages_claim(0, 0x60_0000, 0x60_1000), Claim::Shared],
            vec![Claim::Shared],
        ]
    );
}

#[test]
fn writes_and_reads_back_the_bar_properties() {
    let mut fake = Fake::default();
    let f = at(0, 3, 0);
    let function = fake.add(f, &VIRTIO_BLOCK, 0);
    function.bar(0, BAR_IO, 0xc040, 0x20, u32::MAX);
    function.bar(1, 0, 0xfcd0_0000, 0x800, u32::MAX);
    function.bar(
        2,
        BAR_64 | BAR_PREFETCHABLE,
        0x8_0000_0000,
        0x4000,
        u32::MAX,
    );
    function.space[usize::from(COMMAND)] = (COMMAND_IO | COMMAND_MEMORY) as u8;
    let sized = size_bars(&mut fake, f);
    let properties = bar_properties(&sized);
    assert_eq!(
        properties,
        [
            ("bar0".into(), "0x20".into()),
            ("bar1".into(), "0x800".into()),
            ("bar2".into(), "0x4000".into()),
        ]
    );
    // Taking them back reads the registers only, which `kept_bars`'s
    // shared reference to the configuration space ensures.
    assert_eq!(kept_bars(&fake, f, &properties), Some(sized));
    // Other properties are ignored.
    let mut more = properties.clone();
    more.push(("cache".into(), "write-back".into()));
    assert!(kept_bars(&fake, f, &more).is_some());
}

#[test]
fn sizes_again_unless_the_bar_properties_fit() {
    let mut fake = Fake::default();
    let f = at(0, 4, 0);
    let function = fake.add(f, &VIRTIO_BLOCK, 0);
    function.bar(0, 0, 0xfe00_0000, 0x1000, u32::MAX);
    function.bar(2, BAR_64, 0x8_0000_0000, 0x4000, u32::MAX);
    let good = |pairs: &[(&str, &str)]| -> Vec<(String, String)> {
        pairs.iter().map(|&(k, v)| (k.into(), v.into())).collect()
    };
    assert!(kept_bars(&fake, f, &good(&[("bar0", "0x1000"), ("bar2", "0x4000")])).is_some());
    for properties in [
        // A BAR without its property, or a property without its BAR.
        good(&[("bar0", "0x1000")]),
        good(&[("bar0", "0x1000"), ("bar2", "0x4000"), ("bar4", "0x10")]),
        // The upper half of a 64-bit pair.
        good(&[("bar0", "0x1000"), ("bar2", "0x4000"), ("bar3", "0x10")]),
        // Not a size: not hexadecimal, uppercase, no prefix, not a power of
        // two, smaller than a memory BAR can be, or larger than the base's
        // alignment.
        good(&[("bar0", "0x1000"), ("bar2", "4000")]),
        good(&[("bar0", "0x1000"), ("bar2", "0x4G00")]),
        good(&[("bar0", "0X1000"), ("bar2", "0x4000")]),
        good(&[("bar0", "0x1000"), ("bar2", "0xA000")]),
        good(&[("bar0", "0x1000"), ("bar2", "0x3000")]),
        good(&[("bar0", "0x8"), ("bar2", "0x4000")]),
        good(&[("bar0", "0x4000000"), ("bar2", "0x4000")]),
    ] {
        assert_eq!(kept_bars(&fake, f, &properties), None, "{properties:?}");
    }
}

#[test]
fn only_a_display_function_that_decodes_memory_is_left_unsized() {
    let mut fake = Fake::default();
    let display = Header {
        class: CLASS_DISPLAY,
        subclass: 0x00,
        ..VIRTIO_BLOCK
    };
    let (shown, idle, block) = (at(0, 2, 0), at(0, 3, 0), at(0, 4, 0));
    for (f, header) in [(shown, &display), (idle, &display), (block, &VIRTIO_BLOCK)] {
        let function = fake.add(f, header, 0);
        function.bar(0, 0, 0xe000_0000, 0x100_0000, u32::MAX);
    }
    let decode = |fake: &mut Fake, f: Address| {
        let function = fake.functions.get_mut(&f).unwrap();
        function.space[usize::from(COMMAND)] = COMMAND_MEMORY as u8;
    };
    decode(&mut fake, shown);
    decode(&mut fake, block);
    // A shared reference to the configuration space: reads only.
    assert!(decoding_display(&fake, shown));
    assert!(!decoding_display(&fake, idle));
    assert!(!decoding_display(&fake, block));
}
