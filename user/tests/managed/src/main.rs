//! A program started the way the process manager starts programs, for
//! the startup transfer tests. Its exit code reports the state it
//! received.

#![no_std]
#![no_main]

use mikros_rt::Managed;

mikros_rt::managed_main!(main);

/// Exits with 100, plus 10 for each startup handle, plus one for each
/// namespace entry, when the header holds the values the tests send,
/// and with 1 otherwise.
fn main(m: &Managed) -> i64 {
    let h = &m.header;
    let expected = h.pid == 7
        && h.parent == 1
        && h.app_id == "test.managed"
        && h.bundle.is_none()
        && h.program_phnum == 3;
    if !expected {
        return 1;
    }
    let ids_in_order = m
        .handles
        .iter()
        .enumerate()
        .all(|(i, sh)| sh.id == i as u32);
    let paths = m.namespace.iter().all(|e| e.path.starts_with('/'));
    if !ids_in_order || !paths {
        return 1;
    }
    // Large blocks need the allocator to map memory into the process.
    let big = alloc::vec![7u8; 100_000];
    if big.iter().map(|&b| u64::from(b)).sum::<u64>() != 700_000 {
        return 1;
    }
    100 + 10 * m.handles.len() as i64 + m.namespace.len() as i64
}

extern crate alloc;
