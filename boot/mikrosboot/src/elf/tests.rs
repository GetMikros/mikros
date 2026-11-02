use super::*;
use alloc::vec;

const START: u64 = boot_protocol::x86_64::KERNEL_RANGE_START;
const R: u32 = 4;

#[derive(Clone, Copy)]
struct Ph {
    kind: u32,
    flags: u32,
    offset: u64,
    vaddr: u64,
    file_size: u64,
    mem_size: u64,
    align: u64,
}

fn load(flags: u32, offset: u64, vaddr: u64, file_size: u64, mem_size: u64) -> Ph {
    Ph {
        kind: PT_LOAD,
        flags,
        offset,
        vaddr,
        file_size,
        mem_size,
        align: 4096,
    }
}

fn note(major: u16, minor: u16) -> Vec<u8> {
    let mut n = Vec::new();
    n.extend_from_slice(&7u32.to_le_bytes());
    n.extend_from_slice(&4u32.to_le_bytes());
    n.extend_from_slice(&NOTE_TYPE.to_le_bytes());
    n.extend_from_slice(b"Mikros\0\0");
    n.extend_from_slice(&major.to_le_bytes());
    n.extend_from_slice(&minor.to_le_bytes());
    n
}

const NOTE_AT: u64 = 0x400;

fn note_ph(len: usize) -> Ph {
    Ph {
        kind: PT_NOTE,
        flags: R,
        offset: NOTE_AT,
        vaddr: 0,
        file_size: len as u64,
        mem_size: 0,
        align: 4,
    }
}

/// An ELF file of 0x3000 bytes with `notes` at 0x400.
fn elf(entry: u64, phs: &[Ph], notes: &[u8]) -> Vec<u8> {
    let mut f = vec![0u8; 0x3000];
    f[..7].copy_from_slice(&[0x7f, b'E', b'L', b'F', 2, 1, 1]);
    f[16..18].copy_from_slice(&ET_EXEC.to_le_bytes());
    f[18..20].copy_from_slice(&EM_X86_64.to_le_bytes());
    f[20..24].copy_from_slice(&1u32.to_le_bytes());
    f[24..32].copy_from_slice(&entry.to_le_bytes());
    f[32..40].copy_from_slice(&64u64.to_le_bytes());
    f[54..56].copy_from_slice(&56u16.to_le_bytes());
    f[56..58].copy_from_slice(&(phs.len() as u16).to_le_bytes());
    for (i, p) in phs.iter().enumerate() {
        let h = &mut f[64 + 56 * i..][..56];
        h[0..4].copy_from_slice(&p.kind.to_le_bytes());
        h[4..8].copy_from_slice(&p.flags.to_le_bytes());
        h[8..16].copy_from_slice(&p.offset.to_le_bytes());
        h[16..24].copy_from_slice(&p.vaddr.to_le_bytes());
        h[32..40].copy_from_slice(&p.file_size.to_le_bytes());
        h[40..48].copy_from_slice(&p.mem_size.to_le_bytes());
        h[48..56].copy_from_slice(&p.align.to_le_bytes());
    }
    f[NOTE_AT as usize..][..notes.len()].copy_from_slice(notes);
    f
}

fn text() -> Ph {
    load(R | PF_X, 0x1000, START, 0x1000, 0x1000)
}

fn data() -> Ph {
    load(R | PF_W, 0x2010, START + 0x1010, 0x100, 0x2000)
}

fn kernel(phs: &[Ph]) -> Vec<u8> {
    let n = note(1, 0);
    let mut all = phs.to_vec();
    all.push(note_ph(n.len()));
    elf(START + 0x10, &all, &n)
}

#[test]
fn accepts_a_valid_kernel() {
    let k = check(&kernel(&[text(), data()]), START).unwrap();
    assert_eq!(k.entry, START + 0x10);
    assert_eq!(k.version, (1, 0));
    assert_eq!(k.segments.len(), 2);
    assert_eq!(
        k.segments[1].pages(),
        (u128::from(START) + 0x1000, u128::from(START) + 0x4000)
    );
}

