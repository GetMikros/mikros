use super::*;
use crate::elf::Segment;
use alloc::boxed::Box;
use alloc::vec;
use alloc::vec::Vec;
use boot_protocol::x86_64::{DIRECT_MAP_BASE, KERNEL_RANGE_START};

const FIRST_FRAME: u64 = 0x4000_0000;
const USER: u64 = 1 << 2;

struct Pool {
    tables: Vec<Box<[u64; 512]>>,
    limit: usize,
}

impl Pool {
    fn new(limit: usize) -> Pool {
        Pool {
            tables: Vec::new(),
            limit,
        }
    }
}

impl Frames for Pool {
    fn alloc(&mut self) -> Option<u64> {
        if self.tables.len() == self.limit {
            return None;
        }
        self.tables.push(Box::new([0xdead; 512]));
        Some(FIRST_FRAME + (self.tables.len() as u64 - 1) * PAGE_SIZE)
    }

    fn table(&mut self, phys: u64) -> &mut [u64; 512] {
        &mut self.tables[((phys - FIRST_FRAME) / PAGE_SIZE) as usize]
    }
}

/// The physical address, page size and entry bits that map `virt`.
fn translate(pool: &mut Pool, root: u64, virt: u64) -> Option<(u64, u64, u64)> {
    let mut table = root;
    for level in (0..=3).rev() {
        let entry = pool.table(table)[index(virt, level)];
        if entry & PRESENT == 0 {
            return None;
        }
        assert_eq!(entry & USER, 0, "user bit set");
        let page = 1u64 << (12 + 9 * level);
        if level == 0 || entry & HUGE != 0 {
            let phys = (entry & ADDR_MASK & !(page - 1)) + virt % page;
            return Some((phys, page, entry & !ADDR_MASK));
        }
        assert_eq!(entry & NO_EXECUTE, 0, "upper levels must not restrict");
        assert_ne!(entry & WRITABLE, 0, "upper levels must not restrict");
        table = entry & ADDR_MASK;
    }
    unreachable!()
}

fn access(bits: u64) -> Access {
    Access {
        writable: bits & WRITABLE != 0,
        executable: bits & NO_EXECUTE == 0,
    }
}

const RX: Access = Access {
    writable: false,
    executable: true,
};
const R: Access = Access {
    writable: false,
    executable: false,
};

#[test]
fn picks_the_largest_page_alignment_allows() {
    let mut pool = Pool::new(16);
    let mut mapper = Mapper::new(&mut pool, true).unwrap();
    let virt = DIRECT_MAP_BASE + GIB_1 - MIB_2 - PAGE_SIZE;
    let phys = GIB_1 - MIB_2 - PAGE_SIZE;
    mapper
        .map(
            virt,
            phys,
            PAGE_SIZE + MIB_2 + GIB_1 + PAGE_SIZE,
            Access::DATA,
        )
        .unwrap();
    let root = mapper.root();
    let page_at = |pool: &mut Pool, off: u64| translate(pool, root, virt + off).map(|t| t.1);
    assert_eq!(page_at(&mut pool, 0), Some(PAGE_SIZE));
    assert_eq!(page_at(&mut pool, PAGE_SIZE), Some(MIB_2));
    assert_eq!(page_at(&mut pool, PAGE_SIZE + MIB_2), Some(GIB_1));
    assert_eq!(
        page_at(&mut pool, PAGE_SIZE + MIB_2 + GIB_1),
        Some(PAGE_SIZE)
    );
    assert_eq!(page_at(&mut pool, 2 * PAGE_SIZE + MIB_2 + GIB_1), None);
    let (phys_at, _, bits) = translate(&mut pool, root, virt + PAGE_SIZE + 5).unwrap();
    assert_eq!(phys_at, phys + PAGE_SIZE + 5);
    assert_eq!(access(bits), Access::DATA);
}

#[test]
fn avoids_1gib_pages_unless_allowed() {
    let mut pool = Pool::new(16);
    let mut mapper = Mapper::new(&mut pool, false).unwrap();
    mapper.map(DIRECT_MAP_BASE, 0, GIB_1, Access::DATA).unwrap();
    let root = mapper.root();
    assert_eq!(
        translate(&mut pool, root, DIRECT_MAP_BASE).unwrap().1,
        MIB_2
    );
}

#[test]
fn refuses_double_mappings_and_reports_exhaustion() {
    let mut pool = Pool::new(4);
    let mut mapper = Mapper::new(&mut pool, false).unwrap();
    mapper.map(0x20_0000, 0x20_0000, MIB_2, RX).unwrap();
    assert_eq!(
        mapper.map(0x20_1000, 0, PAGE_SIZE, RX),
        Err(Error::AlreadyMapped(0x20_1000))
    );
    assert_eq!(
        mapper.map(0x20_0000, 0, MIB_2, RX),
        Err(Error::AlreadyMapped(0x20_0000))
    );
    assert_eq!(
        mapper.map(1 << 39, 0, PAGE_SIZE, RX),
        Err(Error::OutOfFrames)
    );
}

