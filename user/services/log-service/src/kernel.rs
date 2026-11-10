//! The kernel log as a source: every record that the kernel log holds or
//! gets, which carries the app ID `kernel` and the pid 0, except the
//! log service's own, among them its copies of the records of clients.

use crate::log::Log;
use alloc::rc::Rc;
use alloc::string::String;
use mikros_rt::exec::Reactor;
use mikros_rt::sys;

/// The app ID of the kernel log's records.
const KERNEL: &str = "kernel";
/// The pid of the kernel log's records, which name no process of the
/// process manager.
const NO_PID: u32 = 0;
/// A kernel log record as `log_reader_read` copies it out: its
/// timestamp, severity, message length and message at these offsets.
const RECORD_SIZE: usize = 288;
const TIMESTAMP: usize = 0x08;
const PROCESS_KOID: usize = 0x10;
const SEVERITY: usize = 0x18;
const LENGTH: usize = 0x1A;
const MESSAGE: usize = 0x20;
const MESSAGE_MAX: usize = 256;
const SHOULD_WAIT: sys::Status = -22;
const READABLE: u32 = 1 << 0;

/// Adds the records of the kernel log behind `handle` to `log`, from the
/// oldest the kernel holds, and then each as it comes, but for those of
/// the process with the koid `own`.
pub async fn read(log: Rc<Log>, reactor: Reactor, handle: sys::Handle, own: u64) {
    let mut reader = 0;
    // SAFETY: the out pointer is valid for the call.
    let status = unsafe { sys::log_reader_create(handle, 0, &mut reader) };
    if status != mikros_rt::OK {
        mikros_rt::log!(Error, "log: reading the kernel log failed: {status}");
        return;
    }
    let mut record = [0u8; RECORD_SIZE];
    loop {
        // SAFETY: the record holds the bytes the call writes.
        let status = unsafe { sys::log_reader_read(reader, record.as_mut_ptr()) };
        if status == SHOULD_WAIT {
            if reactor.signals(reader, READABLE).await.is_err() {
                return;
            }
            continue;
        }
        if status != mikros_rt::OK {
            mikros_rt::log!(Error, "log: reading the kernel log failed: {status}");
            return;
        }
        let at = |offset: usize| record[offset..offset + 8].try_into().expect("8 bytes");
        if u64::from_le_bytes(at(PROCESS_KOID)) == own {
            continue;
        }
        let timestamp = i64::from_le_bytes(at(TIMESTAMP));
        let len = usize::from(u16::from_le_bytes([record[LENGTH], record[LENGTH + 1]]));
        let message = &record[MESSAGE..MESSAGE + len.min(MESSAGE_MAX)];
        let message = String::from_utf8_lossy(message).into_owned();
        log.add(timestamp, NO_PID, KERNEL.into(), record[SEVERITY], message);
    }
}
