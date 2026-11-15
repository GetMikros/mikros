use super::*;

const P: u64 = PAGE_SIZE;

fn range(page: u64, pages: u64, kind: Kind) -> MemoryMapEntry {
    MemoryMapEntry {
        phys_start: page * P,
        page_count: pages,
        kind: kind as u32,
        reserved: 0,
    }
}

/// Boot image memory at pages 16 to 31, split in two ranges, and
/// bootloader memory at pages 4 to 7.
fn map() -> [MemoryMapEntry; 3] {
    [
        range(24, 8, Kind::BootImage),
        range(4, 4, Kind::BootloaderReclaimable),
        range(16, 8, Kind::BootImage),
    ]
}

/// Paths are stored from page 5 on, one per 64 bytes.
const PATHS: [&[u8]; 4] = [b"init", b"bin/processmgr", b"lib/x.yaml", b"empty"];
const ARRAY: u64 = 4 * P;

fn module(i: usize, phys: u64, size: u64) -> Module {
    Module {
        phys,
        size,
        path_phys: 5 * P + 64 * i as u64,
        path_len: PATHS[i].len() as u32,
    }
}

fn modules() -> Vec<Module> {
    vec![
        module(0, 16 * P, 3 * P + 1),
        module(1, 20 * P, 5 * P),
        module(2, 30 * P, 10),
        module(3, 0, 0),
    ]
}

fn path(m: &Module) -> &'static [u8] {
    PATHS[((m.path_phys - 5 * P) / 64) as usize]
}

fn checked(modules: &[Module]) -> Result<(), Error> {
    check_array(ARRAY, modules.len() as u64, map().into_iter())?;
    check(modules, map().into_iter(), path)
}

#[test]
fn decodes_entries() {
    let mut bytes = vec![0u8; 2 * ENTRY_SIZE];
    bytes[0..8].copy_from_slice(&0x1000u64.to_le_bytes());
    bytes[8..16].copy_from_slice(&5u64.to_le_bytes());
    bytes[16..24].copy_from_slice(&0x2000u64.to_le_bytes());
    bytes[24..28].copy_from_slice(&4u32.to_le_bytes());
    bytes[28..32].fill(0xff);
    let entries = decode(&bytes);
    assert_eq!(entries.len(), 2);
    assert_eq!(
        entries[0],
        Module {
            phys: 0x1000,
            size: 5,
            path_phys: 0x2000,
            path_len: 4
        }
    );
}

#[test]
fn finds_ranges_across_entries() {
    assert!(covered(map().into_iter(), Kind::BootImage, 16 * P, 16 * P));
    assert!(!covered(
        map().into_iter(),
        Kind::BootImage,
        16 * P,
        16 * P + 1
    ));
    assert!(!covered(map().into_iter(), Kind::BootImage, 15 * P, 2));
    assert!(covered(map().into_iter(), Kind::BootImage, 0, 0));
    assert!(!covered(map().into_iter(), Kind::BootImage, u64::MAX, 2));
}

#[test]
fn accepts_a_valid_array() {
    assert_eq!(checked(&modules()), Ok(()));
}

#[test]
fn checks_the_count_and_the_array() {
    assert_eq!(checked(&[]), Err(Error::Count(0)));
    let many = vec![module(3, 0, 0); MODULES_MAX as usize + 1];
    assert_eq!(checked(&many), Err(Error::Count(MODULES_MAX + 1)));
    assert_eq!(
        check_array(8 * P - 64, 4, map().into_iter()),
        Err(Error::Array)
    );
}

#[test]
fn checks_contents() {
    let mut m = modules();
    m[3].phys = 16 * P;
    assert_eq!(checked(&m), Err(Error::EmptyWithAddress(3)));
    let mut m = modules();
    m[1].phys += 8;
    assert_eq!(checked(&m), Err(Error::Unaligned(1)));
    let mut m = modules();
    m[2].size = 2 * P + 1;
    assert_eq!(checked(&m), Err(Error::NotBootImage(2)));
    let mut m = modules();
    m[0].phys = 12 * P;
    assert_eq!(checked(&m), Err(Error::NotBootImage(0)));
    let mut m = modules();
    m[2].phys = 19 * P;
    assert_eq!(checked(&m), Err(Error::Overlap(0, 2)));
    let mut m = modules();
    m[1].size = u64::MAX;
    assert_eq!(checked(&m), Err(Error::NotBootImage(1)));
}

#[test]
fn checks_paths() {
    let mut m = modules();
    m[1].path_phys = 8 * P - 4;
    assert_eq!(checked(&m), Err(Error::PathMemory(1)));
    let mut m = modules();
    m[2].path_len = 0;
    assert_eq!(checked(&m), Err(Error::Path(2)));
    let mut m = modules();
    m[3].path_phys = m[0].path_phys;
    assert_eq!(checked(&m), Err(Error::SamePath(0, 3)));
    for bad in [&b"/init"[..], b"a//b", b"./a", b"a/..", b"a/", b"\xff"] {
        assert!(!valid_path(bad), "{bad:?}");
    }
    assert!(valid_path(&[b'a'; 1024]));
    assert!(!valid_path(&[b'a'; 1025]));
    assert!(valid_path(b"lib/.hidden/a..b"));
}

#[test]
fn lays_out_the_index_and_the_contents() {
    let sizes = [3 * P + 1, 5 * P, 0, 10];
    let l = layout(&sizes, &PATHS);
    assert_eq!(l.index.len() as u64, P);
    assert_eq!(&l.index[..8], b"MIKROSBM");
    assert_eq!(l.index[8..16], [1, 0, 0, 0, 4, 0, 0, 0]);
    assert_eq!(l.offsets, [P, 5 * P, 0, 10 * P]);
    assert_eq!(l.size, 11 * P);
    let entry = |i: usize, at: usize| {
        let e = 16 + 24 * i + at;
        u64::from_le_bytes(l.index[e..e + 8].try_into().unwrap())
    };
    assert_eq!(entry(1, 0), 5 * P);
    assert_eq!(entry(1, 8), 5 * P);
    assert_eq!(entry(2, 0), 0);
    let path = |i: usize| {
        let e = 16 + 24 * i + 16;
        let at = u32::from_le_bytes(l.index[e..e + 4].try_into().unwrap()) as usize;
        let len = u32::from_le_bytes(l.index[e + 4..e + 8].try_into().unwrap()) as usize;
        &l.index[at..at + len]
    };
    assert_eq!(path(0), b"init");
    assert_eq!(path(3), b"empty");
    let end = 16 + 24 * 4 + PATHS.iter().map(|p| p.len()).sum::<usize>();
    assert!(l.index[end..].iter().all(|&b| b == 0));
}

#[test]
fn gives_the_index_as_many_pages_as_it_needs() {
    let path = [b'p'; 1000];
    let paths = vec![&path[..]; 5];
    let l = layout(&[1; 5], &paths);
    assert_eq!(l.index.len() as u64, 2 * P);
    assert_eq!(l.offsets[0], 2 * P);
    assert_eq!(l.size, 7 * P);
}