fn entry(start: u64, pages: u64, kind: Kind) -> MemoryMapEntry {
    MemoryMapEntry {
        phys_start: start,
        page_count: pages,
        kind: kind as u32,
        reserved: 0,
    }
}

#[test]
fn builds_the_entry_tables() {
    let kernel = Kernel {
        entry: KERNEL_RANGE_START + 0x1000,
        segments: vec![
            Segment {
                vaddr: KERNEL_RANGE_START + 0x1000,
                mem_size: 0x1800,
                offset: 0x1000,
                file_size: 0x1800,
                flags: PF_X,
            },
            Segment {
                vaddr: KERNEL_RANGE_START + 0x4000,
                mem_size: 0x1000,
                offset: 0x3000,
                file_size: 0x1000,
                flags: 0,
            },
            Segment {
                vaddr: KERNEL_RANGE_START + 0x6000,
                mem_size: 0x2000,
                offset: 0x4000,
                file_size: 0,
                flags: PF_W,
            },
        ],
        version: (1, 0),
        relocations: Vec::new(),
    };
    let layout = crate::load::layout(&kernel);
    let kernel_phys = 0x20_0000;
    let memory_map = [
        entry(0, 0x9f, Kind::Usable),
        entry(0x10_0000, 0x100, Kind::KernelImage),
        entry(0x20_0000, 0x200, Kind::PageTables),
        entry(0x40_0000, 1, Kind::Mmio),
        entry(0x40_1000, 0x1ff, Kind::AcpiNvs),
        entry(0x60_0000, 0x200, Kind::Framebuffer),
    ];
    let placement = Placement {
        kernel: &kernel,
        layout,
        kernel_phys,
        stack_phys: 0x80_0000,
        memory_map: &memory_map,
        direct_map_base: DIRECT_MAP_BASE,
        switch_code: (0x9000, 0x2000),
    };
    let mut pool = Pool::new(64);
    let root = build(&mut pool, true, &placement).unwrap();
    let runs: Vec<_> = direct_runs(&memory_map).collect();
    assert_eq!(
        runs,
        [
            (0, 0x9f_000),
            (0x10_0000, 0x40_0000),
            (0x40_1000, 0x60_0000)
        ]
    );
    let bound = table_bound(&kernel, 0x2000, runs);
    assert!(
        pool.tables.len() as u64 <= bound,
        "{} > {bound}",
        pool.tables.len()
    );
    let mut at = |virt| translate(&mut pool, root, virt);

    // Each segment with its own rights, nothing in the gaps.
    let base = layout.virt_base;
    assert_eq!(at(base), Some((kernel_phys, PAGE_SIZE, RX.bits())));
    assert_eq!(at(base + 0x1fff).unwrap().0, kernel_phys + 0x1fff);
    assert_eq!(at(base + 0x2000), None);
    assert_eq!(at(base + 0x3000).map(|t| access(t.2)), Some(R));
    assert_eq!(at(base + 0x4000), None);
    let data = at(base + 0x6000).unwrap();
    assert_eq!(
        (data.0, access(data.2)),
        (kernel_phys + 0x6000, Access::DATA)
    );

    // The stack below the kernel, with an unmapped page below it.
    let stack = stack_virt(layout);
    assert_eq!(stack >> 39 & 511, 511);
    assert_eq!(at(stack), Some((0x80_0000, PAGE_SIZE, Access::DATA.bits())));
    assert_eq!(at(base - 1).unwrap().0, 0x80_0000 + BOOT_STACK_SIZE - 1);
    assert_eq!(at(stack - 1), None);

    // The switch code at its physical address.
    assert_eq!(at(0x9000), Some((0x9000, PAGE_SIZE, RX.bits())));
    assert_eq!(at(0xb000), None);

    // The direct map, without MMIO and the framebuffer, and large pages
    // across range boundaries.
    let mut direct = |phys| at(DIRECT_MAP_BASE + phys).map(|t| (t.0, t.1));
    assert_eq!(direct(0x9e_fff), Some((0x9e_fff, PAGE_SIZE)));
    assert_eq!(direct(0x9f_000), None);
    assert_eq!(direct(0x10_0000).unwrap().1, PAGE_SIZE);
    assert_eq!(direct(0x20_0000), Some((0x20_0000, MIB_2)));
    assert_eq!(direct(0x40_0000), None);
    assert_eq!(direct(0x40_1000).unwrap().1, PAGE_SIZE);
    assert_eq!(direct(0x60_0000), None);
}