#[test]
fn checks_the_header() {
    let good = kernel(&[text()]);
    for (at, value) in [(4, 1u8), (5, 2), (6, 0), (16, 3), (18, 183), (20, 2)] {
        let mut bad = good.clone();
        bad[at] = value;
        assert_eq!(check(&bad, START), Err(Error::BadHeader), "byte {at}");
    }
    assert_eq!(check(&good[..40], START), Err(Error::Truncated));
}

#[test]
fn refuses_forbidden_and_missing_segments() {
    for kind in [PT_INTERP, PT_DYNAMIC, PT_TLS] {
        let mut p = text();
        p.kind = kind;
        assert_eq!(
            check(&kernel(&[text(), p]), START),
            Err(Error::ForbiddenSegment(kind))
        );
    }
    assert_eq!(check(&kernel(&[]), START), Err(Error::NoLoadSegment));
}

#[test]
fn refuses_bad_load_segments() {
    let mut big = data();
    big.file_size = 0x3000;
    assert_eq!(
        check(&kernel(&[text(), big]), START),
        Err(Error::BadSegment(1))
    );
    let outside = load(R, 0x2fff, START + 0x1fff, 2, 2);
    assert_eq!(
        check(&kernel(&[text(), outside]), START),
        Err(Error::BadSegment(1))
    );
    let misaligned = load(R, 0x2000, START + 0x1010, 0x10, 0x10);
    assert_eq!(
        check(&kernel(&[text(), misaligned]), START),
        Err(Error::Misaligned(1))
    );
    let low = load(R, 0x2000, START - 0x1000, 0x10, 0x10);
    assert_eq!(
        check(&kernel(&[text(), low]), START),
        Err(Error::OutsideKernelRange(1))
    );
    let past_top = load(R, 0x2000, u64::MAX - 0xfff, 0, 0x2000);
    assert_eq!(
        check(&kernel(&[text(), past_top]), START),
        Err(Error::OutsideKernelRange(1))
    );
    let at_top = load(R, 0x2000, u64::MAX - 0xfff, 0, 0x1000);
    assert!(
        check(&kernel(&[text(), at_top]), START).is_ok(),
        "ends exactly at the top"
    );
}

#[test]
fn refuses_overlaps_wx_and_bad_entries() {
    let same_page = load(R, 0x1800, START + 0x800, 0x10, 0x10);
    assert_eq!(
        check(&kernel(&[text(), same_page]), START),
        Err(Error::Overlap(0, 1))
    );
    let wx = load(R | PF_W | PF_X, 0x2000, START + 0x2000, 0x10, 0x10);
    assert_eq!(
        check(&kernel(&[text(), wx]), START),
        Err(Error::WritableAndExecutable(1))
    );
    let n = note(1, 0);
    let in_data = elf(START + 0x1010, &[text(), data(), note_ph(n.len())], &n);
    assert_eq!(check(&in_data, START), Err(Error::EntryNotExecutable));
}

#[test]
fn checks_the_version_note() {
    let none = elf(START, &[text()], &[]);
    assert_eq!(check(&none, START), Err(Error::NoVersionNote));
    let n = note(2, 3);
    let newer = elf(START, &[text(), note_ph(n.len())], &n);
    assert_eq!(check(&newer, START), Err(Error::WrongVersion(2, 3)));
    let n = note(1, 7);
    let minor = elf(START, &[text(), note_ph(n.len())], &n);
    assert_eq!(check(&minor, START).unwrap().version, (1, 7));
    let mut two = note(1, 0);
    two.extend(note(1, 0));
    let twice = elf(START, &[text(), note_ph(two.len())], &two);
    assert_eq!(check(&twice, START), Err(Error::SeveralVersionNotes));
}
