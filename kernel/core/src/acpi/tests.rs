use super::*;
use std::vec;
use std::vec::Vec;

/// Memory made of separate regions, each at a physical address.
struct Memory(Vec<(u64, Vec<u8>)>);

impl PhysRead for Memory {
    fn read(&self, phys: u64, len: usize) -> Option<&[u8]> {
        self.0.iter().find_map(|(start, bytes)| {
            let at = usize::try_from(phys.checked_sub(*start)?).ok()?;
            bytes.get(at..at.checked_add(len)?)
        })
    }
}

fn checksum(bytes: &mut [u8], at: usize) {
    bytes[at] = 0;
    let sum = bytes.iter().fold(0u8, |s, &b| s.wrapping_add(b));
    bytes[at] = 0u8.wrapping_sub(sum);
}

fn table(signature: &[u8; 4], body: &[u8]) -> Vec<u8> {
    let mut t = vec![0; HEADER_SIZE];
    t[..4].copy_from_slice(signature);
    t.extend_from_slice(body);
    let len = t.len() as u32;
    t[4..8].copy_from_slice(&len.to_le_bytes());
    checksum(&mut t, 9);
    t
}

fn rsdp(xsdt: u64) -> Vec<u8> {
    let mut r = vec![0; RSDP_SIZE];
    r[..8].copy_from_slice(b"RSD PTR ");
    r[15] = 2;
    r[20..24].copy_from_slice(&36u32.to_le_bytes());
    r[24..32].copy_from_slice(&xsdt.to_le_bytes());
    checksum(&mut r[..20], 8);
    checksum(&mut r, 32);
    r
}

fn fadt(pm_port: u32, wide: bool, extended: Option<(u8, u64)>) -> Vec<u8> {
    let mut body = vec![0; 276 - HEADER_SIZE];
    let at = |offset: usize| offset - HEADER_SIZE;
    body[at(76)..at(80)].copy_from_slice(&pm_port.to_le_bytes());
    body[at(91)] = 4;
    if wide {
        body[at(112)..at(116)].copy_from_slice(&(1u32 << 8).to_le_bytes());
    }
    if let Some((space, address)) = extended {
        body[at(208)] = space;
        body[at(212)..at(220)].copy_from_slice(&address.to_le_bytes());
    }
    table(b"FACP", &body)
}

fn hpet(base: u64) -> Vec<u8> {
    let mut body = vec![0; 56 - HEADER_SIZE];
    body[44 - HEADER_SIZE..52 - HEADER_SIZE].copy_from_slice(&base.to_le_bytes());
    table(b"HPET", &body)
}

fn system(tables: &[(u64, Vec<u8>)]) -> Memory {
    let entries: Vec<u8> = tables.iter().flat_map(|(at, _)| at.to_le_bytes()).collect();
    let mut regions = vec![(0x1000, rsdp(0x2000)), (0x2000, table(b"XSDT", &entries))];
    regions.extend(tables.iter().cloned());
    Memory(regions)
}

#[test]
fn finds_listed_tables() {
    let mem = system(&[
        (0x3000, fadt(0x608, true, None)),
        (0x4000, hpet(0xfed0_0000)),
    ]);
    let tables = Tables::new(&mem, 0x1000).unwrap();
    let f = tables.find(*b"FACP").unwrap().unwrap();
    assert_eq!(
        pm_timer(f),
        Some(PmTimer {
            address: Address::Io(0x608),
            wide: true
        })
    );
    let h = tables.find(*b"HPET").unwrap().unwrap();
    assert_eq!(hpet_base(h), Some(0xfed0_0000));
    assert_eq!(tables.find(*b"APIC").unwrap(), None);
}

#[test]
fn prefers_the_extended_pm_timer_address() {
    let f = fadt(0x608, false, Some((1, 0xb008)));
    assert_eq!(
        pm_timer(&f),
        Some(PmTimer {
            address: Address::Io(0xb008),
            wide: false
        })
    );
    assert_eq!(pm_timer(&fadt(0, false, None)), None);
}

#[test]
fn checks_checksums_and_lengths() {
    let mut bad = fadt(0x608, true, None);
    bad[100] ^= 1;
    let mem = system(&[(0x3000, bad)]);
    let tables = Tables::new(&mem, 0x1000).unwrap();
    assert_eq!(tables.find(*b"FACP"), Err(Error::BadTable(*b"FACP")));

    let mut short = table(b"HPET", &[]);
    short[4..8].copy_from_slice(&400u32.to_le_bytes());
    let mem = system(&[(0x3000, short)]);
    let tables = Tables::new(&mem, 0x1000).unwrap();
    assert_eq!(tables.find(*b"HPET"), Err(Error::Unreadable(0x3000)));

    let mut mem = system(&[]);
    mem.0[0].1[16] ^= 1;
    assert!(matches!(Tables::new(&mem, 0x1000), Err(Error::BadRsdp)));
    assert!(matches!(
        Tables::new(&mem, 0x9000),
        Err(Error::Unreadable(0x9000))
    ));
}
