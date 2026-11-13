//! Exception reports read from an exception channel, and the report ID
//! of the latest one for each thread, which resuming the thread needs.

use crate::{Result, ensure, read_message, u64_at};
use core::sync::atomic::{AtomicU64, Ordering};
use mikros_rt::sys::Handle;

/// The size of an exception report.
pub const REPORT_SIZE: usize = 0x30;

/// The report ID of the latest report read for each thread handle, as
/// pairs of handle and ID.
static REPORT_IDS: [(AtomicU64, AtomicU64); 32] =
    [const { (AtomicU64::new(0), AtomicU64::new(0)) }; 32];

/// Keeps the report ID of a report that came with `thread`.
pub fn record_report(thread: Handle, bytes: &[u8]) {
    let id = u64_at(bytes, 0x28);
    let slot = REPORT_IDS
        .iter()
        .find(|(h, _)| h.load(Ordering::Relaxed) == u64::from(thread))
        .or_else(|| {
            REPORT_IDS
                .iter()
                .min_by_key(|(_, id)| id.load(Ordering::Relaxed))
        });
    if let Some((h, i)) = slot {
        h.store(u64::from(thread), Ordering::Relaxed);
        i.store(id, Ordering::Relaxed);
    }
}

/// The report ID of the latest report read with `thread`, or 0.
pub fn report_id(thread: Handle) -> u64 {
    REPORT_IDS
        .iter()
        .find(|(h, _)| h.load(Ordering::Relaxed) == u64::from(thread))
        .map_or(0, |(_, id)| id.load(Ordering::Relaxed))
}

pub const PAGE_FAULT: u32 = 2;

/// The bytes of the next report on exception channel `channel` and the
/// handle of its thread.
pub fn report_bytes(channel: Handle) -> Result<([u8; 0x30], Handle)> {
    let m = read_message(channel, 2000)?;
    ensure("receiving a report", m.len == REPORT_SIZE && m.count == 1)?;
    record_report(m.handles[0], &m.bytes);
    let mut bytes = [0; 0x30];
    bytes.copy_from_slice(&m.bytes[..0x30]);
    Ok((bytes, m.handles[0]))
}
