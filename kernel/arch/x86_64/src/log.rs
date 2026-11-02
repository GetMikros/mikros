//! The kernel's log and its output.

use crate::serial;
use alloc::sync::Arc;
use kernel_core::log::{Log, Message, Record, Severity, Source, line};
use kernel_core::object::{Header, Object, ObjectType};
use kernel_core::sync::SpinLock;

/// 256 records of 288 bytes: 73728 bytes.
static LOG: Log<256> = Log::new();

/// Appends a kernel record and writes out every record not yet written.
pub fn write(severity: Severity, args: core::fmt::Arguments) {
    let mut message = Message::default();
    let _ = core::fmt::Write::write_fmt(&mut message, args);
    append(0, severity, Source::Kernel, message.as_bytes());
}

/// Appends a record written by the process with ID `koid` and writes out
/// every record not yet written.
pub fn write_user(koid: u64, severity: Severity, message: &[u8]) {
    append(koid, severity, Source::User, message);
}

fn append(koid: u64, severity: Severity, source: Source, message: &[u8]) {
    LOG.write(crate::time::now(), koid, severity, source, message);
    LOG.drain(|record| serial::write(line(record, b"\r\n").as_bytes()));
}

/// The record with the smallest sequence above `after` still in the log.
pub fn next_after(after: u64) -> Option<Record> {
    LOG.next_after(after)
}

/// The `Log` object, through which user space reaches the log.
pub struct LogObject {
    header: Header,
}

impl Object for LogObject {
    fn header(&self) -> &Header {
        &self.header
    }
}

static OBJECT: SpinLock<Option<Arc<LogObject>>> = SpinLock::new(None);

/// The `Log` object, created on the first call. No other is created.
#[expect(dead_code, reason = "init is not started yet")]
pub fn object() -> Arc<LogObject> {
    OBJECT
        .lock()
        .get_or_insert_with(|| {
            Arc::new(LogObject {
                header: Header::new(ObjectType::Log),
            })
        })
        .clone()
}

/// Logs a formatted kernel message with the given severity.
macro_rules! log {
    ($severity:ident, $($arg:tt)*) => {
        $crate::log::write(
            kernel_core::log::Severity::$severity,
            format_args!($($arg)*),
        )
    };
}

pub(crate) use log;
